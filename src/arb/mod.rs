use alloy::primitives::{address, Address, U256};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::Mutex;
use tracing::{debug, info, warn};

use crate::decoder::{DecodedSwap, DexType};
use crate::dryrun::{DryRunLogger, SimResult};
use crate::pools::{Pool, PoolState};

/// Spreads above this % are stale data — skip as anomaly
const MAX_SANE_SPREAD_PCT: f64 = 5.0;

/// Cooldown duration after a failed arb on a pool pair (avoid spam reverts)
const COOLDOWN_SECS: u64 = 30;

/// Tracks pool pairs that recently failed to avoid repeated gas waste
pub struct ArbCooldown {
    pairs: Mutex<HashMap<(Address, Address), Instant>>,
}

impl ArbCooldown {
    pub fn new() -> Self {
        Self {
            pairs: Mutex::new(HashMap::new()),
        }
    }

    /// Check if this pool pair is on cooldown
    pub async fn is_cooled_down(&self, pool_a: Address, pool_b: Address) -> bool {
        let key = if pool_a < pool_b { (pool_a, pool_b) } else { (pool_b, pool_a) };
        let map = self.pairs.lock().await;
        if let Some(last_fail) = map.get(&key) {
            last_fail.elapsed().as_secs() < COOLDOWN_SECS
        } else {
            false
        }
    }

    /// Mark a pool pair as failed (starts cooldown)
    pub async fn mark_failed(&self, pool_a: Address, pool_b: Address) {
        let key = if pool_a < pool_b { (pool_a, pool_b) } else { (pool_b, pool_a) };
        let mut map = self.pairs.lock().await;
        map.insert(key, Instant::now());
        // Prune old entries (keep map small)
        map.retain(|_, t| t.elapsed().as_secs() < COOLDOWN_SECS * 2);
    }
}

/// Max arb input (1 ETH) — safety cap
const MAX_ARB_WEI: f64 = 1_000_000_000_000_000_000.0;
/// Min arb input (0.01 ETH)
const MIN_ARB_WEI: f64 = 10_000_000_000_000_000.0;

/// Min test amount for 3-hop arbs (0.005 ETH)
const MIN_AMOUNT_3HOP: f64 = 5_000_000_000_000_000.0;
/// Max test amount for 3-hop arbs (0.5 ETH)
const MAX_AMOUNT_3HOP: f64 = 500_000_000_000_000_000.0;

/// Represents an arbitrage opportunity
#[derive(Debug, Clone)]
pub struct ArbOpportunity {
    pub trigger_swap: DecodedSwap,
    /// Pool to buy from (lower price after trigger swap)
    pub buy_pool: Pool,
    /// Pool to sell to (higher price)
    pub sell_pool: Pool,
    /// Optimal input amount for the arb
    pub optimal_amount_in: U256,
    /// Expected profit in token terms
    pub expected_profit: U256,
    /// Expected profit in ETH (for threshold comparison)
    pub expected_profit_eth: f64,
    /// V3 fee tier for buy pool (0 if V2)
    pub buy_fee: u32,
    /// V3 fee tier for sell pool (0 if V2)
    pub sell_fee: u32,
}

/// High-competition: only UniswapV3 ↔ UniswapV3 (same DEX, co-located bots dominate)
/// PancakeSwap and SushiSwap V3 have LESS bot competition — allow them
fn is_high_competition(pool: &Pool) -> bool {
    matches!(pool.dex, DexType::UniswapV3)
}

/// Low-competition pools — arbs involving these persist longer
fn is_low_competition(pool: &Pool) -> bool {
    matches!(pool.dex, DexType::CamelotV2 | DexType::CamelotV3 | DexType::RamsesV2
        | DexType::CurveStable | DexType::BalancerStable | DexType::UniswapV2 | DexType::SushiSwapV2)
}

/// Detects cross-DEX arbitrage opportunities from a decoded swap.
/// Filters for low-competition arbs that don't need sub-100ms latency.
pub fn detect_arb(
    swap: &DecodedSwap,
    pools: &[Pool],
    min_profit_eth: f64,
    logger: Option<&Arc<DryRunLogger>>,
) -> Option<ArbOpportunity> {
    let relevant_pools: Vec<&Pool> = pools
        .iter()
        .filter(|p| {
            (p.token0 == swap.token_in && p.token1 == swap.token_out)
                || (p.token0 == swap.token_out && p.token1 == swap.token_in)
        })
        .collect();

    if relevant_pools.len() < 2 {
        return None;
    }

    let mut best_opp: Option<ArbOpportunity> = None;

    for i in 0..relevant_pools.len() {
        for j in (i + 1)..relevant_pools.len() {
            let pool_a = relevant_pools[i];
            let pool_b = relevant_pools[j];

            // Skip same-DEX pools — cross-DEX only for real arb margins
            if pool_a.dex == pool_b.dex {
                continue;
            }

            // HIGH-LATENCY FILTER: skip arbs where BOTH pools are popular V3
            // These are captured by co-located bots in <20ms — we can't compete
            // Only take arbs where at least ONE pool is low-competition
            if is_high_competition(pool_a) && is_high_competition(pool_b) {
                continue;
            }

            for (buy, sell) in [(pool_a, pool_b), (pool_b, pool_a)] {
                if let Some(opp) = check_pair_arb(swap, buy, sell, min_profit_eth, logger) {
                    if best_opp
                        .as_ref()
                        .map_or(true, |best| opp.expected_profit_eth > best.expected_profit_eth)
                    {
                        best_opp = Some(opp);
                    }
                }
            }
        }
    }

    if let Some(ref opp) = best_opp {
        info!(
            profit_eth = opp.expected_profit_eth,
            buy_dex = ?opp.buy_pool.dex,
            sell_dex = ?opp.sell_pool.dex,
            token_in = ?opp.trigger_swap.token_in,
            token_out = ?opp.trigger_swap.token_out,
            amount_in = ?opp.optimal_amount_in,
            "Arb opportunity detected!"
        );
    }

    best_opp
}

