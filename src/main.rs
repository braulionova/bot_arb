mod arb;
mod config;
mod decoder;
mod dryrun;
mod executor;
mod feed;
mod gmx;
mod liquidation;
mod pools;
mod scanner;
mod timeboost;
mod sim;
mod telegram;
mod wallet;

use alloy::primitives::Address;
use alloy::providers::{Provider, ProviderBuilder};
use eyre::Result;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, AtomicBool, Ordering};
use tokio::sync::mpsc;
use tracing::{debug, error, info, warn};

use crate::arb::{detect_arb, detect_stablecoin_depeg, detect_triangular_arb, ArbCooldown};
use crate::config::Config;
use crate::decoder::DecodedSwap;
use crate::dryrun::DryRunLogger;
use crate::executor::Executor;
use crate::liquidation::{discover_borrowers, start_position_monitor, LiquidationMonitor};
use crate::pools::PoolState;
use crate::pools::indexer::{index_priority_pools, index_background};
use crate::pools::tracker::{start_curve_refresh, start_v2_refresh, start_v3_refresh};
use crate::scanner::start_event_scanner;
use crate::sim::LocalSim;
use crate::telegram::TgMsg;
use crate::wallet::WalletManager;

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "arbitrum_bot=info".into()),
        )
        .init();

    let config = Config::from_env()?;

    info!("╔══════════════════════════════════════════╗");
    info!("║       ARBITRUM MEV BOT — FLASH LOANS     ║");
    info!("╚══════════════════════════════════════════╝");
    info!(bot = %config.bot_address, "Wallet");
    info!(contract = %config.arb_contract, "ArbExecutor");

    // ─── Wait for node if local ───

    let rpc_url = wait_for_rpc(&config.node_rpc_url, config.fallback_rpc_url.as_deref()).await?;
    info!(rpc = %rpc_url, "RPC ready");

    // ─── Connect ───

    let node_provider = ProviderBuilder::new()
        .connect_http(rpc_url.parse()?);

    // Build a wallet-enabled provider for sending signed txs
    // Use Arbitrum sequencer RPC directly for lowest latency
    let signer: alloy::signers::local::PrivateKeySigner = config.private_key
        .strip_prefix("0x").unwrap_or(&config.private_key)
        .parse()?;
    let eth_wallet = alloy::network::EthereumWallet::from(signer);
    // Send provider: needs eth_chainId support (alloy signing requirement).
    // Local node supports all methods and has lowest latency.
    let send_rpc = &config.node_rpc_url;
    let send_provider = ProviderBuilder::new()
        .wallet(eth_wallet)
        .connect_http(send_rpc.parse()?);
    info!(rpc = %send_rpc, "Send provider (local node)");

    let chain_id = node_provider.get_chain_id().await?;
    info!(chain_id, "Connected");

    // Retry block/balance queries (upstreams may be temporarily rate-limited)
    let mut block = 0u64;
    for attempt in 1..=5 {
        match node_provider.get_block_number().await {
            Ok(b) => { block = b; break; }
            Err(e) => {
                warn!(attempt, error = %e, "get_block_number failed, retrying in 3s...");
                tokio::time::sleep(std::time::Duration::from_secs(3)).await;
            }
        }
    }
    info!(block, "Block");

    let mut balance = alloy::primitives::U256::ZERO;
    for attempt in 1..=5 {
        match node_provider.get_balance(config.bot_address).await {
            Ok(b) => { balance = b; break; }
            Err(e) => {
                warn!(attempt, error = %e, "get_balance failed, retrying in 3s...");
                tokio::time::sleep(std::time::Duration::from_secs(3)).await;
            }
        }
    }
    let balance = balance;
    let eth_bal = balance.to_string().parse::<f64>().unwrap_or(0.0) / 1e18;
    info!(eth = %format!("{:.6}", eth_bal), "Balance");

    if eth_bal < 0.001 {
        warn!("Low balance! Need ETH for gas. Recommend 0.05 ETH minimum.");
    }

    // ─── Wallet ───

    let wallet = WalletManager::from_private_key(&config.private_key)?;
    wallet.sync_nonce(&node_provider).await?;

    // ─── Pool state ───

    let pool_state = PoolState::new();

    // Phase 1: instant pool loading — ZERO RPC calls
    // All pool metadata hardcoded, reserves filled by rpc-cache refresher in 250ms
    if let Err(e) = index_priority_pools(node_provider.clone(), &pool_state).await {
        warn!(error = %e, "Priority pool indexing partial");
    }

    // Phase 2: background indexing of remaining pools (while bot trades)
    let bg_provider = node_provider.clone();
    let bg_pool_state = pool_state.clone();
    tokio::spawn(async move {
        index_background(bg_provider, bg_pool_state).await;
    });

    // ─── Real-time: WebSocket reserve tracker (V2 Sync + V3 Swap events) ───
    // Gives instant state updates; polling refresh below is fallback for missed events.

    let ws_url = config.node_ws_url.clone();
    let ws_ps = pool_state.clone();
    tokio::spawn(async move {
        loop {
            match ProviderBuilder::new().connect_ws(alloy::transports::ws::WsConnect::new(&ws_url)).await {
                Ok(ws_prov) => {
                    info!("WebSocket reserve tracker connected");
                    if let Err(e) = crate::pools::tracker::start_reserve_tracker(ws_prov, ws_ps.clone()).await {
                        error!(error = %e, "WebSocket reserve tracker error");
                    }
                }
                Err(e) => {
                    warn!(error = %e, "WebSocket connect failed, retrying in 5s");
                }
            }
            tokio::time::sleep(std::time::Duration::from_secs(5)).await;
        }
    });

    // ─── Background: reserve refresh (fallback for missed WS events) ───

    let ps = pool_state.clone();
    let np = node_provider.clone();
    tokio::spawn(async move {
        if let Err(e) = start_v2_refresh(np, ps).await {
            error!(error = %e, "V2 refresh failed");
        }
    });

    let ps = pool_state.clone();
    let np = node_provider.clone();
    let secs = config.pool_refresh_interval_secs;
    tokio::spawn(async move {
        if let Err(e) = start_v3_refresh(np, ps, secs).await {
            error!(error = %e, "V3 refresh failed");
        }
    });

    let ps = pool_state.clone();
    let np = node_provider.clone();
    tokio::spawn(async move {
        if let Err(e) = start_curve_refresh(np, ps).await {
            error!(error = %e, "Curve refresh failed");
        }
    });

    // ─── Liquidation monitor ───

    let liq_monitor = LiquidationMonitor::new();

    // One-shot borrower discovery from recent Borrow events
    let liq_np = node_provider.clone();
    let liq_mon = liq_monitor.clone();
    tokio::spawn(async move {
        discover_borrowers(liq_np, liq_mon).await;
    });

    // Periodic health-factor refresh (every 30s)
    let liq_np = node_provider.clone();
    let liq_mon = liq_monitor.clone();
    tokio::spawn(async move {
        start_position_monitor(liq_np, liq_mon).await;
    });

    // ─── Telegram notifications ───

    let tg_tx = telegram::start_tg_sender();
    if tg_tx.is_some() {
        info!("Telegram notifications enabled");
    }

    // ─── Executor ───

    // ML_OBSERVE_ONLY: detect + simulate but do NOT send tx. Collects training data.
    // Set to false once ML model is trained and ready.
    let ml_observe_only = std::env::var("ML_OBSERVE_ONLY").unwrap_or_else(|_| "true".to_string()) == "true";

    let live_mode = config.arb_contract != Address::ZERO && !ml_observe_only;
    if ml_observe_only {
        info!(contract = %config.arb_contract, "ML OBSERVE MODE — simulating only, collecting training data");
    } else if live_mode {
        info!(contract = %config.arb_contract, "LIVE MODE — flash loan execution enabled");
    } else {
        warn!("DRY RUN MODE — no contract deployed");
    }

    // Local simulator via raw eth_call (~1-3ms when synced)
    let mut sim = LocalSim::new(config.bot_address);
    sim.set_rpc(&config.node_rpc_url);
    if let Some(ref fb) = config.fallback_rpc_url {
        sim.set_fallback(fb);
    }
    // Background sync monitor: auto-routes sims to local vs fallback based on sync state
    sim.start_sync_monitor();
    let local_sim = Arc::new(sim);

    // Timeboost: enabled for high-profit arbs (200ms priority compensates AWS latency)
    let timeboost: Option<Arc<crate::timeboost::TimeboostManager>> = {
        let tb_signer: alloy::signers::local::PrivateKeySigner = config.private_key
            .strip_prefix("0x").unwrap_or(&config.private_key)
            .parse()?;
        match crate::timeboost::TimeboostManager::new(
            tb_signer,
            config.timeboost_max_bid_wei,
            &node_provider,
        ).await {
            Ok(tb) => {
                info!("Timeboost express lane ENABLED");
                Some(Arc::new(tb))
            }
            Err(e) => {
                warn!(error = %e, "Timeboost init failed — running without express lane");
                None
            }
        }
    };

    let mut executor = Executor::new(
        node_provider.clone(),
        send_provider,
        wallet,
        config.arb_contract,
        live_mode,
        tg_tx.clone(),
        local_sim,
        timeboost,
    );

    // Set local signer for fast tx signing (no RPC roundtrips)
    {
        let fast_signer: alloy::signers::local::PrivateKeySigner = config.private_key
            .strip_prefix("0x").unwrap_or(&config.private_key)
            .parse()?;
        executor.set_signer(fast_signer);
        info!("Fast local signer enabled (skip eth_chainId RPC)");
    }

    // ─── Dry run logger ───

    // Ensure logs directory exists
    std::fs::create_dir_all("./logs").ok();
    let logger = Arc::new(DryRunLogger::new("./logs/arb_opportunities.jsonl"));

    // ─── Cooldown tracker (avoid spam reverts on same pool pair) ───
    let cooldown = Arc::new(ArbCooldown::new());

    // ─── Swap detection (dual: sequencer feed + event scanner) ───

    let (swap_tx, mut swap_rx) = mpsc::unbounded_channel::<DecodedSwap>();

    // Primary: Sequencer feed — real-time tx decoding (~0ms latency)
    let feed_url = config.sequencer_feed_url.clone();
    let feed_swap_tx = swap_tx.clone();
    tokio::spawn(async move {
        if let Err(e) = crate::feed::start_feed_scanner(&feed_url, feed_swap_tx).await {
            error!(error = %e, "Feed scanner fatal error");
        }
    });

    // Secondary: Event scanner — eth_getLogs polling (backup, catches missed swaps)
    let np = node_provider.clone();
    let ps = pool_state.clone();
    let scanner_swap_tx = swap_tx.clone();
    let sniper_swap_tx = swap_tx.clone();
    tokio::spawn(async move {
        loop {
            if let Err(e) = start_event_scanner(np.clone(), ps.clone(), scanner_swap_tx.clone()).await {
                error!(error = %e, "Scanner error, restarting in 5s...");
                tokio::time::sleep(std::time::Duration::from_secs(5)).await;
            }
        }
    });

    // Pool sniper — detect new pools and arb immediately
    let sniper_np = node_provider.clone();
    let sniper_ps = pool_state.clone();
    tokio::spawn(async move {
        loop {
            if let Err(e) = crate::scanner::sniper::start_pool_sniper(
                sniper_np.clone(), sniper_ps.clone(), sniper_swap_tx.clone()
            ).await {
                error!(error = %e, "Pool sniper error, restarting in 5s...");
                tokio::time::sleep(std::time::Duration::from_secs(5)).await;
            }
        }
    });

    // ─── GMX V2: market indexing + backrun scanner ───

    let gmx_state = crate::gmx::GmxState::new();

    // Index GMX markets once at startup (non-fatal if it fails)
    let gmx_index_np = node_provider.clone();
    let gmx_index_state = gmx_state.clone();
    tokio::spawn(async move {
        if let Err(e) = crate::gmx::index_gmx_markets(&gmx_index_np, &gmx_index_state).await {
            error!(error = %e, "GMX market indexing failed");
        }
    });

    // GMX backrun scanner: DISABLED — needs low latency to backrun keepers
    // Keeping GMX market indexing for future use

    // ─── Stats counters ───

    let swap_count = Arc::new(AtomicU64::new(0));
    let arb_found = Arc::new(AtomicU64::new(0));
    let arb_executed = Arc::new(AtomicU64::new(0));
    let arb_success = Arc::new(AtomicU64::new(0));

    // Stats printer every 60s (with Telegram every 5 min)
    let sc = swap_count.clone();
    let af = arb_found.clone();
    let ae = arb_executed.clone();
    let asuc = arb_success.clone();
    let pc = pool_state.clone();
    let tg_stats = tg_tx.clone();
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(std::time::Duration::from_secs(60));
        let mut tg_counter = 0u32;
        loop {
            interval.tick().await;
            let s = sc.load(Ordering::Relaxed);
            let a = af.load(Ordering::Relaxed);
            let e = ae.load(Ordering::Relaxed);
            let su = asuc.load(Ordering::Relaxed);
            let p = pc.pool_count().await;
            info!("STATS | swaps={s} arbs_found={a} executed={e} success={su} pools={p}");

            // Send to Telegram every 5 min
            tg_counter += 1;
            if tg_counter % 5 == 0 {
                if let Some(ref tx) = tg_stats {
                    let _ = tx.send(TgMsg::Stats {
                        swaps: s, arbs_found: a, executed: e, success: su, pools: p,
                    });
                }
            }
        }
    });

    // ─── Liquidation scanner (every 2s, fast scanning) ───

    if live_mode {
        let liq_mon = liq_monitor.clone();
        let exec = executor.clone();
        let np = node_provider.clone();
        let min_p = config.min_profit_eth;
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(std::time::Duration::from_secs(2));
            loop {
                interval.tick().await;
                let opportunities = liq_mon.scan_positions(&np).await;
                for opp in opportunities {
                    if opp.estimated_profit_eth < min_p {
                        continue;
                    }
                    info!(
                        user = %opp.user,
                        hf = format!("{:.4}", opp.health_factor),
                        profit_eth = format!("{:.6}", opp.estimated_profit_eth),
                        collateral = %opp.collateral_token,
                        debt = %opp.debt_token,
                        ">>> LIQUIDATION OPPORTUNITY"
                    );
                    let exec_clone = exec.clone();
                    tokio::spawn(async move {
                        if let Err(e) = exec_clone.execute_liquidation(&opp).await {
                            error!(error = %e, "Liquidation execution failed");
                        }
                    });
                }
            }
        });
    }

    // ─── Stablecoin depeg scanner (every 5s) ───

    if live_mode {
        let depeg_ps = pool_state.clone();
        let depeg_exec = executor.clone();
        let depeg_cd = cooldown.clone();
        let depeg_af = arb_found.clone();
        let depeg_ae = arb_executed.clone();
        let depeg_as = arb_success.clone();
        let depeg_tg = tg_tx.clone();
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(std::time::Duration::from_secs(5));
            loop {
                interval.tick().await;
                let all_pools: Vec<crate::pools::Pool> = {
                    let pools = depeg_ps.pools.read().await;
                    pools.values().cloned().collect()
                };
                if let Some(opp) = detect_stablecoin_depeg(&all_pools, 0.03) {
                    let buy_addr = opp.buy_pool.address;
                    let sell_addr = opp.sell_pool.address;
                    if depeg_cd.is_cooled_down(buy_addr, sell_addr).await {
                        continue;
                    }
                    let arb_num = depeg_af.fetch_add(1, Ordering::Relaxed) + 1;
                    depeg_ae.fetch_add(1, Ordering::Relaxed);
                    info!(
                        arb_num,
                        profit = format!("{:.6}", opp.expected_profit_eth),
                        buy = ?opp.buy_pool.dex,
                        sell = ?opp.sell_pool.dex,
                        "STABLE DEPEG ARB"
                    );
                    if let Some(ref tx) = depeg_tg {
                        let _ = tx.send(TgMsg::ArbDetected {
                            arb_num,
                            profit_eth: opp.expected_profit_eth,
                            spread_pct: 0.0,
                            buy_dex: format!("{:?}", opp.buy_pool.dex),
                            sell_dex: format!("{:?}", opp.sell_pool.dex),
                            amount_eth: crate::arb::u256_to_f64(opp.optimal_amount_in) / 1e18,
                            is_3hop: false,
                        });
                    }
                    let cd = depeg_cd.clone();
                    let asuc = depeg_as.clone();
                    let exec = depeg_exec.clone();
                    tokio::spawn(async move {
                        match exec.execute_arb(&opp).await {
                            Ok(()) => { asuc.fetch_add(1, Ordering::Relaxed); }
                            Err(e) => {
                                tracing::error!(error = %e, "Depeg arb failed");
                                cd.mark_failed(buy_addr, sell_addr).await;
                            }
                        }
                    });
                }
            }
        });
    }

    info!("Bot active. Scanning for arbitrage...");
    info!("─────────────────────────────────────");

    // ─── Main loop (non-blocking: arb detection + execution in parallel) ───

    while let Some(swap) = swap_rx.recv().await {
        let t0 = std::time::Instant::now();
        let n = swap_count.fetch_add(1, Ordering::Relaxed) + 1;

        if n % 50 == 1 {
            info!(
                dex = ?swap.dex,
                token_in = %swap.token_in,
                token_out = %swap.token_out,
                "SWAP #{n}"
            );
        }

        // ── BACKRUN: update the swapped pool's price from the swap data ──
        // Single write lock — avoids read→write double-lock contention on hot path.
        if swap.pool != Address::ZERO && !swap.amount_in.is_zero() {
            let mut pools_w = pool_state.pools.write().await;
            if let Some(pool) = pools_w.get_mut(&swap.pool) {
                if pool.reserve0 > alloy::primitives::U256::ZERO && pool.reserve1 > alloy::primitives::U256::ZERO {
                    if pool.token0 == swap.token_in {
                        pool.reserve0 += swap.amount_in;
                        if pool.reserve1 > swap.amount_out {
                            pool.reserve1 -= swap.amount_out;
                        }
                    } else if pool.token1 == swap.token_in {
                        pool.reserve1 += swap.amount_in;
                        if pool.reserve0 > swap.amount_out {
                            pool.reserve0 -= swap.amount_out;
                        }
                    }
                }
            }
            // Write lock dropped here — minimizes hold time
        }

        // Fast pool lookup (read lock, non-blocking)
        let pools = pool_state
            .get_pools_for_pair(swap.token_in, swap.token_out)
            .await;
        let t_lookup = t0.elapsed();

        if pools.len() < 2 {
            continue;
        }

        // ── FAST PROBE: swapped pool vs best cross-DEX counterpart ──
        // Max 2 pairs × 2 directions × 2 eth_calls = 8 calls (~112ms)
        // Was: 84 calls (~1200ms) — 10x faster
        if live_mode && pools.len() >= 2 {
            let probe_pools = pools.clone();
            let probe_exec = executor.clone();
            let probe_af = arb_found.clone();
            let probe_ae = arb_executed.clone();
            let probe_as = arb_success.clone();
            let probe_tg = tg_tx.clone();
            let ti = swap.token_in;
            let to_addr = swap.token_out;
            let swap_pool_addr = swap.pool;
            tokio::spawn(async move {
                // Find the swapped pool
                let swapped_idx = probe_pools.iter().position(|p| p.address == swap_pool_addr);
                let swapped = match swapped_idx {
                    Some(i) => &probe_pools[i],
                    None => {
                        if probe_pools.len() < 2 { return; }
                        &probe_pools[0]
                    }
                };

                // Find top 2 cross-DEX counterparts (different DEX or different fee)
                let mut counter = 0u32;
                for other in &probe_pools {
                    if other.address == swapped.address { continue; }
                    if other.dex == swapped.dex && other.fee_bps == swapped.fee_bps { continue; }
                    if counter >= 2 { break; } // max 2 counterparts
                    counter += 1;

                    // Direction 1: buy on swapped (price just moved), sell on other
                    if let Ok(()) = probe_exec.probe_2hop_arb(ti, to_addr, swapped, other).await {
                        probe_af.fetch_add(1, Ordering::Relaxed);
                        probe_ae.fetch_add(1, Ordering::Relaxed);
                        probe_as.fetch_add(1, Ordering::Relaxed);
                        info!(buy = ?swapped.dex, sell = ?other.dex, "$$$ PROBE SUCCESS — tx sent!");
                        if let Some(ref tx) = probe_tg {
                            let _ = tx.send(TgMsg::ArbDetected {
                                arb_num: probe_af.load(Ordering::Relaxed),
                                profit_eth: 0.001, spread_pct: 0.0,
                                buy_dex: format!("{:?}", swapped.dex),
                                sell_dex: format!("{:?}", other.dex),
                                amount_eth: 0.0, is_3hop: false,
                            });
                        }
                        return;
                    }
                    // Direction 2: reverse
                    if let Ok(()) = probe_exec.probe_2hop_arb(ti, to_addr, other, swapped).await {
                        probe_af.fetch_add(1, Ordering::Relaxed);
                        probe_ae.fetch_add(1, Ordering::Relaxed);
                        probe_as.fetch_add(1, Ordering::Relaxed);
                        info!(buy = ?other.dex, sell = ?swapped.dex, "$$$ PROBE SUCCESS (rev) — tx sent!");
                        return;
                    }
                }
            });
        }

        // ── Traditional arb detection (off-chain math, backup) ──
        let min_profit = config.min_profit_eth;
        let log = logger.clone();
        if let Some(opp) = detect_arb(&swap, &pools, min_profit, Some(&log)) {
            let t_detect = t0.elapsed();
            let arb_num = arb_found.fetch_add(1, Ordering::Relaxed) + 1;

            // SEND FIRST, log after — every ms counts
            let profit_eth = opp.expected_profit_eth;
            let buy_dex = format!("{:?}", opp.buy_pool.dex);
            let sell_dex = format!("{:?}", opp.sell_pool.dex);
            let amount_in_f = crate::arb::u256_to_f64(opp.optimal_amount_in) / 1e18;
            let t_spawn = t0.elapsed();
            if live_mode {
                let cd = cooldown.clone();
                let buy_addr = opp.buy_pool.address;
                let sell_addr = opp.sell_pool.address;

                // Check cooldown before executing
                if cd.is_cooled_down(buy_addr, sell_addr).await {
                    debug!("Skipping arb — pool pair on cooldown");
                } else {
                    arb_executed.fetch_add(1, Ordering::Relaxed);
                    let ae_clone = arb_success.clone();
                    let exec = executor.clone();
                    let tg_clone = tg_tx.clone();
                    tokio::spawn(async move {
                        match exec.execute_arb(&opp).await {
                            Ok(()) => {
                                ae_clone.fetch_add(1, Ordering::Relaxed);
                            }
                            Err(e) => {
                                error!(error = %e, "Execution failed");
                                // Mark pair as failed — cooldown for 30s
                                cd.mark_failed(buy_addr, sell_addr).await;
                            }
                        }
                        // Notify Telegram AFTER send (non-blocking)
                        if let Some(ref tx) = tg_clone {
                            let spread = profit_eth * 100.0 / amount_in_f.max(0.001);
                            let _ = tx.send(TgMsg::ArbDetected {
                                arb_num,
                                profit_eth,
                                spread_pct: spread,
                                buy_dex: buy_dex.clone(),
                                sell_dex: sell_dex.clone(),
                                amount_eth: amount_in_f,
                                is_3hop: false,
                            });
                        }
                    });
                }
            }

            info!(
                ">>> ARB #{arb_num} | profit={profit_eth:.6} ETH | {amount_in_f:.4} in | lookup={:.0}us detect={:.0}us spawn={:.0}us",
                t_lookup.as_micros() as f64,
                t_detect.as_micros() as f64,
                t_spawn.as_micros() as f64,
            );
        }

        // ── 3-hop triangular arb detection ──
        // Check every swap for triangular opportunities (A→B→C→A)
        if live_mode {
            let ps = pool_state.clone();
            let swap_clone = swap.clone();
            let min_p = config.min_profit_eth;
            let exec = executor.clone();
            let af = arb_found.clone();
            let ae = arb_executed.clone();
            let asuc = arb_success.clone();
            let tg_3hop = tg_tx.clone();
            let cd_3hop = cooldown.clone();
            tokio::spawn(async move {
                if let Some(tri_arb) = detect_triangular_arb(&swap_clone, &ps, min_p).await {
                    // Cooldown: use first and last pool addresses as key
                    let first_pool = tri_arb.hops.first().map(|h| h.pool.address).unwrap_or_default();
                    let last_pool = tri_arb.hops.last().map(|h| h.pool.address).unwrap_or_default();
                    if cd_3hop.is_cooled_down(first_pool, last_pool).await {
                        return; // skip — this route recently failed
                    }

                    let arb_num = af.fetch_add(1, Ordering::Relaxed) + 1;
                    info!(
                        ">>> 3-HOP ARB #{arb_num} | est_profit={:.6} ETH | {} hops",
                        tri_arb.estimated_profit_eth,
                        tri_arb.hops.len(),
                    );
                    if let Some(ref tx) = tg_3hop {
                        let route = tri_arb.hops.iter()
                            .map(|h| format!("{:?}", h.pool.dex))
                            .collect::<Vec<_>>()
                            .join("→");
                        let _ = tx.send(TgMsg::ArbDetected {
                            arb_num,
                            profit_eth: tri_arb.estimated_profit_eth,
                            spread_pct: 0.0,
                            buy_dex: route,
                            sell_dex: String::new(),
                            amount_eth: crate::arb::u256_to_f64(tri_arb.amount_in) / 1e18,
                            is_3hop: true,
                        });
                    }
                    ae.fetch_add(1, Ordering::Relaxed);
                    match exec.execute_multi_hop(&tri_arb).await {
                        Ok(()) => { asuc.fetch_add(1, Ordering::Relaxed); }
                        Err(e) => {
                            error!(error = %e, "3-hop execution failed");
                            // Cooldown this route for 30s to avoid spam reverts
                            cd_3hop.mark_failed(first_pool, last_pool).await;
                        }
                    }
                }
            });
        }
    }

    Ok(())
}

