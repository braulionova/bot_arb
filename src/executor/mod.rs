use alloy::primitives::{Address, Bytes, U256, TxKind};
use alloy::providers::Provider;
use alloy::rpc::types::TransactionRequest;
use alloy::consensus::{TxEip1559, SignableTransaction};
use alloy::network::TxSignerSync;
use alloy::signers::local::PrivateKeySigner;
use alloy::sol;
use eyre::Result;
use tracing::{debug, error, info, warn};

use crate::arb::{ArbOpportunity, MultiHopArb};
use crate::decoder::DexType;
use crate::liquidation::LiquidationOpportunity;
use crate::sim::LocalSim;
use crate::telegram::TgMsg;
use crate::timeboost::TimeboostManager;
use crate::wallet::WalletManager;
use std::sync::Arc;
use tokio::sync::mpsc;

// ABI for on-demand reserve refresh
sol! {
    #[sol(rpc)]
    interface IRefreshV2 {
        function getReserves() external view returns (uint112 reserve0, uint112 reserve1, uint32 blockTimestampLast);
    }
    #[sol(rpc)]
    interface IRefreshV3 {
        function slot0() external view returns (uint160 sqrtPriceX96, int24 tick, uint16 observationIndex, uint16 observationCardinality, uint16 observationCardinalityNext, uint8 feeProtocol, bool unlocked);
        function liquidity() external view returns (uint128);
    }
}

/// Maximum gas price in wei (0.5 gwei) — abort if gas exceeds this
const MAX_GAS_PRICE: u128 = 500_000_000;

// ─── ABI bindings for ArbExecutor.sol (direct pool calls, no routers) ───

sol! {
    #[sol(rpc)]
    interface IArbExecutor {
        /// Flash loan arb: direct pool calls (saves ~60-100k gas vs routers)
        function executeArbFlashLoan(
            address tokenIn,
            uint256 amountIn,
            address buyPool,
            address sellPool,
            address tokenOut,
            bool buyIsV3,
            bool sellIsV3,
            uint256 minProfit
        ) external;

        /// Multi-hop flash loan arb (3+ legs)
        struct SwapHop {
            address pool;
            address tokenOut;
            bool isV3;
            bool isCurve;
            bool zeroForOne;
            uint256 amountOut;
            int128 curveI;
            int128 curveJ;
        }

        function executeMultiHopFlashLoan(
            address flashToken,
            uint256 flashAmount,
            SwapHop[] calldata hops,
            uint256 minProfit
        ) external;

        /// Direct arb (no flash loan, uses contract balance)
        function executeArb(
            address tokenIn,
            uint256 amountIn,
            address buyPool,
            address sellPool,
            address tokenOut,
            bool buyIsV3,
            bool sellIsV3,
            uint256 minProfit
        ) external;

        /// 2-hop via Aave V3 flash loan (fallback)
        function executeArbAaveFL(
            address tokenIn,
            uint256 amountIn,
            address buyPool,
            address sellPool,
            address tokenOut,
            bool buyIsV3,
            bool sellIsV3,
            uint256 minProfit
        ) external;

        /// Multi-hop via Aave V3 flash loan (fallback)
        function executeMultiHopAaveFL(
            address flashToken,
            uint256 flashAmount,
            SwapHop[] calldata hops,
            uint256 minProfit
        ) external;

        /// Liquidation via Balancer flash loan: borrow debtToken → liquidate → sell collateral → repay
        function liquidateFlashLoan(
            address lendingPool,
            address borrower,
            address debtToken,
            uint256 debtAmount,
            address collateralToken,
            address sellPool,
            bool sellIsV3,
            bool sellIsCurve,
            uint256 minProfit
        ) external;

        function withdraw(address token, uint256 amount) external;
        function withdrawETH() external;
        function setApproval(address token, address spender, uint256 amount) external;
    }
}

fn is_v3(dex: DexType) -> bool {
    matches!(
        dex,
        DexType::UniswapV3 | DexType::CamelotV3 | DexType::SushiSwapV3 | DexType::PancakeSwapV3
    )
}

fn is_curve(dex: DexType) -> bool {
    matches!(dex, DexType::CurveStable | DexType::BalancerStable)
}