fn check_pair_arb(
    swap: &DecodedSwap,
    buy_pool: &Pool,
    sell_pool: &Pool,
    min_profit_eth: f64,
    logger: Option<&Arc<DryRunLogger>>,
) -> Option<ArbOpportunity> {
    // Skip ghost pools (fee=0 means pool was auto-discovered with bad data)
    if buy_pool.fee_bps == 0 || sell_pool.fee_bps == 0 {
        return None;
    }

    // Skip pools with insufficient liquidity
    for pool in [buy_pool, sell_pool] {
        if is_v3(pool.dex) {
            if let Some(liq) = pool.liquidity {
                if liq < 1_000_000_000_000_000 { return None; } // 1e15 min
            }
        }
        if is_v2(pool.dex) || is_curve(pool.dex) {
            // BOTH reserves must be meaningful (not just one)
            // Tokens with 6 decimals (USDC/USDT): 1e6 = $1
            // Tokens with 18 decimals (WETH/ARB): 1e15 = 0.001
            let min_reserve = U256::from(100_000u64); // $0.10 for 6-decimal tokens
            if pool.reserve0 < min_reserve || pool.reserve1 < min_reserve {
                return None;
            }
            // Also check that neither reserve is absurdly small relative to the other
            // (ratio > 1,000,000:1 means one side is essentially empty)
            let r0 = u256_to_f64(pool.reserve0);
            let r1 = u256_to_f64(pool.reserve1);
            if r0 > 0.0 && r1 > 0.0 {
                let ratio = if r0 > r1 { r0 / r1 } else { r1 / r0 };
                if ratio > 1_000_000.0 {
                    return None; // one-sided pool
                }
            }
        }
    }

    let price_buy = get_effective_price(buy_pool, &swap.token_in)?;
    let price_sell = get_effective_price(sell_pool, &swap.token_in)?;

    // Sanity check: verify V3 pool prices are in a reasonable range
    // If price ratio between pools > 2x, one pool has stale/broken pricing
    if price_buy > 0.0 && price_sell > 0.0 {
        let ratio = if price_buy > price_sell { price_buy / price_sell } else { price_sell / price_buy };
        if ratio > 2.0 {
            return None; // pools pricing wildly different = stale data
        }
    }

    let spread_pct = ((price_sell - price_buy) / price_buy) * 100.0;

    if price_buy >= price_sell {
        if let Some(log) = logger {
            log.log_spread(swap.dex, swap.token_in, swap.token_out, buy_pool, sell_pool, price_buy, price_sell, spread_pct, None);
        }
        return None;
    }

    // Filter: spreads > 5% are almost certainly stale reserves
    if spread_pct > MAX_SANE_SPREAD_PCT {
        debug!(
            spread = format!("{:.2}%", spread_pct),
            buy_dex = ?buy_pool.dex,
            sell_dex = ?sell_pool.dex,
            "Anomalous spread — stale data, skipping"
        );
        return None;
    }

    // ── Check net spread is positive after fees ──

    // Use directional fee if available (Camelot V2)
    let buy_fee_pct = if buy_pool.dex == DexType::CamelotV2 {
        // When buying tokenOut on buy_pool, we're selling tokenIn
        if buy_pool.token0 == swap.token_in {
            buy_pool.fee_bps_token0.unwrap_or(buy_pool.fee_bps) as f64 / 10000.0
        } else {
            buy_pool.fee_bps_token1.unwrap_or(buy_pool.fee_bps) as f64 / 10000.0
        }
    } else {
        buy_pool.fee_bps as f64 / 10000.0
    };
    let sell_fee_pct = if sell_pool.dex == DexType::CamelotV2 {
        // When selling tokenOut on sell_pool, we're selling tokenOut (received from buy)
        if sell_pool.token0 == swap.token_out {
            sell_pool.fee_bps_token0.unwrap_or(sell_pool.fee_bps) as f64 / 10000.0
        } else {
            sell_pool.fee_bps_token1.unwrap_or(sell_pool.fee_bps) as f64 / 10000.0
        }
    } else {
        sell_pool.fee_bps as f64 / 10000.0
    };
    let net_spread = spread_pct / 100.0 - buy_fee_pct - sell_fee_pct;

    if net_spread <= 0.001 {
        // Need at least 0.1% net spread to cover price impact
        return None;
    }

    // ── Compute optimal amount dynamically ──
    // For V2: sqrt(reserveIn * reserveOut * fee_factor) - reserveIn
    // For V3/mixed: fraction of liquidity scaled by spread
    // Cap by pool reserves and global max

    // Curve and V2 both use reserves for sizing
    let buy_is_reserve = is_v2(buy_pool.dex) || is_curve(buy_pool.dex);
    let sell_is_reserve = is_v2(sell_pool.dex) || is_curve(sell_pool.dex);

    let optimal_in_f = if buy_is_reserve && sell_is_reserve {
        // V2↔V2 or Curve↔V2/Curve: quadratic formula for optimal input
        compute_optimal_v2(buy_pool, sell_pool, &swap.token_in).unwrap_or(MIN_ARB_WEI)
    } else if buy_is_reserve || sell_is_reserve {
        // Mixed reserve↔V3: use reserve pool as depth guide
        let res_pool = if buy_is_reserve { buy_pool } else { sell_pool };
        if let Some((r_in, _)) = get_reserves_ordered(res_pool, &swap.token_in) {
            let r = u256_to_f64(r_in);
            // Trade ~0.2% of reserve, scaled by spread
            (r * 0.002 * net_spread.min(0.05) * 20.0).min(MAX_ARB_WEI).max(MIN_ARB_WEI)
        } else {
            MIN_ARB_WEI
        }
    } else {
        // V3↔V3: use spread to scale amount
        // Smaller spread → smaller amount (less price impact needed)
        (net_spread * 5.0 * 1e18).min(MAX_ARB_WEI).max(MIN_ARB_WEI)
    };

    // Apply V2 reserve cap
    let v2_cap = get_v2_reserve_cap(buy_pool, sell_pool, &swap.token_in);
    let capped = if let Some(cap) = v2_cap {
        optimal_in_f.min(cap).max(MIN_ARB_WEI)
    } else {
        optimal_in_f
    };

    let optimal_in = U256::from(capped as u128);

    // ── REAL SIMULATION: validate profit with actual swap math ──
    // Instead of theoretical spread*amount, simulate both legs
    let sim_buy = simulate_swap(buy_pool, &swap.token_in, &optimal_in);
    let sim_sell = sim_buy.and_then(|bought| simulate_swap(sell_pool, &swap.token_out, &bought));

    let (profit, profit_eth, sim_bought, sim_sold) = match (sim_buy, sim_sell) {
        (Some(bought), Some(sold)) => {
            if sold > optimal_in {
                let p = sold - optimal_in;
                let p_eth = u256_to_f64(p) / 1e18;
                (p, p_eth, bought, sold)
            } else {
                // Simulation shows NO profit after price impact — skip
                if let Some(log) = logger {
                    log.log_spread(
                        swap.dex, swap.token_in, swap.token_out, buy_pool, sell_pool,
                        price_buy, price_sell, spread_pct,
                        Some(SimResult {
                            optimal_input: optimal_in,
                            bought,
                            sold,
                            profit: U256::ZERO,
                            profit_eth: 0.0,
                        }),
                    );
                }
                return None;
            }
        }
        _ => {
            // Simulation failed (missing reserves/price) — skip
            if let Some(log) = logger {
                log.log_spread(
                    swap.dex, swap.token_in, swap.token_out, buy_pool, sell_pool,
                    price_buy, price_sell, spread_pct, None,
                );
            }
            return None;
        }
    };

    // Gas on Arbitrum ~$0.025 per tx (including reverts)
    let gas_cost_eth = 0.000012;
    let profit_after_gas = profit_eth - gas_cost_eth;

    // Log to JSONL with real simulation data
    if let Some(log) = logger {
        log.log_spread(
            swap.dex, swap.token_in, swap.token_out, buy_pool, sell_pool,
            price_buy, price_sell, spread_pct,
            Some(SimResult {
                optimal_input: optimal_in,
                bought: sim_bought,
                sold: sim_sold,
                profit,
                profit_eth,
            }),
        );
    }

    if profit_after_gas < min_profit_eth {
        return None;
    }

    info!(
        spread = format!("{:.4}%", spread_pct),
        net_spread = format!("{:.4}%", net_spread * 100.0),
        est_profit_eth = format!("{:.8}", profit_after_gas),
        optimal_in = %optimal_in,
        buy_dex = ?buy_pool.dex,
        sell_dex = ?sell_pool.dex,
        "Arb candidate → eth_call"
    );

    // fee_bps (e.g. 30 = 0.3%) → Uniswap V3 fee units (e.g. 3000)
    let buy_fee = if is_v3(buy_pool.dex) { buy_pool.fee_bps * 100 } else { 0 };
    let sell_fee = if is_v3(sell_pool.dex) { sell_pool.fee_bps * 100 } else { 0 };

    Some(ArbOpportunity {
        trigger_swap: swap.clone(),
        buy_pool: buy_pool.clone(),
        sell_pool: sell_pool.clone(),
        optimal_amount_in: optimal_in,
        expected_profit: profit,
        expected_profit_eth: profit_after_gas,
        buy_fee,
        sell_fee,
    })
}

