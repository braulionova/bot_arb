/// Pool sniping: detect new pool creation events and arb against existing pools.
/// New pools often have mispriced initial liquidity (window: 5-60 seconds).
/// 400ms latency is irrelevant here — we scan factory events every 500ms.

use alloy::primitives::{Address, B256, U256};
use alloy::providers::Provider;
use alloy::rpc::types::Filter;
use alloy::sol;
use alloy::sol_types::SolEvent;
use eyre::Result;
use tracing::{debug, info};

use crate::decoder::{DecodedSwap, DexType};
use crate::pools::indexer;
use crate::pools::{Pool, PoolState};

// Factory addresses on Arbitrum (re-declared here to avoid coupling with indexer internals)
const UNISWAP_V3_FACTORY: Address = indexer::UNISWAP_V3_FACTORY;
const UNISWAP_V2_FACTORY: Address = indexer::UNISWAP_V2_FACTORY;
const CAMELOT_V2_FACTORY: Address = indexer::CAMELOT_V2_FACTORY;
const SUSHISWAP_V2_FACTORY: Address = indexer::SUSHISWAP_V2_FACTORY;
const RAMSES_V2_FACTORY: Address = indexer::RAMSES_V2_FACTORY;
const RAMSES_V3_FACTORY: Address = indexer::RAMSES_V3_FACTORY;
const PANCAKESWAP_V3_FACTORY: Address = indexer::PANCAKESWAP_V3_FACTORY;
const SUSHISWAP_V3_FACTORY: Address = indexer::SUSHISWAP_V3_FACTORY;

// Safe tokens — only snipe pools containing at least one of these (anti-rug)
fn is_safe_token(token: &Address) -> bool {
    *token == indexer::WETH
        || *token == indexer::USDC
        || *token == indexer::USDC_E
        || *token == indexer::USDT
        || *token == indexer::WBTC
        || *token == indexer::ARB
        || *token == indexer::DAI
        || *token == indexer::GMX
        || *token == indexer::LINK
        || *token == indexer::UNI
        || *token == indexer::PENDLE
        || *token == indexer::RDNT
        || *token == indexer::MAGIC
        || *token == indexer::GRAIL
        || *token == indexer::DPX
        || *token == indexer::STG
        || *token == indexer::JOE
        || *token == indexer::WSTETH
        || *token == indexer::FRAX
        || *token == indexer::RSETH
        || *token == indexer::VELA
        || *token == indexer::CBBTC
        || *token == indexer::GNS
        || *token == indexer::GNO
        || *token == indexer::APE
        || *token == indexer::USDS
        || *token == indexer::IDOS
}

sol! {
    // V2/V2-fork factory event
    event PairCreated(
        address indexed token0,
        address indexed token1,
        address pair,
        uint256 allPairsLength
    );

    // V3 factory event
    event PoolCreated(
        address indexed token0,
        address indexed token1,
        uint24 indexed fee,
        int24 tickSpacing,
        address pool
    );
}

