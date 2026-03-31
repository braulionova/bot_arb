pub mod sniper;

use alloy::primitives::{Address, B256, U256};
use alloy::providers::Provider;
use alloy::rpc::types::{Filter, Log};
use alloy::sol;
use alloy::sol_types::SolEvent;
use eyre::Result;
use tokio::sync::mpsc;
use tracing::{debug, info, warn};

use crate::decoder::{DecodedSwap, DexType};
use crate::pools::{Pool, PoolState};
use crate::pools::indexer::IUniswapV3Pool;

// V3 Swap event: Swap(address indexed sender, address indexed recipient, int256 amount0, int256 amount1, uint160 sqrtPriceX96, uint128 liquidity, int24 tick)
// topic0 = 0xc42079f94a6350d7e6235f29174924f928cc2ac818eb64fed8004e115fbcca67
sol! {
    event Swap(
        address indexed sender,
        address indexed recipient,
        int256 amount0,
        int256 amount1,
        uint160 sqrtPriceX96,
        uint128 liquidity,
        int24 tick
    );
}

// V2 Swap event has a DIFFERENT signature (same name but different params)
// topic0 = 0xd78ad95fa46c994b6551d0da85fc275fe613ce37657fb8d5e3d130840159d822
// We decode it manually since it conflicts with the V3 Swap name

const V3_SWAP_TOPIC: &str = "0xc42079f94a6350d7e6235f29174924f928cc2ac818eb64fed8004e115fbcca67";
const V2_SWAP_TOPIC: &str = "0xd78ad95fa46c994b6551d0da85fc275fe613ce37657fb8d5e3d130840159d822";
// Sync(uint112 reserve0, uint112 reserve1) — emitted with every V2 swap
const SYNC_TOPIC: &str = "0x1c411e9a96e071241c2f21f7726b17ae89e3cab4c78be50e062b03a9fffbbad1";

pub async fn start_event_scanner<P: Provider + Clone + 'static>(
    provider: P,
    pool_state: PoolState,
    swap_sender: mpsc::UnboundedSender<DecodedSwap>,
) -> Result<()>
where P: Send + Sync {
    info!("Starting event-based swap scanner");

    let v3_topic: B256 = V3_SWAP_TOPIC.parse()?;
    let v2_topic: B256 = V2_SWAP_TOPIC.parse()?;
    let sync_topic: B256 = SYNC_TOPIC.parse()?;

    // Track pools we've already tried to discover (avoid repeated failed queries)
    let mut discovery_cache: std::collections::HashSet<Address> = std::collections::HashSet::new();

    let mut last_block = provider.get_block_number().await?;
    info!(block = last_block, "Scanning from block");

    // Poll every 100ms for minimum latency (Arbitrum block = 250ms)
    let mut interval = tokio::time::interval(std::time::Duration::from_millis(100));

    loop {
        interval.tick().await;

        let current_block = match provider.get_block_number().await {
            Ok(b) => b,
            Err(_) => continue,
        };

        if current_block <= last_block {
            continue;
        }

        let from_block = last_block + 1;

        // Query V3 swap events
        let v3_filter = Filter::new()
            .from_block(from_block)
            .to_block(current_block)
            .event_signature(v3_topic);

        let v3_logs = match provider.get_logs(&v3_filter).await {
            Ok(l) => l,
            Err(e) => {
                warn!(error = %e, "V3 log query failed");
                last_block = current_block;
                continue;
            }
        };

        // Query V2 swap + Sync events (Sync gives us reserves for V2 pools)
        let v2_filter = Filter::new()
            .from_block(from_block)
            .to_block(current_block)
            .event_signature(vec![v2_topic, sync_topic]);

        let v2_logs = provider.get_logs(&v2_filter).await.unwrap_or_default();

        // Process Sync events first to update reserves
        for log in &v2_logs {
            if log.topic0() == Some(&sync_topic) {
                let pool_addr = log.address();
                let data = log.data().data.as_ref();
                if data.len() >= 64 {
                    let reserve0 = U256::from_be_slice(&data[0..32]);
                    let reserve1 = U256::from_be_slice(&data[32..64]);
                    if reserve0 > U256::ZERO && reserve1 > U256::ZERO {
                        pool_state.update_reserves(pool_addr, reserve0, reserve1).await;
                    }
                }
            }
        }

        let total_logs = v3_logs.len() + v2_logs.len();
        let blocks_scanned = current_block - last_block;

        if total_logs > 0 {
            info!(blocks = blocks_scanned, v3 = v3_logs.len(), v2 = v2_logs.len(), "Swap events found!");
        }

        // Process V3 swaps
        for log in &v3_logs {
            let pool_addr = log.address();
            let is_known = pool_state.pools.read().await.contains_key(&pool_addr);

            if !is_known && !discovery_cache.contains(&pool_addr) {
                discovery_cache.insert(pool_addr);
                // Auto-discover with rate limiting (1 pool per batch max)
                if let Some(p) = auto_discover_v3_pool(pool_addr, &pool_state, &provider).await {
                    info!(pool = %pool_addr, t0 = %p.token0, t1 = %p.token1, "Auto-discovered V3 pool");
                }
            }
            process_v3_swap(log, &pool_state, &swap_sender).await;
        }

        // Process V2 swaps
        for log in &v2_logs {
            process_v2_swap(log, &pool_state, &swap_sender).await;
        }

        last_block = current_block;
    }
}