fn is_v2(dex: DexType) -> bool {
    matches!(dex, DexType::UniswapV2 | DexType::CamelotV2 | DexType::SushiSwapV2 | DexType::RamsesV2)
}

fn is_v3(dex: DexType) -> bool {
    matches!(dex, DexType::UniswapV3 | DexType::CamelotV3 | DexType::SushiSwapV3 | DexType::PancakeSwapV3)
}

fn is_curve(dex: DexType) -> bool {
    matches!(dex, DexType::CurveStable | DexType::BalancerStable)
}

/// Get effective price for token_in → token_out from a pool
fn get_effective_price(pool: &Pool, token_in: &Address) -> Option<f64> {
    if is_curve(pool.dex) {
        // Curve stable pools: approximate price via reserve ratio.
        // Stable pools target 1:1, so reserve ratio is a good proxy.
        // Actual execution uses get_dy() on-chain — this is only for spread detection.
        let (r_in, r_out) = if pool.token0 == *token_in {
            (pool.reserve0, pool.reserve1)
        } else {
            (pool.reserve1, pool.reserve0)
        };
        if r_in.is_zero() { return None; }
        Some(u256_to_f64(r_out) / u256_to_f64(r_in))
    } else if let Some(sqrt_price) = pool.sqrt_price_x96 {
        // V3: price = (sqrtPriceX96 / 2^96)^2
        let sq = u256_to_f64(sqrt_price);
        let price = (sq / (2.0_f64.powi(96))).powi(2);
        if pool.token0 == *token_in {
            Some(price)
        } else {
            if price == 0.0 { None } else { Some(1.0 / price) }
        }
    } else {
        // V2: price = reserve_out / reserve_in
        let (r_in, r_out) = if pool.token0 == *token_in {
            (pool.reserve0, pool.reserve1)
        } else {
            (pool.reserve1, pool.reserve0)
        };
        if r_in.is_zero() { None } else { Some(u256_to_f64(r_out) / u256_to_f64(r_in)) }
    }
}

