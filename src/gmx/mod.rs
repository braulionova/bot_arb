use alloy::primitives::{address, Address, U256};
use alloy::providers::Provider;
use alloy::sol;
use alloy::sol_types::SolEvent;
use alloy::rpc::types::Filter;
use eyre::Result;
use futures_util::StreamExt;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::{debug, info};

// ─── GMX V2 contract interfaces ───

sol! {
    /// GMX DataStore for reading market/position data
    #[sol(rpc)]
    interface IDataStore {
        function getUint(bytes32 key) external view returns (uint256);
        function getInt(bytes32 key) external view returns (int256);
        function getAddress(bytes32 key) external view returns (address);
    }

    /// GMX EventEmitter events for price updates
    event OraclePriceUpdate(
        address indexed token,
        uint256 minPrice,
        uint256 maxPrice,
        bool isPrimary,
        bool isPriceFeed
    );

    /// GMX V2 Reader interface for markets and swap quotes
    #[sol(rpc)]
    interface IGmxReader {
        struct MarketProps {
            address marketToken;
            address indexToken;
            address longToken;
            address shortToken;
        }

        struct PriceProps {
            uint256 min;
            uint256 max;
        }

        struct MarketPrices {
            PriceProps indexTokenPrice;
            PriceProps longTokenPrice;
            PriceProps shortTokenPrice;
        }

        struct SwapFees {
            uint256 feeReceiverAmount;
            uint256 feeAmountForPool;
            uint256 amountAfterFees;
            address uiFeeReceiver;
            uint256 uiFeeReceiverFactor;
            uint256 uiFeeAmount;
        }

        function getMarkets(
            address dataStore,
            uint256 start,
            uint256 end
        ) external view returns (MarketProps[] memory);

        function getSwapAmountOut(
            address dataStore,
            MarketProps memory market,
            MarketPrices memory prices,
            address tokenIn,
            uint256 amountIn,
            address uiFeeReceiver
        ) external view returns (uint256 amountOut, int256 impactAmount, SwapFees memory fees);
    }
}

// ─── GMX V2 Addresses on Arbitrum ───

pub const GMX_DATASTORE: Address = address!("FD70de6b91282D8017aA4E741e9Ae325CAb992d8");
pub const GMX_READER: Address = address!("f60becbba223EEA9495Da3f606753867eC10d139");
pub const GMX_EVENT_EMITTER: Address = address!("C8ee91A54287DB53897056e12D9819156D3822Fb");

pub const GMX_EXCHANGE_ROUTER: Address = address!("7C68C7866A64FA2160F78EEaE12217FFbf871fa8");
pub const GMX_DEPOSIT_VAULT: Address = address!("F89e77e8Dc11691C9e8757e84aaFbCD8A67d7A55");
pub const GMX_ORDER_VAULT: Address = address!("31eF83a530Fde1B38EE9A18093A333D8Bbbc40D5");

/// A GMX V2 market (GM token pool)
#[derive(Debug, Clone)]
pub struct GmxMarket {
    pub market_token: Address,
    pub index_token: Address,
    pub long_token: Address,
    pub short_token: Address,
}

/// Stores the latest oracle prices from GMX
#[derive(Clone)]
pub struct GmxState {
    /// token address → (min_price, max_price) in GMX precision (30 decimals)
    pub oracle_prices: Arc<RwLock<HashMap<Address, GmxPrice>>>,
    /// Indexed GMX V2 markets
    pub markets: Arc<RwLock<Vec<GmxMarket>>>,
}

#[derive(Debug, Clone)]
pub struct GmxPrice {
    pub min_price: U256,
    pub max_price: U256,
}

impl GmxState {
    pub fn new() -> Self {
        Self {
            oracle_prices: Arc::new(RwLock::new(HashMap::new())),
            markets: Arc::new(RwLock::new(Vec::new())),
        }
    }

    pub async fn get_price(&self, token: &Address) -> Option<GmxPrice> {
        self.oracle_prices.read().await.get(token).cloned()
    }

    pub async fn update_price(&self, token: Address, min_price: U256, max_price: U256) {
        self.oracle_prices.write().await.insert(
            token,
            GmxPrice {
                min_price,
                max_price,
            },
        );
    }
}

/// Represents an arb opportunity between GMX oracle price and AMM spot price
#[derive(Debug)]
pub struct GmxArbOpportunity {
    pub token: Address,
    /// Buy on GMX (oracle price) and sell on AMM, or vice versa
    pub direction: GmxArbDirection,
    /// Price on GMX (mid price, 30 decimals)
    pub gmx_price: U256,
    /// Price on AMM (scaled to 30 decimals for comparison)
    pub amm_price: U256,
    /// Expected profit in USD terms
    pub expected_profit_usd: f64,
}

