use alloy::network::EthereumWallet;
use alloy::primitives::Address;
use alloy::signers::local::PrivateKeySigner;
use eyre::{Result, WrapErr};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use tracing::info;

/// Manages wallet signing and nonce tracking for fast tx submission.
#[derive(Clone)]
pub struct WalletManager {
    pub wallet: EthereumWallet,
    pub address: Address,
    nonce: Arc<AtomicU64>,
}

impl WalletManager {
    /// Create from hex private key (with or without 0x prefix)
    pub fn from_private_key(key_hex: &str) -> Result<Self> {
        let key_hex = key_hex.strip_prefix("0x").unwrap_or(key_hex);
        let signer: PrivateKeySigner = key_hex
            .parse()
            .wrap_err("Failed to parse private key")?;

        let address = signer.address();
        let wallet = EthereumWallet::from(signer);

        info!(?address, "Wallet loaded");

        Ok(Self {
            wallet,
            address,
            nonce: Arc::new(AtomicU64::new(0)),
        })
    }

    /// Initialize nonce from on-chain state (with retry for rate-limited RPCs)
    pub async fn sync_nonce<P: alloy::providers::Provider>(&self, provider: &P) -> Result<()> {
        for attempt in 1..=5u32 {
            match provider.get_transaction_count(self.address).await {
                Ok(on_chain_nonce) => {
                    self.nonce.store(on_chain_nonce, Ordering::SeqCst);
                    info!(nonce = on_chain_nonce, "Nonce synced from chain");
                    return Ok(());
                }
                Err(e) if attempt < 5 => {
                    tracing::warn!(attempt, error = %e, "Nonce sync failed, retrying in 500ms...");
                    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
                }
                Err(e) => return Err(e.into()),
            }
        }
        unreachable!()
    }

    /// Get and increment nonce atomically (for concurrent tx submission)
    pub fn next_nonce(&self) -> u64 {
        self.nonce.fetch_add(1, Ordering::SeqCst)
    }

    /// Reset nonce (e.g., after a tx failure)
    pub fn reset_nonce(&self, nonce: u64) {
        self.nonce.store(nonce, Ordering::SeqCst);
    }
}