/// Executes arbitrage transactions on-chain via ArbExecutor contract
#[derive(Clone)]
pub struct Executor<P: Provider + Clone, S: Provider> {
    sim_provider: P,
    send_provider: S,
    wallet: WalletManager,
    arb_contract: Address,
    use_flash_loan: bool,
    tg_tx: Option<mpsc::UnboundedSender<TgMsg>>,
    local_sim: Arc<LocalSim>,
    timeboost: Option<Arc<TimeboostManager>>,
    /// Local signer for fast tx signing (no RPC roundtrip for eth_chainId)
    signer: Option<PrivateKeySigner>,
    /// HTTP client for raw tx submission directly to sequencer
    raw_client: reqwest::Client,
    /// Sequencer RPC URL for direct tx submission (lowest latency)
    sequencer_url: String,
}

impl<P: Provider + Clone + 'static, S: Provider + Clone + 'static> Executor<P, S> {
    pub fn new(
        sim_provider: P,
        send_provider: S,
        wallet: WalletManager,
        arb_contract: Address,
        use_flash_loan: bool,
        tg_tx: Option<mpsc::UnboundedSender<TgMsg>>,
        local_sim: Arc<LocalSim>,
        timeboost: Option<Arc<TimeboostManager>>,
    ) -> Self {
        // Sequencer endpoint from env (direct to Arbitrum sequencer = lowest latency)
        let sequencer_url = std::env::var("SEQUENCER_ENDPOINT_URL")
            .unwrap_or_else(|_| "https://arb1-sequencer.arbitrum.io/rpc".to_string());

        Self {
            sim_provider,
            send_provider,
            wallet,
            arb_contract,
            use_flash_loan,
            tg_tx,
            local_sim,
            timeboost,
            signer: None,
            raw_client: reqwest::Client::builder()
                .pool_max_idle_per_host(8)
                .pool_idle_timeout(std::time::Duration::from_secs(120))
                .tcp_keepalive(std::time::Duration::from_secs(30))
                .tcp_nodelay(true)
                .timeout(std::time::Duration::from_secs(5))
                .build()
                .unwrap(),
            sequencer_url,
        }
    }

    /// Direct probe: try a 2-hop flash loan arb via eth_call without off-chain math.
    /// Tests multiple amounts. Returns Ok if a profitable amount was found and tx sent.
    pub async fn probe_2hop_arb(
        &self,
        token_in: Address,
        token_out: Address,
        buy_pool: &crate::pools::Pool,
        sell_pool: &crate::pools::Pool,
    ) -> Result<()> {
        if self.arb_contract == Address::ZERO { return Ok(()); }

        let buy_is_v3 = is_v3(buy_pool.dex);
        let sell_is_v3 = is_v3(sell_pool.dex);

        let contract = IArbExecutor::new(self.arb_contract, &self.sim_provider);

        // FAST PROBE: max 3 eth_calls (was 7) — speed > optimal amount
        // Try medium first, then branch up or down
        let medium = U256::from(100_000_000_000_000_000u64); // 0.1 ETH
        let small = U256::from(10_000_000_000_000_000u64);   // 0.01 ETH
        let large = U256::from(500_000_000_000_000_000u64);  // 0.5 ETH

        let mut best_amount = U256::ZERO;

        // Call 1: try medium
        let cd_med = contract.executeArbFlashLoan(
            token_in, medium, buy_pool.address, sell_pool.address,
            token_out, buy_is_v3, sell_is_v3, U256::ZERO,
        ).calldata().clone();

        if self.local_sim.simulate(self.arb_contract, &cd_med).await {
            // Medium works! Send immediately, try large in background for next time
            best_amount = medium;
        } else {
            // Try small — still fast (1 more call)
            let cd_sm = contract.executeArbFlashLoan(
                token_in, small, buy_pool.address, sell_pool.address,
                token_out, buy_is_v3, sell_is_v3, U256::ZERO,
            ).calldata().clone();
            if self.local_sim.simulate(self.arb_contract, &cd_sm).await {
                best_amount = small;
            }
        }

        if best_amount.is_zero() {
            return Err(eyre::eyre!("no profitable amount"));
        }

        info!(
            amount = format!("{:.4}", crate::arb::u256_to_f64(best_amount) / 1e18),
            buy_pool = %buy_pool.address,
            buy = ?buy_pool.dex, sell = ?sell_pool.dex,
            token_in = %token_in, token_out = %token_out,
            "PROBE HIT — sending flash loan arb"
        );

        // Build and send the winning tx
        let calldata = contract.executeArbFlashLoan(
            token_in, best_amount,
            buy_pool.address, sell_pool.address,
            token_out, buy_is_v3, sell_is_v3, U256::ZERO,
        ).calldata().clone();

        let dummy_opp = ArbOpportunity {
            trigger_swap: crate::decoder::DecodedSwap {
                dex: buy_pool.dex, pool: buy_pool.address,
                token_in, token_out,
                amount_in: best_amount, amount_out: U256::ZERO,
                sender: Address::ZERO,
            },
            buy_pool: buy_pool.clone(), sell_pool: sell_pool.clone(),
            optimal_amount_in: best_amount,
            expected_profit: U256::ZERO,
            expected_profit_eth: 0.001, // unknown, sim passed
            buy_fee: 0, sell_fee: 0,
        };

        self.send_tx(calldata, &dummy_opp, false).await
    }

