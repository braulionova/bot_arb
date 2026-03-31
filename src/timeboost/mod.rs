use alloy::primitives::{address, Address, Bytes, FixedBytes, U256, keccak256};
use alloy::signers::local::PrivateKeySigner;
use alloy::signers::Signer;
use alloy::sol;
use eyre::Result;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use tracing::{error, info, warn};

// ─── Timeboost Constants (Arbitrum One Mainnet — LIVE since April 2025) ───

pub const EXPRESS_LANE_AUCTION: Address = address!("5fcb496a31b7ae91e7c9078ec662bd7a55cd3079");
pub const WETH: Address = address!("82aF49447D8a07e3bd95BD0d56f35241523fBab1");
pub const AUCTIONEER_URL: &str = "https://arb1-auctioneer.arbitrum.io/rpc";
pub const SEQUENCER_URL: &str = "https://arb1-sequencer.arbitrum.io/rpc";
pub const CHAIN_ID: u64 = 42161;
pub const ROUND_DURATION_SECS: u64 = 60;
pub const OFFSET_TIMESTAMP: u64 = 1741687071;

// ─── Contract ABI ───

sol! {
    #[sol(rpc)]
    interface IExpressLaneAuction {
        function currentRound() external view returns (uint64);
        function domainSeparator() external view returns (bytes32);
        function reservePrice() external view returns (uint256);
        function balanceOf(address account) external view returns (uint256);
        function deposit(uint256 amount) external;
    }
}

/// Manages Timeboost express lane: auction bidding + priority tx submission.
///
/// When we win the auction (costs ~0.001 WETH/round), our txs get
/// 200ms priority over normal txs = first in block = guaranteed backrun.
pub struct TimeboostManager {
    signer: PrivateKeySigner,
    controller: Address,
    client: reqwest::Client,
    sequence_number: Arc<AtomicU64>,
    domain_separator: FixedBytes<32>,
    current_round: AtomicU64,
    max_bid_wei: U256,
    /// Set by executor when a profitable arb is detected
    arb_signal: std::sync::atomic::AtomicBool,
}

impl TimeboostManager {
    pub async fn new<P: alloy::providers::Provider + Clone>(
        signer: PrivateKeySigner,
        max_bid_wei: U256,
        provider: &P,
    ) -> Result<Self> {
        let controller = signer.address();
        let auction = IExpressLaneAuction::new(EXPRESS_LANE_AUCTION, provider);
        let domain_sep = auction.domainSeparator().call().await?;
        let current = auction.currentRound().call().await?;
        let reserve = auction.reservePrice().call().await?;

        info!(
            controller = %controller,
            round = current,
            reserve = %reserve,
            max_bid = %max_bid_wei,
            "Timeboost initialized"
        );

        Ok(Self {
            signer,
            controller,
            client: reqwest::Client::new(),
            sequence_number: Arc::new(AtomicU64::new(0)),
            domain_separator: domain_sep,
            current_round: AtomicU64::new(current),
            max_bid_wei,
            arb_signal: std::sync::atomic::AtomicBool::new(false),
        })
    }

    /// Submit sealed bid to auctioneer (off-chain RPC, NOT on-chain).
    /// EIP-712: Bid(uint64 round, address expressLaneController, uint256 amount)
    pub async fn submit_bid(&self, round: u64, amount: U256) -> Result<()> {
        let bid_type_hash = keccak256("Bid(uint64 round,address expressLaneController,uint256 amount)");

        let mut struct_data = Vec::with_capacity(96);
        struct_data.extend_from_slice(&bid_type_hash.0);
        // round (uint64 padded to 32 bytes)
        let mut round_buf = [0u8; 32];
        round_buf[24..].copy_from_slice(&round.to_be_bytes());
        struct_data.extend_from_slice(&round_buf);
        // expressLaneController (address padded to 32 bytes)
        let mut addr_buf = [0u8; 32];
        addr_buf[12..].copy_from_slice(self.controller.as_slice());
        struct_data.extend_from_slice(&addr_buf);
        // amount (uint256)
        struct_data.extend_from_slice(&amount.to_be_bytes::<32>());

        let struct_hash = keccak256(&struct_data);

        // EIP-712 digest: \x19\x01 || domainSeparator || structHash
        let mut digest_data = Vec::with_capacity(66);
        digest_data.extend_from_slice(&[0x19, 0x01]);
        digest_data.extend_from_slice(&self.domain_separator.0);
        digest_data.extend_from_slice(&struct_hash.0);
        let digest = keccak256(&digest_data);

        let sig = self.signer.sign_hash(&digest.into()).await?;
        let mut sig_bytes = [0u8; 65];
        sig_bytes[..32].copy_from_slice(&sig.r().to_be_bytes::<32>());
        sig_bytes[32..64].copy_from_slice(&sig.s().to_be_bytes::<32>());
        sig_bytes[64] = if sig.v() { 28 } else { 27 };

        let body = serde_json::json!({
            "jsonrpc": "2.0",
            "method": "auctioneer_submitBid",
            "params": [{
                "chainId": format!("0x{:x}", CHAIN_ID),
                "expressLaneController": format!("{:?}", self.controller),
                "auctionContractAddress": format!("{:?}", EXPRESS_LANE_AUCTION),
                "round": format!("0x{:x}", round),
                "amount": format!("0x{:x}", amount),
                "signature": format!("0x{}", hex::encode(sig_bytes)),
            }],
            "id": 1
        });

        let resp = self.client.post(AUCTIONEER_URL).json(&body).send().await?;
        let text = resp.text().await.unwrap_or_default();

        if text.contains("error") {
            warn!(round, resp = %text, "Bid rejected");
        } else {
            info!(round, amount = %amount, "Bid accepted");
        }

        Ok(())
    }

