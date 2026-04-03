use std::sync::Arc;
use tokio::sync::mpsc;
use tracing::{error, info, warn};

/// Telegram notification types
#[derive(Debug, Clone)]
pub enum TgMsg {
    /// Arb detected and sent to eth_call simulation
    ArbDetected {
        arb_num: u64,
        profit_eth: f64,
        spread_pct: f64,
        buy_dex: String,
        sell_dex: String,
        amount_eth: f64,
        is_3hop: bool,
    },
    /// Simulation passed, tx sent to sequencer
    TxSent {
        arb_num: u64,
        tx_hash: String,
        profit_eth: f64,
    },
    /// Arb confirmed on-chain
    ArbSuccess {
        tx_hash: String,
        profit_eth: f64,
    },
    /// Arb reverted on-chain (gas lost)
    ArbReverted {
        tx_hash: String,
        reason: String,
    },
    /// Simulation reverted (no gas spent)
    SimReverted {
        arb_num: u64,
        reason: String,
    },
    /// Periodic stats summary
    Stats {
        swaps: u64,
        arbs_found: u64,
        executed: u64,
        success: u64,
        pools: usize,
    },
}

/// Telegram notifier — batches messages to avoid rate limits
pub struct TgNotifier {
    bot_token: String,
    chat_id: String,
    client: reqwest::Client,
}

impl TgNotifier {
    pub fn new(bot_token: String, chat_id: String) -> Self {
        Self {
            bot_token,
            chat_id,
            client: reqwest::Client::new(),
        }
    }

    pub fn from_env() -> Option<Self> {
        let token = std::env::var("TELEGRAM_BOT_TOKEN").ok()?;
        let chat = std::env::var("TELEGRAM_CHAT_ID").ok()?;
        if token.is_empty() || chat.is_empty() {
            return None;
        }
        info!("Telegram notifications enabled");
        Some(Self::new(token, chat))
    }

    async fn send(&self, text: &str) {
        let url = format!(
            "https://api.telegram.org/bot{}/sendMessage",
            self.bot_token
        );
        let body = serde_json::json!({
            "chat_id": self.chat_id,
            "text": text,
            "parse_mode": "HTML",
            "disable_web_page_preview": true,
        });
        match self.client.post(&url).json(&body).send().await {
            Ok(resp) if !resp.status().is_success() => {
                warn!(status = %resp.status(), "Telegram send failed");
            }
            Err(e) => {
                warn!(error = %e, "Telegram send error");
            }
            _ => {}
        }
    }

    fn format_msg(msg: &TgMsg) -> String {
        match msg {
            TgMsg::ArbDetected {
                arb_num,
                profit_eth,
                spread_pct,
                buy_dex,
                sell_dex,
                amount_eth,
                is_3hop,
            } => {
                let hop = if *is_3hop { "3-HOP " } else { "" };
                format!(
                    "🔍 <b>{hop}ARB #{arb_num}</b>\n\
                     Spread: <b>{spread_pct:.3}%</b>\n\
                     Est profit: <b>{profit_eth:.6} ETH</b>\n\
                     Amount: {amount_eth:.4} ETH\n\
                     Route: {buy_dex} → {sell_dex}"
                )
            }
            TgMsg::TxSent {
                arb_num,
                tx_hash,
                profit_eth,
            } => {
                format!(
                    "📤 <b>TX SENT #{arb_num}</b>\n\
                     Est profit: <b>{profit_eth:.6} ETH</b>\n\
                     <a href=\"https://arbiscan.io/tx/{tx_hash}\">View on Arbiscan</a>"
                )
            }
            TgMsg::ArbSuccess { tx_hash, profit_eth } => {
                format!(
                    "✅ <b>ARB SUCCESS!</b>\n\
                     Profit: <b>{profit_eth:.6} ETH</b>\n\
                     <a href=\"https://arbiscan.io/tx/{tx_hash}\">View on Arbiscan</a>"
                )
            }
            TgMsg::ArbReverted { tx_hash, reason } => {
                format!(
                    "❌ <b>ARB REVERTED</b>\n\
                     Reason: {reason}\n\
                     <a href=\"https://arbiscan.io/tx/{tx_hash}\">View on Arbiscan</a>"
                )
            }
            TgMsg::SimReverted { arb_num, reason } => {
                let short = if reason.len() > 80 {
                    &reason[..80]
                } else {
                    reason
                };
                format!("⏭ Sim #{arb_num}: {short}")
            }
            TgMsg::Stats {
                swaps,
                arbs_found,
                executed,
                success,
                pools,
            } => {
                format!(
                    "📊 <b>Stats</b>\n\
                     Swaps: {swaps} | Arbs: {arbs_found}\n\
                     Executed: {executed} | Success: {success}\n\
                     Pools: {pools}"
                )
            }
        }
    }
}

/// Start the Telegram sender loop. Returns a channel to send messages.
pub fn start_tg_sender() -> Option<mpsc::UnboundedSender<TgMsg>> {
    let notifier = TgNotifier::from_env()?;
    let (tx, mut rx) = mpsc::unbounded_channel::<TgMsg>();

    tokio::spawn(async move {
        // Batch sim reverts — only send count every 60s
        let mut sim_revert_count = 0u64;
        let mut last_batch = tokio::time::Instant::now();

        while let Some(msg) = rx.recv().await {
            match &msg {
                TgMsg::SimReverted { .. } => {
                    sim_revert_count += 1;
                    // Batch: send summary every 60s if there were reverts
                    if last_batch.elapsed() > tokio::time::Duration::from_secs(60)
                        && sim_revert_count > 0
                    {
                        notifier
                            .send(&format!("⏭ {sim_revert_count} sim reverts in last 60s"))
                            .await;
                        sim_revert_count = 0;
                        last_batch = tokio::time::Instant::now();
                    }
                }
                _ => {
                    // Send immediately for important events
                    notifier.send(&TgNotifier::format_msg(&msg)).await;
                }
            }
        }
    });

    Some(tx)
}