    /// Set the local signer for fast tx signing (call once at startup)
    pub fn set_signer(&mut self, signer: PrivateKeySigner) {
        self.signer = Some(signer);
    }

    /// Quick reserve refresh for 2-hop arb (2 pools)
    async fn refresh_2hop_reserves(&self, opp: &ArbOpportunity) -> bool {
        for pool in [&opp.buy_pool, &opp.sell_pool] {
            if is_v3(pool.dex) {
                let v3 = IRefreshV3::new(pool.address, &self.sim_provider);
                if let Ok(s0) = v3.slot0().call().await {
                    let new_sqrt = U256::from(s0.sqrtPriceX96);
                    if let Some(old_sqrt) = pool.sqrt_price_x96 {
                        let old_f = crate::arb::u256_to_f64(old_sqrt);
                        let new_f = crate::arb::u256_to_f64(new_sqrt);
                        if old_f > 0.0 && ((new_f - old_f) / old_f).abs() > 0.02 {
                            info!("2-hop reserves stale — skipping");
                            return false;
                        }
                    }
                }
            } else if !is_curve(pool.dex) {
                let v2 = IRefreshV2::new(pool.address, &self.sim_provider);
                if let Ok(res) = v2.getReserves().call().await {
                    let new_r0 = crate::arb::u256_to_f64(U256::from(res.reserve0));
                    let old_r0 = crate::arb::u256_to_f64(pool.reserve0);
                    if old_r0 > 0.0 && ((new_r0 - old_r0) / old_r0).abs() > 0.02 {
                        info!("2-hop reserves stale — skipping");
                        return false;
                    }
                }
            }
        }
        true
    }

    /// Execute an arb opportunity — SEND IMMEDIATELY, no delays
    /// Retries at 50% and 25% amount if sim says "insufficient profit"
    pub async fn execute_arb(&self, opp: &ArbOpportunity) -> Result<()> {
        if self.arb_contract == Address::ZERO {
            return Ok(());
        }
        let t0 = std::time::Instant::now();

        // On-demand reserve refresh
        if !self.refresh_2hop_reserves(opp).await {
            return Err(eyre::eyre!("reserves stale — skipping"));
        }

        let amounts = [opp.optimal_amount_in, opp.optimal_amount_in / U256::from(2), opp.optimal_amount_in / U256::from(4)];

        for (i, amount) in amounts.iter().enumerate() {
            if amount.is_zero() { continue; }

            let mut opp_reduced = opp.clone();
            opp_reduced.optimal_amount_in = *amount;

            // Try Balancer first (0% fee)
            let calldata = self.build_flash_arb(&opp_reduced, false);
            match self.send_tx(calldata, &opp_reduced, false).await {
                Ok(()) => {
                    let t_total = t0.elapsed();
                    info!(send_ms = t_total.as_millis(), profit = format!("{:.6}", opp.expected_profit_eth), "Balancer arb sent");
                    return Ok(());
                }
                Err(e) => {
                    let err_str = e.to_string();
                    if err_str.contains("BAL#528") || err_str.contains("BAL") {
                        info!("Balancer lacks token — trying Aave flash loan");
                        let aave_cd = self.build_aave_flash_arb(&opp_reduced);
                        match self.send_tx(aave_cd, &opp_reduced, false).await {
                            Ok(()) => return Ok(()),
                            Err(_) if i < amounts.len() - 1 => continue,
                            Err(e) => return Err(e),
                        }
                    } else if err_str.contains("insufficient profit") && i < amounts.len() - 1 {
                        debug!(attempt = i + 1, amount = %amount, "Retrying with smaller amount");
                        continue;
                    }
                    if i == amounts.len() - 1 {
                        return Err(e);
                    }
                }
            }
        }
        Ok(())
    }

