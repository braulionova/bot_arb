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
                .pool_max_idle_per_host(4)
                .pool_idle_timeout(std::time::Duration::from_secs(60))
                .timeout(std::time::Duration::from_secs(10))
                .build()
                .unwrap(),
        }
    }

    /// Set the local signer for fast tx signing (call once at startup)
    pub fn set_signer(&mut self, signer: PrivateKeySigner) {
        self.signer = Some(signer);
    }

    /// Execute an arb opportunity — SEND IMMEDIATELY, no delays
    pub async fn execute_arb(&self, opp: &ArbOpportunity) -> Result<()> {
        if self.arb_contract == Address::ZERO {
            return Ok(());
        }
        let t0 = std::time::Instant::now();

        // Try Balancer first (0% fee)
        let calldata = self.build_flash_arb(opp, false);
        match self.send_tx(calldata, opp, false).await {
            Ok(()) => {
                let t_total = t0.elapsed();
                info!(send_ms = t_total.as_millis(), profit = format!("{:.6}", opp.expected_profit_eth), "Balancer arb sent");
                return Ok(());
            }
            Err(e) => {
                let err_str = e.to_string();
                // If Balancer doesn't have the token (BAL#528), try Aave
                if err_str.contains("BAL#528") || err_str.contains("BAL") {
                    info!("Balancer lacks token — trying Aave flash loan");
                    let aave_cd = self.build_aave_flash_arb(opp);
                    let result = self.send_tx(aave_cd, opp, false).await;
                    let t_total = t0.elapsed();
                    info!(send_ms = t_total.as_millis(), profit = format!("{:.6}", opp.expected_profit_eth), "Aave arb pipeline");
                    return result;
                }
                return Err(e);
            }
        }
    }

    /// Execute a multi-hop arb. Try Balancer (0% fee), fallback to Aave if BAL#528.
    pub async fn execute_multi_hop(&self, arb: &MultiHopArb) -> Result<()> {
        if self.arb_contract == Address::ZERO {
            return Ok(());
        }

        let balancer_calldata = self.build_multi_hop_calldata(arb, false);
        match self.send_tx(balancer_calldata, &self.multi_hop_dummy(arb), false).await {
            Ok(()) => Ok(()),
            Err(e) if e.to_string().contains("BAL#528") || e.to_string().contains("BAL") => {
                debug!("Balancer lacks token, trying Aave");
                let aave_calldata = self.build_multi_hop_calldata(arb, true);
                self.send_tx(aave_calldata, &self.multi_hop_dummy(arb), false).await
            }
            Err(e) => Err(e),
        }
    }

    fn build_multi_hop_calldata(&self, arb: &MultiHopArb, use_aave: bool) -> Bytes {
        let hops: Vec<IArbExecutor::SwapHop> = arb
            .hops
            .iter()
            .map(|h| {
                let zero_for_one = h.token_in < h.token_out;
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

        // Gas: 0.01 gwei like successful bots (was 0.5 gwei = 50x more expensive)
        let max_fee = 10_000_000u128;     // 0.01 gwei
        let priority_fee = 1_000_000u128; // 0.001 gwei

        // FAST PATH: sign locally + send raw
        if let Some(ref signer) = self.signer {
            // Sim on publicnode (reliable, high rate limits)
            let sim_provider = alloy::providers::ProviderBuilder::new()
                .connect_http("https://arbitrum-one-rpc.publicnode.com".parse().unwrap());
            let sim_tx = TransactionRequest::default()
                .to(self.arb_contract)
                .from(self.wallet.address)
                .input(calldata.clone().into());
            match sim_provider.call(sim_tx).await {
                Ok(_) => {
                    info!(
                        sim_ms = t0.elapsed().as_millis(),
                        profit = format!("{:.6}", opp.expected_profit_eth),
                        "SIM PASSED (sequencer) — sending"
                    );
                }
                Err(e) => {
                    self.wallet.reset_nonce(nonce);
                    return Err(eyre::eyre!("sim rejected: {}", e));
                }
            }
            let t_sim = t0.elapsed();

            let mut tx_1559 = TxEip1559 {
                chain_id: 42161,
                nonce,
                gas_limit: 350_000,
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

            match self.raw_client.post("https://arb1.arbitrum.io/rpc")
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
                                    // Poll up to 30s for receipt
                                    for _ in 0..60 {
                                        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
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
                            if msg.contains("nonce") {
                                let _ = self.wallet.sync_nonce(&self.sim_provider).await;
                            } else {
                                self.wallet.reset_nonce(nonce);
                            }
                            return Ok(());
                        }
                    }
                    self.wallet.reset_nonce(nonce);
                    return Ok(());
                }
                Err(_) => {
                    self.wallet.reset_nonce(nonce);
                    // Fall through to slow path
                }
            }
        }

        // SLOW FALLBACK: alloy send_transaction
        let tx = TransactionRequest::default()
            .to(self.arb_contract)
            .nonce(nonce)
            .max_fee_per_gas(10_000_000u128)
            .max_priority_fee_per_gas(1_000_000u128)
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