#[derive(Debug)]
pub enum GmxArbDirection {
    /// GMX price < AMM price: buy on GMX, sell on AMM
    BuyGmxSellAmm,
    /// AMM price < GMX price: buy on AMM, sell on GMX
    BuyAmmSellGmx,
}

/// Subscribe to GMX oracle price updates
pub async fn start_gmx_price_feed<P: Provider + Clone + 'static>(
    ws_provider: P,
    gmx_state: GmxState,
) -> Result<()> {
    info!("Starting GMX oracle price feed");

    let filter = Filter::new()
        .address(GMX_EVENT_EMITTER)
        .event_signature(OraclePriceUpdate::SIGNATURE_HASH);

    let sub = ws_provider.subscribe_logs(&filter).await?;
    let mut stream = sub.into_stream();

    info!("Subscribed to GMX OraclePriceUpdate events");

    while let Some(log) = stream.next().await {
        if let Ok(event) = OraclePriceUpdate::decode_log_data(log.data()) {
            gmx_state
                .update_price(event.token, event.minPrice, event.maxPrice)
                .await;

            debug!(
                token = ?event.token,
                min = ?event.minPrice,
                max = ?event.maxPrice,
                "GMX oracle price updated"
            );
        }
    }

    Ok(())
}

/// Index all GMX V2 markets from the Reader contract
pub async fn index_gmx_markets<P: Provider + Clone>(
    provider: &P,
    gmx_state: &GmxState,
) -> Result<()> {
    let reader = IGmxReader::new(GMX_READER, provider);
    let markets = reader
        .getMarkets(GMX_DATASTORE, U256::ZERO, U256::from(100))
        .call()
        .await?;

    let mut state_markets = gmx_state.markets.write().await;
    for m in markets {
        state_markets.push(GmxMarket {
            market_token: m.marketToken,
            index_token: m.indexToken,
            long_token: m.longToken,
            short_token: m.shortToken,
        });
    }

    info!(count = state_markets.len(), "GMX markets indexed");
    Ok(())
}

/// Monitor GMX oracle prices for divergence vs AMM prices and emit synthetic
/// swap events into the existing arb detection pipeline.
///
/// GMX V2 swaps are 2-step (keeper-executed), so we cannot do atomic flash
/// loan arb through GMX. Instead we watch for price divergence and let the
/// existing AMM arb machinery handle execution across DEXes.
pub async fn start_gmx_backrun_scanner<P: Provider + Clone + 'static>(
    provider: P,
    gmx_state: GmxState,
    pool_state: crate::pools::PoolState,
    swap_sender: tokio::sync::mpsc::UnboundedSender<crate::decoder::DecodedSwap>,
) -> Result<()> {
    info!("Starting GMX backrun scanner");

    let mut last_block = provider.get_block_number().await?;
    let mut interval = tokio::time::interval(std::time::Duration::from_millis(500));

    loop {
        interval.tick().await;

        let current = match provider.get_block_number().await {
            Ok(b) => b,
            Err(_) => continue,
        };

        if current <= last_block {
            continue;
        }

        // Hold read locks for the duration of the scan pass
        let prices = gmx_state.oracle_prices.read().await;
        let markets = gmx_state.markets.read().await;

        for market in markets.iter() {
            let long_token = market.long_token;
            let short_token = market.short_token;

            let (long_price, short_price) = match (
                prices.get(&long_token),
                prices.get(&short_token),
            ) {
                (Some(l), Some(s)) => (l, s),
                _ => continue,
            };

            // GMX mid prices (30-decimal, USD-denominated)
            let gmx_mid_long =
                u256_to_f64((long_price.min_price + long_price.max_price) / U256::from(2));
            let gmx_mid_short =
                u256_to_f64((short_price.min_price + short_price.max_price) / U256::from(2));

            if gmx_mid_short == 0.0 {
                continue;
            }

            // Implied exchange rate: how many short tokens per long token
            let gmx_rate = gmx_mid_long / gmx_mid_short;

            // Compare against every AMM pool for this pair
            let amm_pools = pool_state
                .get_pools_for_pair(long_token, short_token)
                .await;

            for pool in &amm_pools {
                let amm_price = match get_amm_price(pool, &long_token) {
                    Some(p) => p,
                    None => continue,
                };

                let diff_pct = ((gmx_rate - amm_price) / amm_price).abs() * 100.0;

                if diff_pct > 0.3 {
                    info!(
                        market = %market.market_token,
                        pool = %pool.address,
                        gmx_rate = format!("{:.6}", gmx_rate),
                        amm_price = format!("{:.6}", amm_price),
                        diff = format!("{:.3}%", diff_pct),
                        "GMX<>AMM price divergence detected"
                    );

                    // Emit a synthetic swap into the arb pipeline.
                    // The existing detect_arb() / detect_triangular_arb() will
                    // scan all AMM pools for the best route and, if profitable,
                    // execute via the flash loan executor — no new execution path
                    // required.
                    let _ = swap_sender.send(crate::decoder::DecodedSwap {
                        dex: crate::decoder::DexType::Unknown,
                        pool: market.market_token,
                        token_in: long_token,
                        token_out: short_token,
                        amount_in: U256::from(1_000_000_000_000_000_000u128), // 1 token
                        amount_out: U256::ZERO,
                        sender: Address::ZERO,
                    });

                    // Only fire once per market per poll cycle to avoid flooding
                    break;
                }
            }
        }

        last_block = current;
    }
}