/// Compute optimal input for V2↔V2 arb using quadratic formula
fn compute_optimal_v2(buy_pool: &Pool, sell_pool: &Pool, token_in: &Address) -> Option<f64> {
    let (ra_in, ra_out) = get_reserves_ordered(buy_pool, token_in)?;
    let (_, rb_in) = get_reserves_ordered(sell_pool, token_in)?;

    let ra_in_f = u256_to_f64(ra_in);
    let ra_out_f = u256_to_f64(ra_out);
    let rb_in_f = u256_to_f64(rb_in);
    if ra_in_f < 1e15 || rb_in_f < 1e15 { return None; }

    let rb_out_f = u256_to_f64(get_reserves_ordered(sell_pool, token_in)?.1);
    let fee_a = (10000 - buy_pool.fee_bps) as f64;
    let fee_b = (10000 - sell_pool.fee_bps) as f64;

    let num = (ra_in_f * ra_out_f * rb_in_f * rb_out_f * fee_a * fee_b).sqrt();
    let optimal = (num / 10000.0) - ra_in_f;
    if optimal <= 0.0 { return None; }

    // Cap at 0.5% of smaller reserve and global max
    let reserve_cap = ra_in_f.min(rb_in_f) * 0.005;
    Some(optimal.min(reserve_cap).min(MAX_ARB_WEI).max(MIN_ARB_WEI))
}

/// Get the max amount we can trade on V2/Curve pools (0.5% of smaller reserve)
fn get_v2_reserve_cap(buy_pool: &Pool, sell_pool: &Pool, token_in: &Address) -> Option<f64> {
    let mut caps = Vec::new();
    for pool in [buy_pool, sell_pool] {
        if is_v2(pool.dex) || is_curve(pool.dex) {
            if let Some((r_in, _)) = get_reserves_ordered(pool, token_in) {
                let r = u256_to_f64(r_in);
                if r < 1e15 { return Some(0.0); } // pool too small
                caps.push(r * 0.005);
            }
        }
    }
    caps.into_iter().reduce(f64::min)
}

fn simulate_swap(pool: &Pool, token_in: &Address, amount_in: &U256) -> Option<U256> {
    if is_curve(pool.dex) {
        // Approximate Curve swap via V2-style constant-product formula.
        // Real execution uses get_dy() on-chain, so small errors here are fine.
        simulate_v2_swap(pool, token_in, amount_in)
    } else if is_v2(pool.dex) {
        simulate_v2_swap(pool, token_in, amount_in)
    } else {
        simulate_v3_swap(pool, token_in, amount_in)
    }
}

fn simulate_v2_swap(pool: &Pool, token_in: &Address, amount_in: &U256) -> Option<U256> {
    let (reserve_in, reserve_out) = get_reserves_ordered(pool, token_in)?;
    if reserve_in.is_zero() || reserve_out.is_zero() {
        return None;
    }

    // Use directional fees for CamelotV2 (denominator 100000, not 10000)
    if pool.dex == DexType::CamelotV2 {
        // Camelot fees: fee_bps_token0 = fee when selling token0
        let fee_pct = if pool.token0 == *token_in {
            pool.fee_bps_token0.unwrap_or(pool.fee_bps) as u64
        } else {
            pool.fee_bps_token1.unwrap_or(pool.fee_bps) as u64
        };
        // Camelot fee denominator is 100000 (e.g. fee_pct=300 means 0.3%)
        // But our fee_bps are already in basis points (30 = 0.3%), stored as /1000 from raw
        // Use standard 10000 denominator with our bps value
        let fee_factor = 10000u64.saturating_sub(fee_pct);
        let amount_in_with_fee = *amount_in * U256::from(fee_factor);
        let numerator = amount_in_with_fee * reserve_out;
        let denominator = reserve_in * U256::from(10000u64) + amount_in_with_fee;
        if denominator.is_zero() { None } else { Some(numerator / denominator) }
    } else {
        let fee = (10000 - pool.fee_bps) as u64;
        let amount_in_with_fee = *amount_in * U256::from(fee);
        let numerator = amount_in_with_fee * reserve_out;
        let denominator = reserve_in * U256::from(10000u64) + amount_in_with_fee;
        if denominator.is_zero() { None } else { Some(numerator / denominator) }
    }
}