    /// Refresh reserves of pools in a multi-hop route via on-chain RPC calls.
    /// Logs reserve changes for debugging. Returns false if reserves changed >5%.
    async fn refresh_multihop_reserves(&self, arb: &MultiHopArb) -> bool {
        let mut any_stale = false;
        for (i, hop) in arb.hops.iter().enumerate() {
            let pool = &hop.pool;
            if is_v3(pool.dex) {
                let v3 = IRefreshV3::new(pool.address, &self.sim_provider);
                let slot0_res = v3.slot0().call().await;
                if let Ok(s0) = slot0_res {
                    let new_sqrt = U256::from(s0.sqrtPriceX96);
                    if let Some(old_sqrt) = pool.sqrt_price_x96 {
                        let old_f = crate::arb::u256_to_f64(old_sqrt);
                        let new_f = crate::arb::u256_to_f64(new_sqrt);
                        if old_f > 0.0 {
                            let change_pct = ((new_f - old_f) / old_f).abs() * 100.0;
                            if change_pct > 5.0 { any_stale = true; }
                            info!(hop = i, pool = %pool.address, dex = ?pool.dex, change_pct = format!("{:.2}%", change_pct), "V3 price delta");
                        }
                    }
                }
            } else if !is_curve(pool.dex) {
                let v2 = IRefreshV2::new(pool.address, &self.sim_provider);
                if let Ok(res) = v2.getReserves().call().await {
                    let new_r0 = U256::from(res.reserve0);
                    let new_r1 = U256::from(res.reserve1);
                    let old_r0 = crate::arb::u256_to_f64(pool.reserve0);
                    let old_r1 = crate::arb::u256_to_f64(pool.reserve1);
                    let new_r0f = crate::arb::u256_to_f64(new_r0);
                    let new_r1f = crate::arb::u256_to_f64(new_r1);
                    let change0 = if old_r0 > 0.0 { ((new_r0f - old_r0) / old_r0).abs() * 100.0 } else { 0.0 };
                    let change1 = if old_r1 > 0.0 { ((new_r1f - old_r1) / old_r1).abs() * 100.0 } else { 0.0 };
                    if change0 > 5.0 || change1 > 5.0 { any_stale = true; }
                    info!(
                        hop = i, pool = %pool.address, dex = ?pool.dex,
                        r0_cached = format!("{:.0}", old_r0), r0_fresh = format!("{:.0}", new_r0f),
                        r1_cached = format!("{:.0}", old_r1), r1_fresh = format!("{:.0}", new_r1f),
                        delta0 = format!("{:.2}%", change0), delta1 = format!("{:.2}%", change1),
                        "V2 reserve delta"
                    );
                }
            }
        }
        if any_stale {
            info!("Reserves changed >5% — arb stale, skipping");
        }
        !any_stale
    }

    /// Execute a multi-hop arb. Try Balancer (0% fee), fallback to Aave if BAL#528.
    pub async fn execute_multi_hop(&self, arb: &MultiHopArb) -> Result<()> {
        if self.arb_contract == Address::ZERO {
            return Ok(());
        }

        // On-demand reserve refresh: verify pools haven't changed significantly
        if !self.refresh_multihop_reserves(arb).await {
            return Err(eyre::eyre!("reserves stale — skipping"));
        }

        // Try with original amount, then retry at 50% and 25% if "insufficient profit"
        // The off-chain estimate often overestimates → smaller amount = less price impact
        let amounts = [arb.amount_in, arb.amount_in / U256::from(2), arb.amount_in / U256::from(4)];

        for (i, amount) in amounts.iter().enumerate() {
            if amount.is_zero() { continue; }

            let mut reduced_arb = arb.clone();
            reduced_arb.amount_in = *amount;

            let balancer_calldata = self.build_multi_hop_calldata(&reduced_arb, false);
            match self.send_tx(balancer_calldata, &self.multi_hop_dummy(&reduced_arb), false).await {
                Ok(()) => return Ok(()),
                Err(e) => {
                    let msg = e.to_string();
                    if msg.contains("BAL#528") || msg.contains("BAL") {
                        debug!("Balancer lacks token, trying Aave");
                        let aave_calldata = self.build_multi_hop_calldata(&reduced_arb, true);
                        match self.send_tx(aave_calldata, &self.multi_hop_dummy(&reduced_arb), false).await {
                            Ok(()) => return Ok(()),
                            Err(_) if i < amounts.len() - 1 => continue,
                            Err(e) => return Err(e),
                        }
                    } else if msg.contains("insufficient profit") && i < amounts.len() - 1 {
                        debug!(attempt = i + 1, amount = %amount, "Retrying with smaller amount");
                        continue;
                    } else {
                        return Err(e);
                    }
                }
            }
        }
        Ok(())
    }

