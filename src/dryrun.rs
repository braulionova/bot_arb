use alloy::primitives::{Address, U256};
use serde::Serialize;
use std::fs::OpenOptions;
use std::io::Write;
use std::sync::Mutex;
use tracing::info;

use crate::decoder::DexType;
use crate::pools::Pool;

/// Logs all arb opportunities to a JSONL file for analysis
pub struct DryRunLogger {
    file: Mutex<std::fs::File>,
    path: String,
    count: Mutex<u64>,
}

#[derive(Serialize)]
pub struct OpportunityRecord {
    pub timestamp: String,
    pub id: u64,
    // Swap that triggered the check
    pub trigger_token_in: String,
    pub trigger_token_out: String,
    pub trigger_dex: String,
    // Spread data
    pub buy_dex: String,
    pub sell_dex: String,
    pub buy_pool: String,
    pub sell_pool: String,
    pub price_buy: f64,
    pub price_sell: f64,
    pub spread_pct: f64,
    pub net_spread_pct: f64,
    pub buy_fee_bps: u32,
    pub sell_fee_bps: u32,
    // Simulation
    pub optimal_input: String,
    pub sim_bought: String,
    pub sim_sold: String,
    pub profit_gross: String,
    pub profit_gross_eth: f64,
    pub profit_net_eth: f64,
    pub profitable: bool,
}

impl DryRunLogger {
    pub fn new(path: &str) -> Self {
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .expect("Failed to open dry run log file");

        info!(path, "Dry run logger initialized");

        Self {
            file: Mutex::new(file),
            path: path.to_string(),
            count: Mutex::new(0),
        }
    }

    pub fn log_opportunity(&self, record: OpportunityRecord) {
        let mut count = self.count.lock().unwrap();
        *count += 1;
        let id = *count;
        drop(count);

        let mut record = record;
        record.id = id;
        record.timestamp = chrono::Utc::now().to_rfc3339();

        if let Ok(json) = serde_json::to_string(&record) {
            if let Ok(mut file) = self.file.lock() {
                let _ = writeln!(file, "{}", json);
            }
        }

        if record.profitable {
            info!(
                id,
                spread = format!("{:.4}%", record.spread_pct),
                net_spread = format!("{:.4}%", record.net_spread_pct),
                profit_eth = format!("{:.8}", record.profit_net_eth),
                buy = record.buy_dex,
                sell = record.sell_dex,
                "PROFITABLE ARB"
            );
        }
    }

    pub fn log_spread(
        &self,
        trigger_dex: DexType,
        trigger_token_in: Address,
        trigger_token_out: Address,
        buy_pool: &Pool,
        sell_pool: &Pool,
        price_buy: f64,
        price_sell: f64,
        spread_pct: f64,
        // Optional simulation results
        sim: Option<SimResult>,
    ) {
        let buy_fee = buy_pool.fee_bps as f64 / 10000.0;
        let sell_fee = sell_pool.fee_bps as f64 / 10000.0;
        let net_spread = spread_pct - (buy_fee + sell_fee) * 100.0;

        let (optimal_input, sim_bought, sim_sold, profit_gross, profit_gross_eth, profit_net_eth, profitable) =
            match sim {
                Some(s) => (
                    s.optimal_input.to_string(),
                    s.bought.to_string(),
                    s.sold.to_string(),
                    s.profit.to_string(),
                    s.profit_eth,
                    s.profit_eth - 0.000005,
                    s.profit_eth > 0.000005,
                ),
                None => (
                    "0".to_string(), "0".to_string(), "0".to_string(),
                    "0".to_string(), 0.0, 0.0, false,
                ),
            };

        self.log_opportunity(OpportunityRecord {
            timestamp: String::new(),
            id: 0,
            trigger_token_in: format!("{}", trigger_token_in),
            trigger_token_out: format!("{}", trigger_token_out),
            trigger_dex: format!("{:?}", trigger_dex),
            buy_dex: format!("{:?}", buy_pool.dex),
            sell_dex: format!("{:?}", sell_pool.dex),
            buy_pool: format!("{}", buy_pool.address),
            sell_pool: format!("{}", sell_pool.address),
            price_buy,
            price_sell,
            spread_pct,
            net_spread_pct: net_spread,
            buy_fee_bps: buy_pool.fee_bps,
            sell_fee_bps: sell_pool.fee_bps,
            optimal_input,
            sim_bought,
            sim_sold,
            profit_gross,
            profit_gross_eth,
            profit_net_eth,
            profitable,
        });
    }
}

pub struct SimResult {
    pub optimal_input: U256,
    pub bought: U256,
    pub sold: U256,
    pub profit: U256,
    pub profit_eth: f64,
}
