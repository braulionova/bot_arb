use alloy::primitives::{Address, Bytes};
use alloy::providers::{Provider, ProviderBuilder};
use alloy::rpc::types::TransactionRequest;
use eyre::Result;
use tracing::info;

/// Fast local simulator via rpc-cache proxy (~5ms per sim).
/// Sends eth_call to localhost rpc-cache which has in-memory caching.
pub struct LocalSim {
    bot_address: Address,
    rpc_url: String,
}

impl LocalSim {
    pub fn new(bot_address: Address) -> Self {
        Self {
            bot_address,
            rpc_url: String::new(),
        }
    }

    pub async fn init(&self, _rpc_url: &str) -> Result<()> {
        info!("Local simulator ready (rpc-cache)");
        Ok(())
    }

    /// Set the RPC URL (called once at startup)
    pub fn set_rpc(&mut self, url: &str) {
        self.rpc_url = url.to_string();
    }

    /// Simulate tx via local rpc-cache. ~5ms.
    pub async fn simulate(&self, contract: Address, calldata: &Bytes) -> bool {
        if self.rpc_url.is_empty() {
            return true;
        }

        let provider = ProviderBuilder::new()
            .connect_http(match self.rpc_url.parse() {
                Ok(u) => u,
                Err(_) => return true,
            });

        let sim_tx = TransactionRequest::default()
            .to(contract)
            .input(calldata.clone().into())
            .from(self.bot_address);

        match provider.call(sim_tx).await {
            Ok(_) => true,
            Err(_) => false,
        }
    }
}