fn simulate_v3_swap(pool: &Pool, token_in: &Address, amount_in: &U256) -> Option<U256> {
    let sqrt_price_x96 = pool.sqrt_price_x96?;
    let liquidity = pool.liquidity? as f64;
    if liquidity == 0.0 { return None; }

    let amount_in_f = u256_to_f64(*amount_in);
    if amount_in_f == 0.0 { return None; }

    // Apply fee first (Uniswap V3 takes fee from input)
    let fee_ppm = (pool.fee_bps as f64) * 100.0; // bps*100 = ppm (e.g. 30bps -> 3000ppm)
    let amount_in_after_fee = amount_in_f * (1_000_000.0 - fee_ppm) / 1_000_000.0;

    let sqrt_p = u256_to_f64(sqrt_price_x96);
    let q96 = 2.0_f64.powi(96);
    let sqrt_price = sqrt_p / q96;
    if sqrt_price == 0.0 { return None; }

    // V3 constant-liquidity swap math (within single tick range):
    // For token0 -> token1 (zeroForOne):
    //   new_sqrt_price = L * sqrt_price / (L + amount_in * sqrt_price)
    //   amount_out = L * (sqrt_price - new_sqrt_price)
    // For token1 -> token0 (oneForZero):
    //   new_sqrt_price = sqrt_price + amount_in / L
    //   amount_out = L * (1/new_sqrt_price - 1/sqrt_price)  ... simplified:
    //   amount_out = L * (sqrt_price_new - sqrt_price_old) ... for 1/price domain
    let amount_out = if pool.token0 == *token_in {
        // zeroForOne: selling token0, getting token1
        let new_sqrt_price = liquidity * sqrt_price / (liquidity + amount_in_after_fee * sqrt_price);
        if new_sqrt_price <= 0.0 { return None; }
        liquidity * (sqrt_price - new_sqrt_price)
    } else {
        // oneForZero: selling token1, getting token0
        let new_sqrt_price = sqrt_price + amount_in_after_fee / liquidity;
        if new_sqrt_price <= 0.0 { return None; }
        // amount_out_token0 = L * (1/old_sqrt - 1/new_sqrt)
        liquidity * (1.0 / sqrt_price - 1.0 / new_sqrt_price)
    };

    if amount_out < 1.0 { return None; }
    Some(U256::from(amount_out as u128))
}

/// Check if a pool has enough liquidity to be viable for arb
fn is_pool_viable(pool: &Pool) -> bool {
    if pool.fee_bps == 0 { return false; }
    if is_v2(pool.dex) || is_curve(pool.dex) {
        let min = U256::from(100_000u64);
        if pool.reserve0 < min || pool.reserve1 < min { return false; }
        let r0 = u256_to_f64(pool.reserve0);
        let r1 = u256_to_f64(pool.reserve1);
        if r0 > 0.0 && r1 > 0.0 && (r0 / r1 > 1_000_000.0 || r1 / r0 > 1_000_000.0) {
            return false;
        }
    }
    if is_v3(pool.dex) {
        if let Some(liq) = pool.liquidity {
            if liq < 1_000_000_000_000_000 { return false; }
        }
    }
    true
}

fn get_reserves_ordered(pool: &Pool, token_in: &Address) -> Option<(U256, U256)> {
    if pool.token0 == *token_in {
        Some((pool.reserve0, pool.reserve1))
    } else if pool.token1 == *token_in {
        Some((pool.reserve1, pool.reserve0))
    } else {
        None
    }
}

pub fn u256_to_f64(v: U256) -> f64 {
    v.to_string().parse::<f64>().unwrap_or(0.0)
}

// ═══════════════════════════════════════════════════════════════
//  STABLECOIN DEPEG DETECTION
// ═══════════════════════════════════════════════════════════════

/// Check stablecoin pool prices for depeg opportunities.
/// Compares Curve stable pool vs Uniswap V3 USDC/USDT 0.01% pool.
/// If spread > min_spread_pct (e.g. 0.05%), returns an arb opportunity.
pub fn detect_stablecoin_depeg(
    pool_state: &[Pool],
    min_spread_pct: f64,
) -> Option<ArbOpportunity> {
    // Stablecoin token addresses on Arbitrum
    let usdc = address!("af88d065e77c8cC2239327C5EDb3A432268e5831");
    let usdt = address!("Fd086bC7CD5C481DCC9C85ebE478A1C0b69FCbb9");

    // Find Curve stable pools for USDC/USDT
    let curve_pools: Vec<&Pool> = pool_state.iter()
        .filter(|p| p.dex == DexType::CurveStable
            && ((p.token0 == usdc && p.token1 == usdt)
                || (p.token0 == usdt && p.token1 == usdc)))
        .collect();

    // Find Uniswap V3 USDC/USDT 0.01% pool (fee_bps = 1)
    let v3_pools: Vec<&Pool> = pool_state.iter()
        .filter(|p| is_v3(p.dex)
            && ((p.token0 == usdc && p.token1 == usdt)
                || (p.token0 == usdt && p.token1 == usdc)))
        .collect();

    if curve_pools.is_empty() || v3_pools.is_empty() {
        return None;
    }

    let mut best: Option<ArbOpportunity> = None;

    for curve_pool in &curve_pools {
        for v3_pool in &v3_pools {
            // Price from each pool: USDC per USDT (sell USDT, get USDC)
            let price_curve = match get_effective_price(curve_pool, &usdt) {
                Some(p) if p > 0.0 => p,
                _ => continue,
            };
            let price_v3 = match get_effective_price(v3_pool, &usdt) {
                Some(p) if p > 0.0 => p,
                _ => continue,
            };

            for (buy_pool, sell_pool, buy_price, sell_price) in [
                (*curve_pool, *v3_pool, price_curve, price_v3),
                (*v3_pool, *curve_pool, price_v3, price_curve),
            ] {
                if buy_price >= sell_price {
                    continue;
                }

                let spread_pct = ((sell_price - buy_price) / buy_price) * 100.0;
                if spread_pct < min_spread_pct {
                    continue;
                }

                let buy_fee_pct = buy_pool.fee_bps as f64 / 10000.0;
                let sell_fee_pct = sell_pool.fee_bps as f64 / 10000.0;
                let net_spread = spread_pct / 100.0 - buy_fee_pct - sell_fee_pct;
                if net_spread <= 0.0 {
                    continue;
                }

                // Size the trade conservatively: 0.2% of smaller reserve
                let trade_size = if is_curve(buy_pool.dex) || is_v2(buy_pool.dex) {
                    let r = u256_to_f64(buy_pool.reserve0).min(u256_to_f64(buy_pool.reserve1));
                    (r * 0.002).min(MAX_ARB_WEI).max(MIN_ARB_WEI)
                } else {
                    MIN_ARB_WEI * 10.0
                };

                let profit_raw = trade_size * net_spread;
                let profit_eth = profit_raw / 1e18;
                let profit_after_gas = profit_eth - 0.000012;

                if profit_after_gas <= 0.0 {
                    continue;
                }

                let dummy_swap = DecodedSwap {
                    dex: buy_pool.dex,
                    pool: buy_pool.address,
                    token_in: usdt,
                    token_out: usdc,
                    amount_in: U256::from(trade_size as u128),
                    amount_out: U256::ZERO,
                    sender: Address::ZERO,
                };

                let opp = ArbOpportunity {
                    trigger_swap: dummy_swap,
                    buy_pool: buy_pool.clone(),
                    sell_pool: sell_pool.clone(),
                    optimal_amount_in: U256::from(trade_size as u128),
                    expected_profit: U256::from((profit_raw) as u128),
                    expected_profit_eth: profit_after_gas,
                    buy_fee: if is_v3(buy_pool.dex) { buy_pool.fee_bps * 100 } else { 0 },
                    sell_fee: if is_v3(sell_pool.dex) { sell_pool.fee_bps * 100 } else { 0 },
                };

                if best.as_ref().map_or(true, |b: &ArbOpportunity| profit_after_gas > b.expected_profit_eth) {
                    info!(
                        spread = format!("{:.4}%", spread_pct),
                        net_spread = format!("{:.4}%", net_spread * 100.0),
                        buy_dex = ?buy_pool.dex,
                        sell_dex = ?sell_pool.dex,
                        "Stablecoin depeg detected!"
                    );
                    best = Some(opp);
                }
            }
        }
    }

    best
}