/// Start the pool sniping scanner.
/// Polls factory events for new pool creation and immediately checks for arb opportunities.
pub async fn start_pool_sniper<P: Provider + Clone + 'static>(
    provider: P,
    pool_state: PoolState,
    swap_sender: tokio::sync::mpsc::UnboundedSender<DecodedSwap>,
) -> Result<()> {
    info!("Starting pool sniper — monitoring factory events");

    let pair_created_topic: B256 = PairCreated::SIGNATURE_HASH;
    let pool_created_topic: B256 = PoolCreated::SIGNATURE_HASH;

    let factories = vec![
        UNISWAP_V2_FACTORY,
        UNISWAP_V3_FACTORY,
        CAMELOT_V2_FACTORY,
        SUSHISWAP_V2_FACTORY,
        RAMSES_V2_FACTORY,
        RAMSES_V3_FACTORY,
        PANCAKESWAP_V3_FACTORY,
        SUSHISWAP_V3_FACTORY,
    ];

    let mut last_block = provider.get_block_number().await?;
    let mut interval = tokio::time::interval(std::time::Duration::from_millis(500));

    loop {
        interval.tick().await;

        let current_block = match provider.get_block_number().await {
            Ok(b) => b,
            Err(_) => continue,
        };

        if current_block <= last_block {
            continue;
        }

        // Query factory creation events from both V2 and V3 factories
        let filter = Filter::new()
            .address(factories.clone())
            .event_signature(vec![pair_created_topic, pool_created_topic])
            .from_block(last_block + 1)
            .to_block(current_block);

        let logs = match provider.get_logs(&filter).await {
            Ok(l) => l,
            Err(e) => {
                debug!(error = %e, "Factory log query failed");
                last_block = current_block;
                continue;
            }
        };

        for log in &logs {
            let factory = log.address();
            let topic0 = log.topic0().copied().unwrap_or_default();

            if topic0 == pair_created_topic {
                // V2 PairCreated — token0 and token1 are indexed (in topics), pair is in data
                if let Ok(event) = PairCreated::decode_log_data(log.data()) {
                    let token0 = event.token0;
                    let token1 = event.token1;
                    let pair = event.pair;

                    // Allow longtail: at least ONE token must be a flash-loanable base
                    if !is_safe_token(&token0) && !is_safe_token(&token1) {
                        debug!(%token0, %token1, "Skipping — no base token in pair");
                        continue;
                    }

                    let dex = identify_v2_dex(factory);
                    info!(
                        dex = ?dex,
                        %token0,
                        %token1,
                        %pair,
                        "NEW V2 POOL DETECTED — sniping"
                    );

                    if index_new_v2_pool(&provider, pair, dex, &pool_state).await.is_ok() {
                        let existing = pool_state.get_pools_for_pair(token0, token1).await;
                        if existing.len() >= 2 {
                            info!(pools = existing.len(), "Arb opportunity — {} pools for new pair", existing.len());
                            let _ = swap_sender.send(DecodedSwap {
                                dex,
                                pool: pair,
                                token_in: token0,
                                token_out: token1,
                                amount_in: U256::from(50_000_000_000_000_000u128), // 0.05 ETH
                                amount_out: U256::ZERO,
                                sender: Address::ZERO,
                            });
                        }
                    }
                }
            } else if topic0 == pool_created_topic {
                // V3 PoolCreated — token0, token1, fee are indexed topics; tickSpacing + pool in data
                if let Ok(event) = PoolCreated::decode_log_data(log.data()) {
                    let token0 = event.token0;
                    let token1 = event.token1;
                    let fee = event.fee;
                    let pool_addr = event.pool;

                    if !is_safe_token(&token0) && !is_safe_token(&token1) {
                        debug!(%token0, %token1, "Skipping — no base token in V3 pair");
                        continue;
                    }

                    let dex = identify_v3_dex(factory);
                    let fee_u32: u32 = fee.to();
                    info!(
                        dex = ?dex,
                        %token0,
                        %token1,
                        %pool_addr,
                        fee = fee_u32,
                        "NEW V3 POOL DETECTED — sniping"
                    );

                    if let Ok(true) = indexer::index_single_v3_pool(
                        provider.clone(),
                        pool_addr,
                        dex,
                        &pool_state,
                    )
                    .await
                    {
                        let existing = pool_state.get_pools_for_pair(token0, token1).await;
                        if existing.len() >= 2 {
                            info!(pools = existing.len(), "V3 arb opportunity — {} pools", existing.len());
                            let _ = swap_sender.send(DecodedSwap {
                                dex,
                                pool: pool_addr,
                                token_in: token0,
                                token_out: token1,
                                amount_in: U256::from(50_000_000_000_000_000u128),
                                amount_out: U256::ZERO,
                                sender: Address::ZERO,
                            });
                        }
                    }
                }
            }
        }

        last_block = current_block;
    }
}

fn identify_v2_dex(factory: Address) -> DexType {
    if factory == UNISWAP_V2_FACTORY {
        DexType::UniswapV2
    } else if factory == CAMELOT_V2_FACTORY {
        DexType::CamelotV2
    } else if factory == SUSHISWAP_V2_FACTORY {
        DexType::SushiSwapV2
    } else if factory == RAMSES_V2_FACTORY {
        DexType::RamsesV2
    } else {
        DexType::UniswapV2
    }
}

fn identify_v3_dex(factory: Address) -> DexType {
    if factory == UNISWAP_V3_FACTORY {
        DexType::UniswapV3
    } else if factory == PANCAKESWAP_V3_FACTORY {
        DexType::PancakeSwapV3
    } else {
        DexType::UniswapV3
    }
}

async fn index_new_v2_pool<P: Provider + Clone>(
    provider: &P,
    pair_addr: Address,
    dex: DexType,
    pool_state: &PoolState,
) -> Result<()> {
    use indexer::IUniswapV2Pair;

    let pair = IUniswapV2Pair::new(pair_addr, provider);
    let t0_b = pair.token0();
    let t1_b = pair.token1();
    let res_b = pair.getReserves();
    let (token0, token1, reserves) = tokio::try_join!(t0_b.call(), t1_b.call(), res_b.call())?;

    // Skip empty / zero-liquidity pools
    if reserves.reserve0 == 0 || reserves.reserve1 == 0 {
        debug!(%pair_addr, "New V2 pool has zero reserves — skipping");
        return Ok(());
    }

    pool_state
        .insert_pool(Pool {
            address: pair_addr,
            dex,
            token0,
            token1,
            reserve0: U256::from(reserves.reserve0),
            reserve1: U256::from(reserves.reserve1),
            fee_bps: 30, // default 0.3%
            sqrt_price_x96: None,
            tick: None,
            liquidity: None,
            fee_bps_token0: None,
            fee_bps_token1: None,
            last_update: Some(std::time::Instant::now()),
        })
        .await;

    info!(%pair_addr, "New V2 pool indexed");
    Ok(())
}
