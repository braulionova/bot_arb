use alloy::primitives::{Address, U256};
use alloy::providers::Provider;
use alloy::rpc::types::Filter;
use alloy::sol;
use alloy::sol_types::SolEvent;
use eyre::Result;
use futures_util::StreamExt;
use tracing::{debug, info, warn};

use super::PoolState;

sol! {
    event Sync(uint112 reserve0, uint112 reserve1);

    event Swap(
        address indexed sender,
        address indexed recipient,
        int256 amount0,
        int256 amount1,
        uint160 sqrtPriceX96,
        uint128 liquidity,
        int24 tick
    );

    #[sol(rpc)]
    interface IAlgebraPool {
        function globalState() external view returns (
            uint160 price,
            int24 tick,
            uint16 feeZto,
            uint16 feeOtz,
            uint16 timepointIndex,
            uint8 communityFeeToken0,
            bool unlocked
        );
        function liquidity() external view returns (uint128);
    }
}

/// Subscribe to pool events and update state in real-time.
pub async fn start_reserve_tracker<P: Provider + Clone + 'static>(
    ws_provider: P,
    pool_state: PoolState,
) -> Result<()> {
    info!("Starting reserve tracker via event subscription");

    let pool_addrs: Vec<Address> = {
        let pools = pool_state.pools.read().await;
        pools.keys().cloned().collect()
    };

    if pool_addrs.is_empty() {
        warn!("No pools to track - skipping reserve tracker");
        return Ok(());
    }

    info!(count = pool_addrs.len(), "Tracking pools for reserve updates");

    let sync_topic = Sync::SIGNATURE_HASH;
    let swap_topic = Swap::SIGNATURE_HASH;

    let filter = Filter::new()
        .address(pool_addrs)
        .event_signature(vec![sync_topic, swap_topic]);

    let sub = ws_provider.subscribe_logs(&filter).await?;
    let mut stream = sub.into_stream();

    info!("Subscribed to pool events");

    while let Some(log) = stream.next().await {
        let pool_addr = log.address();

        if log.topic0() == Some(&sync_topic) {
            if let Ok(sync) = Sync::decode_log_data(log.data()) {
                pool_state
                    .update_reserves(
                        pool_addr,
                        U256::from(sync.reserve0),
                        U256::from(sync.reserve1),
                    )
                    .await;

                debug!(pool = ?pool_addr, "V2 reserves updated");
            }
            continue;
        }

        if log.topic0() == Some(&swap_topic) {
            if let Ok(swap) = Swap::decode_log_data(log.data()) {
                pool_state
                    .update_v3_state(
                        pool_addr,
                        U256::from(swap.sqrtPriceX96),
                        swap.tick.as_i32(),
                        swap.liquidity,
                    )
                    .await;

                debug!(pool = ?pool_addr, "V3 state updated");
            }
            continue;
        }
    }

    Ok(())
}

/// Fast periodic refresh of V2 pool reserves (fallback for missed events).
///
/// V2 reserves are the ONLY price source for those pools — stale reserves cause
/// phantom spreads or missed arbs.  Runs on a tight 5-second cadence.
const V2_REFRESH_SECS: u64 = 5;

pub async fn start_v2_refresh<P: Provider + Clone + 'static>(
    provider: P,
    pool_state: PoolState,
) -> Result<()> {
    use super::indexer::IUniswapV2Pair;
    use crate::decoder::DexType;

    let mut interval = tokio::time::interval(std::time::Duration::from_secs(V2_REFRESH_SECS));

    loop {
        interval.tick().await;

        let v2_pools: Vec<Address> = {
            let pools = pool_state.pools.read().await;
            pools
                .values()
                .filter(|p| matches!(
                    p.dex,
                    DexType::UniswapV2 | DexType::CamelotV2 | DexType::SushiSwapV2 | DexType::RamsesV2
                ))
                .map(|p| p.address)
                .collect()
        };

        let mut updated = 0u32;
        for addr in &v2_pools {
            let pair = IUniswapV2Pair::new(*addr, &provider);
            if let Ok(reserves) = pair.getReserves().call().await {
                pool_state
                    .update_reserves(
                        *addr,
                        U256::from(reserves.reserve0),
                        U256::from(reserves.reserve1),
                    )
                    .await;
                updated += 1;
            }
        }

        debug!(updated, total = v2_pools.len(), "V2 reserve refresh complete");
    }
}