// ═══════════════════════════════════════════════════════════════
//  CIRCULAR ARBITRAGE (3-hop + 4-hop triangular/quadrilateral)
// ═══════════════════════════════════════════════════════════════

/// Hub tokens for circular arb — most connected, highest liquidity
const HUB_TOKENS: [Address; 15] = [
    address!("82aF49447D8a07e3bd95BD0d56f35241523fBab1"), // WETH
    address!("af88d065e77c8cC2239327C5EDb3A432268e5831"), // USDC
    address!("FF970A61A04b1cA14834A43f5dE4533eBDDB5CC8"), // USDC.e
    address!("Fd086bC7CD5C481DCC9C85ebE478A1C0b69FCbb9"), // USDT
    address!("912CE59144191C1204E64559FE8253a0e49E6548"), // ARB
    address!("2f2a2543B76A4166549F7aaB2e75Bef0aefC5B0f"), // WBTC
    address!("fc5A1A6EB076a2C7aD06eD22C90d7E710E35ad0a"), // GMX
    address!("f97f4df75117a78c1A5a0DBb814Af92458539FB4"), // LINK
    address!("0c880f6761F1af8d9Aa9C466984b80DAb9a8c9e8"), // PENDLE
    address!("539bdE0d7Dbd336b79148AA742883198BBF60342"), // MAGIC
    address!("3082CC23568eA640225c2467653dB90e9250AaA0"), // RDNT
    address!("DA10009cBd5D07dd0CeCc66161FC93D7c9000da1"), // DAI
    address!("25118290e6a5f4139381d072181157035864099d"), // RAIN
    address!("11920f139a3121c2836e01551d43f95b3c31159c"), // YBR
    address!("60bf4e7cf16ff34513514b968483b54beff42a81"), // VCNT
];

#[derive(Debug, Clone)]
pub struct ArbHop {
    pub pool: Pool,
    pub token_in: Address,
    pub token_out: Address,
}

#[derive(Debug, Clone)]
pub struct MultiHopArb {
    pub hops: Vec<ArbHop>,
    pub flash_token: Address,
    pub amount_in: U256,
    pub estimated_profit_eth: f64,
}

/// Tokens that Balancer V2 has for flash loans (verified on Arbitrum)
fn balancer_has_token(token: &Address) -> bool {
    *token == address!("82aF49447D8a07e3bd95BD0d56f35241523fBab1") || // WETH
    *token == address!("af88d065e77c8cC2239327C5EDb3A432268e5831") || // USDC
    *token == address!("FF970A61A04b1cA14834A43f5dE4533eBDDB5CC8") || // USDC.e
    *token == address!("Fd086bC7CD5C481DCC9C85ebE478A1C0b69FCbb9") || // USDT
    *token == address!("2f2a2543B76A4166549F7aaB2e75Bef0aefC5B0f") || // WBTC
    *token == address!("912CE59144191C1204E64559FE8253a0e49E6548") || // ARB
    *token == address!("DA10009cBd5D07dd0CeCc66161FC93D7c9000da1")    // DAI
}