/// Check for GMX↔AMM arbitrage opportunities.
///
/// GMX uses oracle pricing (not AMM), so arb exists when:
/// - GMX oracle price diverges from AMM spot price
/// - The difference exceeds execution costs (gas + fees + slippage)
pub fn detect_gmx_arb(
    token: Address,
    gmx_price: &GmxPrice,
    amm_sqrt_price_x96: U256,
    _amm_token0: Address,
    _amm_token1: Address,
    token_is_token0: bool,
    min_profit_usd: f64,
) -> Option<GmxArbOpportunity> {
    // Convert AMM sqrtPriceX96 to a price comparable to GMX's 30-decimal format
    // sqrtPriceX96 = sqrt(price) * 2^96
    // price = (sqrtPriceX96 / 2^96)^2
    let amm_price_30d = sqrt_price_to_30d(amm_sqrt_price_x96, token_is_token0);

    let gmx_mid = (gmx_price.min_price + gmx_price.max_price) / U256::from(2);

    if gmx_mid.is_zero() || amm_price_30d.is_zero() {
        return None;
    }

    // Calculate price difference as a percentage
    let (direction, diff_pct) = if gmx_mid < amm_price_30d {
        let diff = amm_price_30d - gmx_mid;
        let pct = u256_to_f64(diff) / u256_to_f64(amm_price_30d) * 100.0;
        (GmxArbDirection::BuyGmxSellAmm, pct)
    } else {
        let diff = gmx_mid - amm_price_30d;
        let pct = u256_to_f64(diff) / u256_to_f64(gmx_mid) * 100.0;
        (GmxArbDirection::BuyAmmSellGmx, pct)
    };

    // Need at least 0.1% difference to cover execution costs
    if diff_pct < 0.1 {
        return None;
    }

    // Rough profit estimate (assuming $10k trade size)
    let trade_size_usd = 10_000.0;
    let profit = trade_size_usd * (diff_pct / 100.0) - 5.0; // subtract ~$5 for gas + fees

    if profit < min_profit_usd {
        return None;
    }

    Some(GmxArbOpportunity {
        token,
        direction,
        gmx_price: gmx_mid,
        amm_price: amm_price_30d,
        expected_profit_usd: profit,
    })
}

/// Derive the exchange rate (token_in per token_out) from a pool's on-chain state.
/// Returns the price of `token_in` expressed in units of the other token.
fn get_amm_price(pool: &crate::pools::Pool, token_in: &Address) -> Option<f64> {
    if let Some(sqrt_price) = pool.sqrt_price_x96 {
        // V3 pool: price = (sqrtPriceX96 / 2^96)^2
        let sq = u256_to_f64(sqrt_price);
        let price = (sq / (2.0_f64.powi(96))).powi(2);
        if pool.token0 == *token_in {
            Some(price)
        } else if price == 0.0 {
            None
        } else {
            Some(1.0 / price)
        }
    } else {
        // V2 pool: price = reserve_out / reserve_in
        let (r_in, r_out) = if pool.token0 == *token_in {
            (pool.reserve0, pool.reserve1)
        } else {
            (pool.reserve1, pool.reserve0)
        };
        if r_in.is_zero() {
            None
        } else {
            Some(u256_to_f64(r_out) / u256_to_f64(r_in))
        }
    }
}

/// Convert sqrtPriceX96 to a 30-decimal price for GMX comparison
fn sqrt_price_to_30d(sqrt_price_x96: U256, token_is_token0: bool) -> U256 {
    if sqrt_price_x96.is_zero() {
        return U256::ZERO;
    }

    // price = (sqrtPriceX96)^2 / 2^192
    // We want price * 10^30
    // = sqrtPriceX96^2 * 10^30 / 2^192

    let sq = sqrt_price_x96 * sqrt_price_x96;
    let scale = U256::from(10u64).pow(U256::from(30));
    let q192 = U256::from(1u64) << 192;

    let price = sq * scale / q192;

    if token_is_token0 {
        price
    } else {
        // Invert: 10^60 / price
        let scale_sq = U256::from(10u64).pow(U256::from(60));
        if price.is_zero() {
            U256::ZERO
        } else {
            scale_sq / price
        }
    }
}

fn u256_to_f64(v: U256) -> f64 {
    let s = v.to_string();
    s.parse::<f64>().unwrap_or(0.0)
}