    fn build_multi_hop_calldata(&self, arb: &MultiHopArb, use_aave: bool) -> Bytes {
        let hops: Vec<IArbExecutor::SwapHop> = arb
            .hops
            .iter()
            .map(|h| {
                let zero_for_one = h.token_in == h.pool.token0;
                IArbExecutor::SwapHop {
                    pool: h.pool.address,
                    tokenOut: h.token_out,
                    isV3: is_v3(h.pool.dex),
                    isCurve: is_curve(h.pool.dex),
                    zeroForOne: zero_for_one,
                    amountOut: U256::ZERO,
                    curveI: if is_curve(h.pool.dex) { if zero_for_one { 0 } else { 1 } } else { 0 },
                    curveJ: if is_curve(h.pool.dex) { if zero_for_one { 1 } else { 0 } } else { 0 },
                }
            })
            .collect();

        let contract = IArbExecutor::new(self.arb_contract, &self.sim_provider);

        if use_aave {
            let call = contract.executeMultiHopAaveFL(
                arb.flash_token,
                arb.amount_in,
                hops,
                U256::ZERO,
            );
            call.calldata().clone()
        } else {
            let call = contract.executeMultiHopFlashLoan(
                arb.flash_token,
                arb.amount_in,
                hops,
                U256::ZERO,
            );
            call.calldata().clone()
        }
    }

    fn multi_hop_dummy(&self, arb: &MultiHopArb) -> ArbOpportunity {
        ArbOpportunity {
            trigger_swap: crate::decoder::DecodedSwap {
                dex: crate::decoder::DexType::Unknown,
                pool: Address::ZERO,
                token_in: arb.flash_token,
                token_out: arb.hops.last().map(|h| h.token_out).unwrap_or(Address::ZERO),
                amount_in: arb.amount_in,
                amount_out: U256::ZERO,
                sender: Address::ZERO,
            },
            buy_pool: arb.hops[0].pool.clone(),
            sell_pool: arb.hops.last().unwrap().pool.clone(),
            optimal_amount_in: arb.amount_in,
            expected_profit: U256::ZERO,
            expected_profit_eth: arb.estimated_profit_eth,
            buy_fee: 0,
            sell_fee: 0,
        }
    }

    /// Execute a liquidation opportunity.
    /// Flash-borrows the debt token from Balancer (0% fee), liquidates the position on
    /// Aave V3 (or a compatible fork), sells the received collateral on a DEX, and repays.
    pub async fn execute_liquidation(&self, opp: &LiquidationOpportunity) -> Result<()> {
        if self.arb_contract == Address::ZERO {
            warn!("No arb contract deployed — skipping liquidation");
            return Ok(());
        }

        use crate::liquidation::AAVE_V3_POOL;

        info!(
            profit_eth = format!("{:.8}", opp.estimated_profit_eth),
            user = %opp.user,
            hf = format!("{:.4}", opp.health_factor),
            bonus_bps = opp.liquidation_bonus_bps,
            "Executing liquidation"
        );

        let contract = IArbExecutor::new(self.arb_contract, &self.sim_provider);
        let call = contract.liquidateFlashLoan(
            AAVE_V3_POOL,
            opp.user,
            opp.debt_token,
            opp.debt_to_cover,
            opp.collateral_token,
            Address::ZERO, // sellPool: must be resolved by caller before calling this
            false,         // sellIsV3
            false,         // sellIsCurve
            U256::ZERO,    // minProfit: accept any profit (contract enforces balance check)
        );

        // Build a minimal dummy opp so we can reuse send_tx's gas / nonce logic
        let dummy_pool = crate::pools::Pool {
            address: Address::ZERO,
            dex: crate::decoder::DexType::Unknown,
            token0: opp.debt_token,
            token1: opp.collateral_token,
            reserve0: U256::ZERO,
            reserve1: U256::ZERO,
            fee_bps: 0,
            sqrt_price_x96: None,
            tick: None,
            liquidity: None,
            fee_bps_token0: None,
            fee_bps_token1: None,
            last_update: None,
        };
        let dummy_opp = ArbOpportunity {
            trigger_swap: crate::decoder::DecodedSwap {
                dex: crate::decoder::DexType::Unknown,
                pool: Address::ZERO,
                token_in: opp.debt_token,
                token_out: opp.collateral_token,
                amount_in: opp.debt_to_cover,
                amount_out: U256::ZERO,
                sender: Address::ZERO,
            },
            buy_pool: dummy_pool.clone(),
            sell_pool: dummy_pool,
            optimal_amount_in: opp.debt_to_cover,
            expected_profit: U256::ZERO,
            expected_profit_eth: opp.estimated_profit_eth,
            buy_fee: 0,
            sell_fee: 0,
        };

        self.send_tx(call.calldata().clone(), &dummy_opp, false).await
    }