/// Detect circular arb: triggered by a swap on (A, B).
/// Checks 3-hop (A→B→C→A) and 4-hop (A→B→C→D→A) cycles.
/// Tests ALL pool combinations per leg (cross-DEX), not just cheapest fee.
pub async fn detect_triangular_arb(
    swap: &DecodedSwap,
    pool_state: &PoolState,
    min_profit_eth: f64,
) -> Option<MultiHopArb> {
    let token_a = swap.token_in;
    let token_b = swap.token_out;

    // Flash loan source: Balancer (0% fee) preferred, Aave (0.05% fee) as fallback

    let mut best: Option<MultiHopArb> = None;

    // ── 3-hop: A → B → C → A ──
    // Check neighbors of BOTH A and B for potential intermediary C
    let neighbors_b = pool_state.get_neighbors(token_b).await;
    let neighbors_a = pool_state.get_neighbors(token_a).await;

    // C candidates: tokens reachable from B that can also reach A
    let c_candidates: Vec<Address> = neighbors_b.iter()
        .filter(|c| **c != token_a && **c != token_b && neighbors_a.contains(*c))
        .copied()
        .collect();

    for token_c in &c_candidates {
        if let Some(arb) = try_3hop_cycle(
            token_a, token_b, *token_c, pool_state, min_profit_eth
        ).await {
            if best.as_ref().map_or(true, |b| arb.estimated_profit_eth > b.estimated_profit_eth) {
                best = Some(arb);
            }
        }
    }

    // ── 4-hop: A → B → C → D → A (via hub tokens) ──
    // Only check hub tokens as C and D to limit search space
    for &token_c in &HUB_TOKENS {
        if token_c == token_a || token_c == token_b { continue; }

        let pools_bc = pool_state.get_pools_for_pair(token_b, token_c).await;
        if pools_bc.is_empty() { continue; }

        for &token_d in &HUB_TOKENS {
            if token_d == token_a || token_d == token_b || token_d == token_c { continue; }

            let pools_cd = pool_state.get_pools_for_pair(token_c, token_d).await;
            let pools_da = pool_state.get_pools_for_pair(token_d, token_a).await;
            if pools_cd.is_empty() || pools_da.is_empty() { continue; }

            if let Some(arb) = try_4hop_cycle(
                token_a, token_b, token_c, token_d, pool_state, min_profit_eth
            ).await {
                if best.as_ref().map_or(true, |b| arb.estimated_profit_eth > b.estimated_profit_eth) {
                    best = Some(arb);
                }
            }
        }
    }

    best
}

/// Simulate a full multi-hop route: returns final amount_out for a given amount_in
fn simulate_multi_hop(hops: &[(& Pool, Address, Address)], amount_in_f: f64) -> Option<f64> {
    let mut current = amount_in_f;
    for (pool, token_in, _token_out) in hops {
        let amt = U256::from(current as u128);
        let out = simulate_swap(pool, token_in, &amt)?;
        current = u256_to_f64(out);
        if current <= 0.0 { return None; }
    }
    Some(current)
}

/// Find optimal input for multi-hop arb via binary search on profit
fn find_optimal_multihop_amount(hops: &[(&Pool, Address, Address)]) -> Option<(f64, f64)> {
    // Quick check: is the route profitable at minimum amount?
    let min_out = simulate_multi_hop(hops, MIN_AMOUNT_3HOP)?;
    let min_profit = min_out - MIN_AMOUNT_3HOP;
    if min_profit <= 0.0 { return None; }

    // Binary search: find the amount that maximizes profit
    let mut lo = MIN_AMOUNT_3HOP;
    let mut hi = MAX_AMOUNT_3HOP;
    let mut best_amount = lo;
    let mut best_profit = min_profit;

    for _ in 0..20 { // ~20 iterations = precision to ~0.0001 ETH
        let mid = (lo + hi) / 2.0;
        let out = simulate_multi_hop(hops, mid).unwrap_or(0.0);
        let profit = out - mid;

        if profit > best_profit {
            best_profit = profit;
            best_amount = mid;
        }

        // Check slope: is profit still increasing?
        let mid_plus = mid * 1.01;
        let out_plus = simulate_multi_hop(hops, mid_plus).unwrap_or(0.0);
        let profit_plus = out_plus - mid_plus;

        if profit_plus > profit {
            lo = mid; // profit still increasing, go bigger
        } else {
            hi = mid; // past the peak, go smaller
        }
    }

    if best_profit > 0.0 {
        Some((best_amount, best_profit))
    } else {
        None
    }
}

