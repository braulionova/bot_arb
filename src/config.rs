use alloy::primitives::{Address, U256};
use eyre::{Result, WrapErr};

#[derive(Debug, Clone)]
pub struct Config {
    /// Local node HTTP RPC
    pub node_rpc_url: String,
    /// Local node WebSocket RPC
    pub node_ws_url: String,
    /// Sequencer feed WebSocket
    pub sequencer_feed_url: String,
    /// Sequencer endpoint for sending txs directly
    pub sequencer_endpoint_url: String,
    /// Private key for signing transactions (hex)
    pub private_key: String,
    /// Bot wallet address (for nonce tracking without deriving from key)
    pub bot_address: Address,
    /// On-chain ArbExecutor contract address
    pub arb_contract: Address,
    /// Minimum profit threshold in ETH to execute an arb
    pub min_profit_eth: f64,
    /// Minimum profit threshold in USD for GMX arbs
    pub min_profit_usd: f64,
    /// Max Timeboost bid in wei
    pub timeboost_max_bid_wei: U256,
    /// Pool reserve refresh interval in seconds
    pub pool_refresh_interval_secs: u64,
    /// Maximum gas price in gwei (abort arb if gas exceeds this)
    pub max_gas_price_gwei: f64,
    /// Fallback RPC URL (used when local node is unreachable)
    pub fallback_rpc_url: Option<String>,
}

impl Config {
    pub fn from_env() -> Result<Self> {
        dotenvy::dotenv().ok();

        let bot_address: Address = std::env::var("BOT_ADDRESS")
            .unwrap_or_else(|_| "0xd69f9856a569b1655b43b0395b7c2923a217cfe0".to_string())
            .parse()
            .wrap_err("Invalid BOT_ADDRESS")?;

        let arb_contract: Address = std::env::var("ARB_CONTRACT")
            .unwrap_or_else(|_| "0x0000000000000000000000000000000000000000".to_string())
            .parse()
            .wrap_err("Invalid ARB_CONTRACT")?;

        let timeboost_max_bid = std::env::var("TIMEBOOST_MAX_BID_ETH")
            .unwrap_or_else(|_| "0.01".to_string())
            .parse::<f64>()
            .wrap_err("Invalid TIMEBOOST_MAX_BID_ETH")?;

        Ok(Self {
            node_rpc_url: std::env::var("NODE_RPC_URL")
                .unwrap_or_else(|_| "http://127.0.0.1:8547".to_string()),
            node_ws_url: std::env::var("NODE_WS_URL")
                .unwrap_or_else(|_| "ws://127.0.0.1:8548".to_string()),
            sequencer_feed_url: std::env::var("SEQUENCER_FEED_URL")
                .unwrap_or_else(|_| "wss://arb1.arbitrum.io/feed".to_string()),
            sequencer_endpoint_url: std::env::var("SEQUENCER_ENDPOINT_URL")
                .unwrap_or_else(|_| "https://arb1-sequencer.arbitrum.io/rpc".to_string()),
            private_key: std::env::var("PRIVATE_KEY")
                .wrap_err("PRIVATE_KEY env var required")?,
            bot_address,
            arb_contract,
            min_profit_eth: std::env::var("MIN_PROFIT_ETH")
                .unwrap_or_else(|_| "0.001".to_string())
                .parse()
                .wrap_err("Invalid MIN_PROFIT_ETH")?,
            min_profit_usd: std::env::var("MIN_PROFIT_USD")
                .unwrap_or_else(|_| "1.0".to_string())
                .parse()
                .wrap_err("Invalid MIN_PROFIT_USD")?,
            timeboost_max_bid_wei: U256::from((timeboost_max_bid * 1e18) as u128),
            pool_refresh_interval_secs: std::env::var("POOL_REFRESH_SECS")
                .unwrap_or_else(|_| "30".to_string())
                .parse()
                .unwrap_or(30),
            max_gas_price_gwei: std::env::var("MAX_GAS_PRICE_GWEI")
                .unwrap_or_else(|_| "0.5".to_string())
                .parse()
                .unwrap_or(0.5),
            fallback_rpc_url: std::env::var("FALLBACK_RPC_URL").ok(),
        })
    }
}