    /// Build calldata for Aave V3 flash loan (fallback when Balancer lacks the token)
    fn build_aave_flash_arb(&self, opp: &ArbOpportunity) -> Bytes {
        let buy_is_v3 = is_v3(opp.buy_pool.dex);
        let sell_is_v3 = is_v3(opp.sell_pool.dex);
        let contract = IArbExecutor::new(self.arb_contract, &self.sim_provider);
        let call = contract.executeArbAaveFL(
            opp.trigger_swap.token_in,
            opp.optimal_amount_in,
            opp.buy_pool.address,
            opp.sell_pool.address,
            opp.trigger_swap.token_out,
            buy_is_v3,
            sell_is_v3,
            U256::ZERO,
        );
        call.calldata().clone()
    }

    /// Build calldata for flash loan arb matching ArbExecutor.sol
    /// Uses direct pool addresses (no routers) — saves ~60-100k gas per 2-leg arb.
    /// If `reverse` is true, flash loan the other token (tokenOut) instead.
    fn build_flash_arb(&self, opp: &ArbOpportunity, reverse: bool) -> Bytes {
        let buy_is_v3 = is_v3(opp.buy_pool.dex);
        let sell_is_v3 = is_v3(opp.sell_pool.dex);

        // minProfit = 0: accept any positive profit
        // The contract only checks that we end with more than we started
        // (balanceAfter >= balanceBefore + debt + minProfit)
        // Since Balancer fee=0, even 1 wei of profit is fine
        let min_profit = U256::ZERO;

        let contract = IArbExecutor::new(self.arb_contract, &self.sim_provider);

        let call = if !reverse {
            // Normal: borrow tokenIn, buy tokenOut on buy pool, sell on sell pool
            contract.executeArbFlashLoan(
                opp.trigger_swap.token_in,
                opp.optimal_amount_in,
                opp.buy_pool.address,   // direct pool address
                opp.sell_pool.address,  // direct pool address
                opp.trigger_swap.token_out,
                buy_is_v3,
                sell_is_v3,
                min_profit,
            )
        } else {
            // Reversed: borrow tokenOut, sell on sell pool first, buy back on buy pool
            contract.executeArbFlashLoan(
                opp.trigger_swap.token_out,  // borrow the OTHER token
                opp.optimal_amount_in,
                opp.sell_pool.address,       // "buy" side is now the sell pool
                opp.buy_pool.address,        // "sell" side is now the buy pool
                opp.trigger_swap.token_in,   // intermediate is now tokenIn
                sell_is_v3,
                buy_is_v3,
                min_profit,
            )
        };

        call.calldata().clone()
    }