/// Try all pool combinations for a 3-hop cycle A→B→C→A
async fn try_3hop_cycle(
    a: Address, b: Address, c: Address,
    pool_state: &PoolState,
    min_profit_eth: f64,
) -> Option<MultiHopArb> {
    let pools_ab = pool_state.get_pools_for_pair(a, b).await;
    let pools_bc = pool_state.get_pools_for_pair(b, c).await;
    let pools_ca = pool_state.get_pools_for_pair(c, a).await;

    let mut best: Option<MultiHopArb> = None;

    // Try ALL combinations of pools across legs (cross-DEX arb)
    for p_ab in &pools_ab {
        if !is_pool_viable(p_ab) { continue; }
        let price_ab = match get_effective_price(p_ab, &a) { Some(p) => p, None => continue };
        let fee_ab = 1.0 - (p_ab.fee_bps as f64 / 10000.0);

        for p_bc in &pools_bc {
            if !is_pool_viable(p_bc) { continue; }
            if p_bc.address == p_ab.address { continue; }
            let price_bc = match get_effective_price(p_bc, &b) { Some(p) => p, None => continue };
            let fee_bc = 1.0 - (p_bc.fee_bps as f64 / 10000.0);

            for p_ca in &pools_ca {
                if !is_pool_viable(p_ca) { continue; }
                if p_ca.address == p_ab.address || p_ca.address == p_bc.address { continue; }
                let price_ca = match get_effective_price(p_ca, &c) { Some(p) => p, None => continue };
                let fee_ca = 1.0 - (p_ca.fee_bps as f64 / 10000.0);

                let product = price_ab * price_bc * price_ca * fee_ab * fee_bc * fee_ca;

                if product <= 1.005 || product > 1.0 + MAX_SANE_SPREAD_PCT / 100.0 {
                    continue;
                }

                // Dynamic sizing via simulation + binary search
                let hop_refs: Vec<(&Pool, Address, Address)> = vec![
                    (p_ab, a, b), (p_bc, b, c), (p_ca, c, a),
                ];
                let (optimal_amount, profit_raw) = match find_optimal_multihop_amount(&hop_refs) {
                    Some(v) => v,
                    None => continue,
                };
                let est_profit = profit_raw / 1e18;
                let gas_cost = 0.000018; // 3 swaps + flash loan
                let net_profit = est_profit - gas_cost;

                if net_profit < min_profit_eth { continue; }

                let amount = U256::from(optimal_amount as u128);
                let arb = MultiHopArb {
                    hops: vec![
                        ArbHop { pool: p_ab.clone(), token_in: a, token_out: b },
                        ArbHop { pool: p_bc.clone(), token_in: b, token_out: c },
                        ArbHop { pool: p_ca.clone(), token_in: c, token_out: a },
                    ],
                    flash_token: a,
                    amount_in: amount,
                    estimated_profit_eth: net_profit,
                };

                if best.as_ref().map_or(true, |x| net_profit > x.estimated_profit_eth) {
                    let profit_pct = (product - 1.0) * 100.0;
                    info!(
                        profit = format!("{:.4}%", profit_pct),
                        net_eth = format!("{:.6}", net_profit),
                        amount_eth = format!("{:.4}", optimal_amount / 1e18),
                        route = format!("{:?}→{:?}→{:?}→{:?}", p_ab.dex, p_bc.dex, p_ca.dex, p_ab.dex),
                        "circular 3-hop"
                    );
                    best = Some(arb);
                }
            }
        }
    }

    best
}

/// Try all pool combinations for a 4-hop cycle A→B→C→D→A
async fn try_4hop_cycle(
    a: Address, b: Address, c: Address, d: Address,
    pool_state: &PoolState,
    min_profit_eth: f64,
) -> Option<MultiHopArb> {
    let pools_ab = pool_state.get_pools_for_pair(a, b).await;
    let pools_bc = pool_state.get_pools_for_pair(b, c).await;
    let pools_cd = pool_state.get_pools_for_pair(c, d).await;
    let pools_da = pool_state.get_pools_for_pair(d, a).await;

    // Use cheapest-fee pool per leg to limit combinatorial explosion
    let p_ab = pools_ab.iter().filter(|p| p.fee_bps > 0).min_by_key(|p| p.fee_bps)?;
    let p_bc = pools_bc.iter().filter(|p| p.fee_bps > 0).min_by_key(|p| p.fee_bps)?;
    let p_cd = pools_cd.iter().filter(|p| p.fee_bps > 0).min_by_key(|p| p.fee_bps)?;
    let p_da = pools_da.iter().filter(|p| p.fee_bps > 0).min_by_key(|p| p.fee_bps)?;

    let price_ab = get_effective_price(p_ab, &a)?;
    let price_bc = get_effective_price(p_bc, &b)?;
    let price_cd = get_effective_price(p_cd, &c)?;
    let price_da = get_effective_price(p_da, &d)?;

    let fee_ab = 1.0 - (p_ab.fee_bps as f64 / 10000.0);
    let fee_bc = 1.0 - (p_bc.fee_bps as f64 / 10000.0);
    let fee_cd = 1.0 - (p_cd.fee_bps as f64 / 10000.0);
    let fee_da = 1.0 - (p_da.fee_bps as f64 / 10000.0);

    let product = price_ab * price_bc * price_cd * price_da * fee_ab * fee_bc * fee_cd * fee_da;

    if product <= 1.002 || product > 1.0 + MAX_SANE_SPREAD_PCT / 100.0 {
        return None;
    }

    // Dynamic sizing via simulation
    let hop_refs: Vec<(&Pool, Address, Address)> = vec![
        (p_ab, a, b), (p_bc, b, c), (p_cd, c, d), (p_da, d, a),
    ];
    let (optimal_amount, profit_raw) = find_optimal_multihop_amount(&hop_refs)?;
    let est_profit = profit_raw / 1e18;
    let gas_cost = 0.000024; // 4 swaps + flash loan
    let net_profit = est_profit - gas_cost;

    if net_profit < min_profit_eth { return None; }

    let amount = U256::from(optimal_amount as u128);
    let profit_pct = (product - 1.0) * 100.0;

    info!(
        profit = format!("{:.4}%", profit_pct),
        net_eth = format!("{:.6}", net_profit),
        route = format!("{:?}→{:?}→{:?}→{:?}", p_ab.dex, p_bc.dex, p_cd.dex, p_da.dex),
        "circular 4-hop"
    );

    Some(MultiHopArb {
        hops: vec![
            ArbHop { pool: p_ab.clone(), token_in: a, token_out: b },
            ArbHop { pool: p_bc.clone(), token_in: b, token_out: c },
            ArbHop { pool: p_cd.clone(), token_in: c, token_out: d },
            ArbHop { pool: p_da.clone(), token_in: d, token_out: a },
        ],
        flash_token: a,
        amount_in: amount,
        estimated_profit_eth: net_profit,
    })
}