    /// Send tx via express lane (200ms priority over normal txs).
    pub async fn send_express_lane_tx(&self, signed_tx_rlp: &Bytes) -> Result<()> {
        let round = self.current_round.load(Ordering::Relaxed);
        let seq = self.sequence_number.fetch_add(1, Ordering::SeqCst);

        // Message: keccak256("TIMEBOOST_BID") || pad(chainId) || auction || round || seq || rlpTx
        let domain = keccak256("TIMEBOOST_BID");
        let mut msg = Vec::with_capacity(100 + signed_tx_rlp.len());
        msg.extend_from_slice(&domain.0);
        let mut chain_buf = [0u8; 32];
        chain_buf[24..].copy_from_slice(&CHAIN_ID.to_be_bytes());
        msg.extend_from_slice(&chain_buf);
        msg.extend_from_slice(EXPRESS_LANE_AUCTION.as_slice());
        msg.extend_from_slice(&round.to_be_bytes());
        msg.extend_from_slice(&seq.to_be_bytes());
        msg.extend_from_slice(signed_tx_rlp);

        // Personal sign
        let msg_hash = keccak256(&msg);
        let prefix = format!("\x19Ethereum Signed Message:\n{}", msg_hash.len());
        let prefixed = keccak256(&[prefix.as_bytes(), &msg_hash.0].concat());

        let sig = self.signer.sign_hash(&prefixed.into()).await?;
        let mut sig_bytes = [0u8; 65];
        sig_bytes[..32].copy_from_slice(&sig.r().to_be_bytes::<32>());
        sig_bytes[32..64].copy_from_slice(&sig.s().to_be_bytes::<32>());
        sig_bytes[64] = if sig.v() { 28 } else { 27 };

        let body = serde_json::json!({
            "jsonrpc": "2.0",
            "method": "timeboost_sendExpressLaneTransaction",
            "params": [{
                "chainId": format!("0x{:x}", CHAIN_ID),
                "round": format!("0x{:x}", round),
                "auctionContractAddress": format!("{:?}", EXPRESS_LANE_AUCTION),
                "transaction": format!("0x{}", hex::encode(signed_tx_rlp.as_ref())),
                "sequenceNumber": format!("0x{:x}", seq),
                "signature": format!("0x{}", hex::encode(sig_bytes)),
                "options": null,
            }],
            "id": 1
        });

        let resp = self.client.post(SEQUENCER_URL).json(&body).send().await?;
        let text = resp.text().await.unwrap_or_default();

        if text.contains("error") {
            warn!(round, seq, resp = %text, "Express lane tx rejected");
        } else {
            info!(round, seq, "Express lane tx sent");
        }

        Ok(())
    }

    /// Smart auction loop — bids only when recent arb activity detected.
    /// Tracks arb_signal: when the bot sees profitable arbs, it sets the signal.
    /// We bid for the next few rounds to have express lane ready.
    pub async fn start_auction_loop<P: alloy::providers::Provider + Clone + 'static>(
        self: Arc<Self>,
        provider: P,
    ) {
        info!("Timeboost smart auction loop started");

        let mut rounds_since_arb: u64 = 999; // start without bidding

        loop {
            let auction = IExpressLaneAuction::new(EXPRESS_LANE_AUCTION, &provider);

            let round = match auction.currentRound().call().await {
                Ok(r) => r,
                Err(e) => {
                    warn!(error = %e, "Failed to get round");
                    tokio::time::sleep(std::time::Duration::from_secs(10)).await;
                    continue;
                }
            };
            let next_round = round + 1;
            self.current_round.store(round, Ordering::Relaxed);
            self.sequence_number.store(0, Ordering::SeqCst);

            // Check if arb signal was set (profitable arb detected recently)
            let signal = self.arb_signal.swap(false, Ordering::Relaxed);
            if signal {
                rounds_since_arb = 0;
            } else {
                rounds_since_arb += 1;
            }

            // Bid for next 5 rounds after seeing an arb (5 min coverage)
            if rounds_since_arb < 5 {
                let bid_amount = self.max_bid_wei;
                info!(round = next_round, rounds_since_arb, "Bidding for express lane");
                if let Err(e) = self.submit_bid(next_round, bid_amount).await {
                    error!(error = %e, "Bid failed");
                }
            }

            // Sleep until next round
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_secs();
            let round_start = OFFSET_TIMESTAMP + next_round * ROUND_DURATION_SECS;
            let sleep = if round_start > now { round_start - now + 2 } else { ROUND_DURATION_SECS };

            tokio::time::sleep(std::time::Duration::from_secs(sleep)).await;
        }
    }

    /// Signal that a profitable arb was detected — triggers bidding for upcoming rounds
    pub fn signal_arb(&self) {
        self.arb_signal.store(true, Ordering::Relaxed);
    }
}
