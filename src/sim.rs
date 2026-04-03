use alloy::primitives::{Address, Bytes};
use eyre::Result;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tracing::{debug, info, warn};

/// Fast local simulator via raw eth_call to local Nitro node.
///
/// Uses raw reqwest HTTP instead of alloy Provider to eliminate middleware
/// overhead (GasFiller, NonceFiller, ChainIdFiller, BlobGasFiller are all
/// unnecessary for eth_call simulation).
///
/// When local node is still syncing, automatically routes sims to the fallback
/// RPC to avoid simulating against stale state. A background task flips the
/// `node_synced` flag once the node catches up.
///
/// Typical latency: ~1-3ms on localhost with network_mode: host.
pub struct LocalSim {
    bot_address: Address,
    /// Reusable HTTP client with connection pooling
    client: reqwest::Client,
    /// Primary RPC URL (local Nitro node)
    primary_url: Option<String>,
    /// Fallback RPC URL (Alchemy etc.)
    fallback_url: Option<String>,
    /// True once local node is fully synced (updated by background task)
    node_synced: Arc<AtomicBool>,
}

impl LocalSim {
    pub fn new(bot_address: Address) -> Self {
        let client = reqwest::Client::builder()
            .pool_max_idle_per_host(8)
            .pool_idle_timeout(std::time::Duration::from_secs(120))
            .tcp_keepalive(std::time::Duration::from_secs(30))
            .tcp_nodelay(true)
            .timeout(std::time::Duration::from_secs(3))
            .http2_keep_alive_interval(std::time::Duration::from_secs(15))
            .build()
            .unwrap();

        Self {
            bot_address,
            client,
            primary_url: None,
            fallback_url: None,
            node_synced: Arc::new(AtomicBool::new(false)),
        }
    }

    pub async fn init(&self, _rpc_url: &str) -> Result<()> {
        info!("Local simulator ready (raw eth_call mode)");
        Ok(())
    }

    /// Set the primary RPC URL (called once at startup)
    pub fn set_rpc(&mut self, url: &str) {
        self.primary_url = Some(url.to_string());
    }

    /// Set fallback RPC URL
    pub fn set_fallback(&mut self, url: &str) {
        self.fallback_url = Some(url.to_string());
    }

    /// Start background sync monitor. Call once after set_rpc().
    /// Checks eth_syncing every 10s and flips `node_synced` flag.
    pub fn start_sync_monitor(&self) {
        let url = match self.primary_url {
            Some(ref u) => u.clone(),
            None => return,
        };
        let client = self.client.clone();
        let synced = self.node_synced.clone();

        tokio::spawn(async move {
            loop {
                let body = serde_json::json!({
                    "jsonrpc": "2.0",
                    "method": "eth_syncing",
                    "params": [],
                    "id": 1
                });

                let is_synced = match client.post(&url).json(&body).send().await {
                    Ok(resp) => {
                        resp.text().await
                            .map(|t| t.contains("\"result\":false"))
                            .unwrap_or(false)
                    }
                    Err(_) => false,
                };

                let was_synced = synced.load(Ordering::Relaxed);
                synced.store(is_synced, Ordering::Relaxed);

                if is_synced && !was_synced {
                    info!("Local node SYNCED — switching sims to local node (lowest latency)");
                }
                if !is_synced && was_synced {
                    warn!("Local node lost sync — routing sims to fallback RPC");
                }

                // Check every 10s while syncing, every 30s once synced
                let delay = if is_synced { 30 } else { 10 };
                tokio::time::sleep(std::time::Duration::from_secs(delay)).await;
            }
        });
    }

    /// Simulate tx via raw eth_call. ~1-3ms on localhost when synced.
    /// Routes to fallback RPC when local node is still syncing.
    pub async fn simulate(&self, contract: Address, calldata: &Bytes) -> bool {
        // Build eth_call JSON-RPC payload once
        let from_hex = format!("{:?}", self.bot_address);
        let to_hex = format!("{:?}", contract);
        let data_hex = format!("0x{}", hex::encode(calldata.as_ref()));

        let body = serde_json::json!({
            "jsonrpc": "2.0",
            "method": "eth_call",
            "params": [{
                "from": from_hex,
                "to": to_hex,
                "data": data_hex
            }, "latest"],
            "id": 1
        });

        // If local node is synced, use it (fastest). Otherwise skip to fallback.
        if self.node_synced.load(Ordering::Relaxed) {
            if let Some(ref url) = self.primary_url {
                match self.raw_eth_call(url, &body).await {
                    SimResult::Success => {
                        debug!("Sim passed (local)");
                        return true;
                    }
                    SimResult::Revert(reason) => {
                        debug!(reason = %reason, "Sim REVERT (local)");
                        return false;
                    }
                    SimResult::TransportError(err) => {
                        debug!(error = %err, "Local sim transport error, trying fallback");
                    }
                }
            }
        }

        // Fallback: Alchemy or other reliable RPC
        if let Some(ref fb_url) = self.fallback_url {
            match self.raw_eth_call(fb_url, &body).await {
                SimResult::Success => {
                    debug!("Sim passed (fallback)");
                    true
                }
                SimResult::Revert(reason) => {
                    debug!(reason = %reason, "Sim REVERT (fallback)");
                    false
                }
                SimResult::TransportError(err) => {
                    debug!(error = %err, "Sim failed (fallback transport)");
                    false
                }
            }
        } else if self.primary_url.is_some() {
            // No fallback and node not synced — try local anyway (better than nothing)
            let url = self.primary_url.as_ref().unwrap();
            match self.raw_eth_call(url, &body).await {
                SimResult::Success => true,
                _ => false,
            }
        } else {
            warn!("No sim provider configured");
            true // pass through
        }
    }

    /// Raw eth_call via reqwest — no alloy middleware overhead
    async fn raw_eth_call(&self, url: &str, body: &serde_json::Value) -> SimResult {
        let resp = match self.client.post(url).json(body).send().await {
            Ok(r) => r,
            Err(e) => return SimResult::TransportError(e.to_string()),
        };

        let text = match resp.text().await {
            Ok(t) => t,
            Err(e) => return SimResult::TransportError(e.to_string()),
        };

        // Fast path: check for "result" key (success)
        if text.contains("\"result\"") && !text.contains("\"error\"") {
            return SimResult::Success;
        }

        // Check for revert — extract reason for debugging
        if text.contains("revert") || text.contains("execution reverted") {
            // Try to extract useful error message
            let reason = if let Ok(json) = serde_json::from_str::<serde_json::Value>(&text) {
                json.pointer("/error/message")
                    .and_then(|m| m.as_str())
                    .unwrap_or("execution reverted")
                    .to_string()
            } else {
                text[..text.len().min(200)].to_string()
            };
            return SimResult::Revert(reason);
        }

        // Other RPC errors (rate limit, internal error, etc.)
        SimResult::TransportError(text)
    }
}

enum SimResult {
    Success,
    Revert(String),
    TransportError(String),
}