/// Periodic refresh of Curve stable pool balances.
/// Curve pools don't emit Sync events, so we must poll.
pub async fn start_curve_refresh<P: Provider + Clone + 'static>(
    provider: P,
    pool_state: PoolState,
) -> Result<()> {
    use super::indexer::ICurvePool;
    use crate::decoder::DexType;

    // Curve balances change slowly for stable pools — 15s cadence is sufficient
    const CURVE_REFRESH_SECS: u64 = 15;
    let mut interval = tokio::time::interval(std::time::Duration::from_secs(CURVE_REFRESH_SECS));

    loop {
        interval.tick().await;

        let curve_pools: Vec<Address> = {
            let pools = pool_state.pools.read().await;
            pools
                .values()
                .filter(|p| matches!(p.dex, DexType::CurveStable | DexType::BalancerStable))
                .map(|p| p.address)
                .collect()
        };

        if curve_pools.is_empty() {
            continue;
        }

        let mut updated = 0u32;
        for addr in &curve_pools {
            let c = ICurvePool::new(*addr, &provider);
            let b0_b = c.balances(alloy::primitives::U256::ZERO);
            let b1_b = c.balances(alloy::primitives::U256::from(1u64));
            if let (Ok(b0), Ok(b1)) = tokio::join!(b0_b.call(), b1_b.call()) {
                pool_state.update_reserves(*addr, b0, b1).await;
                updated += 1;
            }
        }

        debug!(updated, total = curve_pools.len(), "Curve balance refresh complete");
    }
}

/// Slower periodic refresh of V3 pool state (fallback for missed events).
pub async fn start_v3_refresh<P: Provider + Clone + 'static>(
    provider: P,
    pool_state: PoolState,
    interval_secs: u64,
) -> Result<()> {
    use super::indexer::IUniswapV3Pool;
    use crate::decoder::DexType;

    let mut interval = tokio::time::interval(std::time::Duration::from_secs(interval_secs));

    loop {
        interval.tick().await;

        let v3_pools: Vec<Address> = {
            let pools = pool_state.pools.read().await;
            pools
                .values()
                .filter(|p| matches!(p.dex, DexType::UniswapV3 | DexType::CamelotV3 | DexType::SushiSwapV3 | DexType::PancakeSwapV3))
                .map(|p| p.address)
                .collect()
        };

        let mut updated = 0u32;
        for addr in &v3_pools {
            // Try standard slot0() first (UniV3, PcsV3, SushiV3)
            let pool_contract = IUniswapV3Pool::new(*addr, &provider);
            let s0_b = pool_contract.slot0();
            let liq_b = pool_contract.liquidity();
            if let (Ok(slot0), Ok(liq)) = tokio::join!(s0_b.call(), liq_b.call()) {
                pool_state
                    .update_v3_state(
                        *addr,
                        U256::from(slot0.sqrtPriceX96),
                        slot0.tick.as_i32(),
                        liq,
                    )
                    .await;
                updated += 1;
            } else {
                // Fallback: Algebra/CamelotV3 uses globalState() instead of slot0()
                let algebra_pool = IAlgebraPool::new(*addr, &provider);
                let gs_b = algebra_pool.globalState();
                let liq_b2 = algebra_pool.liquidity();
                if let (Ok(gs), Ok(liq)) = tokio::join!(gs_b.call(), liq_b2.call()) {
                    pool_state
                        .update_v3_state(
                            *addr,
                            U256::from(gs.price),
                            gs.tick.as_i32(),
                            liq,
                        )
                        .await;
                    updated += 1;
                }
            }
        }

        debug!(updated, total = v3_pools.len(), "V3 state refresh complete");
    }
}