async fn process_v3_swap(
    log: &Log,
    pool_state: &PoolState,
    swap_sender: &mpsc::UnboundedSender<DecodedSwap>,
) {
    let pool_addr = log.address();

    let swap = match Swap::decode_log_data(log.data()) {
        Ok(s) => s,
        Err(e) => {
            debug!(pool = %pool_addr, error = %e, "Failed to decode V3 swap");
            return;
        }
    };

    let pool = match pool_state.pools.read().await.get(&pool_addr).cloned() {
        Some(p) => p,
        None => return,
    };

    // Update pool state with latest price
    pool_state
        .update_v3_state(
            pool_addr,
            U256::from(swap.sqrtPriceX96),
            swap.tick.as_i32(),
            swap.liquidity,
        )
        .await;

    let (token_in, token_out, amount_in) = if swap.amount0.is_negative() {
        (pool.token1, pool.token0, swap.amount1.unsigned_abs())
    } else {
        (pool.token0, pool.token1, swap.amount0.unsigned_abs())
    };

    info!(
        dex = ?pool.dex,
        pool = %pool_addr,
        token_in = %token_in,
        token_out = %token_out,
        amount = ?amount_in,
        "V3 SWAP"
    );

    let _ = swap_sender.send(DecodedSwap {
        dex: pool.dex,
        pool: pool_addr,
        token_in,
        token_out,
        amount_in,
        amount_out: U256::ZERO,
        sender: Address::ZERO,
    });
}

/// Auto-discover a V3 pool by querying its contract on-chain
async fn auto_discover_v3_pool<P: Provider>(
    pool_addr: Address,
    pool_state: &PoolState,
    provider: &P,
) -> Option<Pool> {
    // Verify pool has code on-chain (skip ghost/self-destructed addresses)
    let code = provider.get_code_at(pool_addr).await.ok()?;
    if code.is_empty() {
        return None;
    }

    let contract = IUniswapV3Pool::new(pool_addr, provider);

    let token0 = contract.token0().call().await.ok()?;
    let token1 = contract.token1().call().await.ok()?;
    let fee = contract.fee().call().await.unwrap_or_default();
    let liquidity = contract.liquidity().call().await.unwrap_or(0);

    let fee_bps: u32 = fee.to::<u32>() / 10;

    // Skip pools with 0 fee (likely Algebra/Camelot V3 that don't have fee())
    if fee_bps == 0 {
        return None;
    }

    // Skip pools with 0 liquidity
    if liquidity == 0 {
        return None;
    }

    let pool = Pool {
        address: pool_addr,
        dex: DexType::UniswapV3,
        token0,
        token1,
        reserve0: U256::ZERO,
        reserve1: U256::ZERO,
        fee_bps,
        sqrt_price_x96: None,
        tick: None,
        liquidity: Some(liquidity),
        fee_bps_token0: None,
        fee_bps_token1: None,
    };

    pool_state.insert_pool(pool.clone()).await;

    Some(pool)
}

async fn process_v2_swap(
    log: &Log,
    pool_state: &PoolState,
    swap_sender: &mpsc::UnboundedSender<DecodedSwap>,
) {
    let pool_addr = log.address();

    let pool_info = pool_state.pools.read().await;
    let pool = match pool_info.get(&pool_addr) {
        Some(p) => p.clone(),
        None => return,
    };
    drop(pool_info);

    // V2 Swap data: amount0In(uint256), amount1In(uint256), amount0Out(uint256), amount1Out(uint256)
    let data = log.data().data.as_ref();
    if data.len() < 128 {
        return;
    }
    {
        let amount0_in = U256::from_be_slice(&data[0..32]);
        let amount1_in = U256::from_be_slice(&data[32..64]);
        let amount0_out = U256::from_be_slice(&data[64..96]);
        let amount1_out = U256::from_be_slice(&data[96..128]);

        let (token_in, token_out, amount_in, amount_out) = if amount0_in > U256::ZERO {
            (pool.token0, pool.token1, amount0_in, amount1_out)
        } else {
            (pool.token1, pool.token0, amount1_in, amount0_out)
        };

        info!(
            dex = ?pool.dex,
            pool = %pool_addr,
            token_in = %token_in,
            token_out = %token_out,
            "V2 SWAP"
        );

        // Update V2 reserves from swap amounts
        // New reserves = old_reserves + amountsIn - amountsOut
        if pool.reserve0 > U256::ZERO {
            let new_r0 = pool.reserve0 + amount0_in - amount0_out;
            let new_r1 = pool.reserve1 + amount1_in - amount1_out;
            pool_state.update_reserves(pool_addr, new_r0, new_r1).await;
        }

        let _ = swap_sender.send(DecodedSwap {
            dex: pool.dex,
            pool: pool_addr,
            token_in,
            token_out,
            amount_in,
            amount_out,
            sender: Address::ZERO,
        });
    }
}