    /// Send tx to chain. Single sim on sequencer (freshest state) then send immediately.
    async fn send_tx(&self, calldata: Bytes, opp: &ArbOpportunity, _is_retry: bool) -> Result<()> {
        let t0 = std::time::Instant::now();
        let nonce = self.wallet.next_nonce();

        // Gas: 0.3 gwei max, 0.01 gwei priority (realistic for Arbitrum inclusion)
        let max_fee = 300_000_000u128;     // 0.3 gwei
        let priority_fee = 10_000_000u128; // 0.01 gwei

        // FAST PATH: sign locally + send raw
        if let Some(ref signer) = self.signer {
            // Single sim via local Nitro node (freshest state, ~1-5ms)
            if !self.local_sim.simulate(self.arb_contract, &calldata).await {
                self.wallet.reset_nonce(nonce);
                return Err(eyre::eyre!("sim rejected: insufficient profit"));
            }
            let t_sim = t0.elapsed();
            info!(
                sim_ms = t_sim.as_millis(),
                profit = format!("{:.6}", opp.expected_profit_eth),
                "SIM PASSED — sending"
            );

            let mut tx_1559 = TxEip1559 {
                chain_id: 42161,
                nonce,
                gas_limit: 700_000, // flash loan + multi-pool swaps need more gas
                max_fee_per_gas: max_fee,
                max_priority_fee_per_gas: priority_fee,
                to: TxKind::Call(self.arb_contract),
                input: calldata.clone().into(),
                ..Default::default()
            };

            let sig = signer.sign_transaction_sync(&mut tx_1559)?;
            let signed = tx_1559.into_signed(sig);
            use alloy::consensus::transaction::RlpEcdsaTx;
            let mut encoded = Vec::with_capacity(512);
            signed.eip2718_encode(&mut encoded);
            let raw_hex = format!("0x{}", hex::encode(&encoded));
            let t_sign = t0.elapsed();

            let body = serde_json::json!({
                "jsonrpc": "2.0",
                "method": "eth_sendRawTransaction",
                "params": [raw_hex],
                "id": 1
            });

            // Send to BOTH sequencer AND local node for redundancy
            let body_clone = body.clone();
            let raw_client2 = self.raw_client.clone();
            tokio::spawn(async move {
                let _ = raw_client2.post("http://127.0.0.1:8547")
                    .json(&body_clone).send().await;
            });

            match self.raw_client.post(&self.sequencer_url)
                .json(&body).send().await
            {
                Ok(resp) => {
                    let t_sent = t0.elapsed();
                    let text = resp.text().await.unwrap_or_default();
                    if let Ok(json) = serde_json::from_str::<serde_json::Value>(&text) {
                        if let Some(hash) = json.get("result").and_then(|r| r.as_str()) {
                            info!(
                                tx_hash = hash, nonce,
                                profit = format!("{:.6}", opp.expected_profit_eth),
                                sim_ms = t_sim.as_millis(),
                                sign_us = t_sign.as_micros(),
                                total_ms = t_sent.as_millis(),
                                "FAST tx sent (sim passed)"
                            );

                            // Monitor receipt in background (fast path was missing this!)
                            let tx_hash_str = hash.to_string();
                            let wallet_clone = self.wallet.clone();
                            let sim_prov = self.sim_provider.clone();
                            let tg_receipt = self.tg_tx.clone();
                            let est_profit = opp.expected_profit_eth;
                            tokio::spawn(async move {
                                // Parse tx hash and poll for receipt
                                if let Ok(hash_bytes) = tx_hash_str.parse::<alloy::primitives::TxHash>() {
                                    // Poll for receipt: fast initially (200ms x 15 = 3s), then slower (1s x 27 = 27s)
                                    for i in 0..42u32 {
                                        let delay = if i < 15 { 200 } else { 1000 };
                                        tokio::time::sleep(std::time::Duration::from_millis(delay)).await;
                                        match sim_prov.get_transaction_receipt(hash_bytes).await {
                                            Ok(Some(receipt)) => {
                                                if receipt.status() {
                                                    info!(tx_hash = %tx_hash_str, profit = format!("{:.6}", est_profit), "FAST ARB SUCCESS");
                                                    if let Some(ref tx) = tg_receipt {
                                                        let _ = tx.send(TgMsg::ArbSuccess {
                                                            tx_hash: tx_hash_str.clone(),
                                                            profit_eth: est_profit,
                                                        });
                                                    }
                                                } else {
                                                    error!(tx_hash = %tx_hash_str, "FAST ARB REVERTED on-chain");
                                                    if let Some(ref tx) = tg_receipt {
                                                        let _ = tx.send(TgMsg::ArbReverted {
                                                            tx_hash: tx_hash_str.clone(),
                                                            reason: "on-chain revert".to_string(),
                                                        });
                                                    }
                                                }
                                                if let Err(e) = wallet_clone.sync_nonce(&sim_prov).await {
                                                    warn!(error = %e, "Nonce resync failed after FAST receipt");
                                                }
                                                return;
                                            }
                                            Ok(None) => continue, // not mined yet
                                            Err(_) => continue,
                                        }
                                    }
                                    warn!(tx_hash = %tx_hash_str, "FAST tx receipt timeout (30s)");
                                    let _ = wallet_clone.sync_nonce(&sim_prov).await;
                                }
                            });

                            return Ok(());
                        } else if let Some(err) = json.get("error") {
                            let msg = err.to_string();
                            error!(error = %msg, nonce, "Sequencer REJECTED tx");
                            if msg.contains("nonce") {
                                let _ = self.wallet.sync_nonce(&self.sim_provider).await;
                            } else {
                                self.wallet.reset_nonce(nonce);
                            }
                            return Err(eyre::eyre!("sequencer rejected: {}", msg));
                        }
                    }
                    error!(nonce, "Sequencer returned unparseable response");
                    self.wallet.reset_nonce(nonce);
                    return Err(eyre::eyre!("sequencer: unparseable response"));
                }
                Err(e) => {
                    error!(error = %e, "Sequencer HTTP error — trying local node");
                    self.wallet.reset_nonce(nonce);
                    // Try local node directly
                    let nonce2 = self.wallet.next_nonce();
                    let mut tx2 = TxEip1559 {
                        chain_id: 42161,
                        nonce: nonce2,
                        gas_limit: 500_000,
                        max_fee_per_gas: max_fee,
                        max_priority_fee_per_gas: priority_fee,
                        to: TxKind::Call(self.arb_contract),
                        input: calldata.clone().into(),
                        ..Default::default()
                    };
                    let sig2 = signer.sign_transaction_sync(&mut tx2)?;
                    let signed2 = tx2.into_signed(sig2);
                    let mut encoded2 = Vec::with_capacity(512);
                    signed2.eip2718_encode(&mut encoded2);
                    let raw2 = format!("0x{}", hex::encode(&encoded2));
                    let body2 = serde_json::json!({"jsonrpc":"2.0","method":"eth_sendRawTransaction","params":[raw2],"id":1});
                    match self.raw_client.post("http://127.0.0.1:8547").json(&body2).send().await {
                        Ok(resp) => {
                            let text = resp.text().await.unwrap_or_default();
                            info!(response = %text, "Local node send result");
                        }
                        Err(e2) => {
                            error!(error = %e2, "Local node also failed");
                            self.wallet.reset_nonce(nonce2);
                        }
                    }
                    // Fall through to slow path
                }
            }
        }

        // SLOW FALLBACK: alloy send_transaction
        let tx = TransactionRequest::default()
            .to(self.arb_contract)
            .nonce(nonce)
            .max_fee_per_gas(300_000_000u128)
            .max_priority_fee_per_gas(10_000_000u128)
            .gas_limit(350_000u64)
            .input(calldata.into())
            .value(U256::ZERO);

        let t_build_tx = t0.elapsed();

        match self.send_provider.send_transaction(tx).await {
            Ok(pending) => {
                let t_sent = t0.elapsed();
                let tx_hash = *pending.tx_hash();
                info!(
                    ?tx_hash,
                    nonce,
                    profit_eth = format!("{:.8}", opp.expected_profit_eth),
                    build_tx_us = t_build_tx.as_micros(),
                    sign_send_ms = t_sent.as_millis(),
                    "Arb tx sent"
                );

                // Notify Telegram: tx sent
                if let Some(ref tx) = self.tg_tx {
                    let _ = tx.send(TgMsg::TxSent {
                        arb_num: 0,
                        tx_hash: format!("{:?}", tx_hash),
                        profit_eth: opp.expected_profit_eth,
                    });
                }

                // Monitor receipt in background and resync nonce
                let wallet = self.wallet.clone();
                let sim_provider = self.sim_provider.clone();
                let tg_receipt = self.tg_tx.clone();
                let est_profit = opp.expected_profit_eth;
                tokio::spawn(async move {
                    match pending.get_receipt().await {
                        Ok(receipt) => {
                            let hash_str = format!("{:?}", tx_hash);
                            if receipt.status() {
                                info!(?tx_hash, "ARB SUCCESS — tx confirmed");
                                if let Some(ref tx) = tg_receipt {
                                    let _ = tx.send(TgMsg::ArbSuccess {
                                        tx_hash: hash_str,
                                        profit_eth: est_profit,
                                    });
                                }
                            } else {
                                error!(?tx_hash, "ARB REVERTED on-chain");
                                if let Some(ref tx) = tg_receipt {
                                    let _ = tx.send(TgMsg::ArbReverted {
                                        tx_hash: hash_str,
                                        reason: "on-chain revert".to_string(),
                                    });
                                }
                            }
                            if let Err(e) = wallet.sync_nonce(&sim_provider).await {
                                warn!(error = %e, "Failed to resync nonce after receipt");
                            }
                        }
                        Err(e) => {
                            error!(?tx_hash, error = %e, "Failed to get receipt");
                            let _ = wallet.sync_nonce(&sim_provider).await;
                        }
                    }
                });
            }
            Err(e) => {
                let err_str = e.to_string();
                error!(error = %e, "Failed to send arb tx");
                // On nonce errors, resync from chain
                if err_str.contains("nonce") {
                    let _ = self.wallet.sync_nonce(&self.sim_provider).await;
                } else {
                    self.wallet.reset_nonce(nonce);
                }
            }
        }

        Ok(())
    }
}