/// Wait for RPC to become available (handles local node still syncing)
/// Falls back to `fallback_url` after 3 failed attempts on a local node.
async fn wait_for_rpc(url: &str, fallback_url: Option<&str>) -> Result<String> {
    // If it's a remote RPC, return immediately
    if !url.contains("127.0.0.1") && !url.contains("localhost") {
        return Ok(url.to_string());
    }

    info!(url, "Waiting for local node...");

    let max_retries = 3;
    for attempt in 1..=max_retries {
        match ProviderBuilder::new()
            .connect_http(url.parse()?)
            .get_chain_id()
            .await
        {
            Ok(chain_id) => {
                info!(chain_id, "Local node ready!");
                return Ok(url.to_string());
            }
            Err(_) => {
                info!(attempt, max_retries, "Node not ready, retrying in 30s...");
                tokio::time::sleep(std::time::Duration::from_secs(30)).await;
            }
        }
    }

    // Local node failed — try fallback
    if let Some(fb) = fallback_url {
        warn!("Local node unreachable after {max_retries} attempts, switching to fallback RPC");
        match ProviderBuilder::new()
            .connect_http(fb.parse()?)
            .get_chain_id()
            .await
        {
            Ok(chain_id) => {
                info!(chain_id, rpc = fb, "Fallback RPC ready");
                return Ok(fb.to_string());
            }
            Err(e) => {
                error!(error = %e, "Fallback RPC also failed");
            }
        }
    }

    // No fallback or fallback failed — keep retrying local node forever
    warn!("Falling back to infinite retry on local node");
    loop {
        match ProviderBuilder::new()
            .connect_http(url.parse()?)
            .get_chain_id()
            .await
        {
            Ok(chain_id) => {
                info!(chain_id, "Local node ready!");
                return Ok(url.to_string());
            }
            Err(_) => {
                info!("Node not ready, retrying in 30s...");
                tokio::time::sleep(std::time::Duration::from_secs(30)).await;
            }
        }
    }
}
