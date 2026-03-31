use alloy::primitives::{address, Address, U256};
use alloy::providers::Provider;
use alloy::sol;
use std::cmp::Ordering;
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::{debug, info, warn};

// Aave V3 Pool on Arbitrum
pub const AAVE_V3_POOL: Address = address!("794a61358D6845594F94dc1DB02A252b5b4814aD");
pub const AAVE_V3_DATA_PROVIDER: Address = address!("69FA688f1Dc47d4B5d8029D5a35FB7a548310654");

// Radiant V2 (Aave fork) on Arbitrum
pub const RADIANT_POOL: Address = address!("F4B1486DD74D07706052A33d31d7c0AAFD0659E1");

sol! {
    #[sol(rpc)]
    interface IAavePool {
        function getUserAccountData(address user) external view returns (
            uint256 totalCollateralBase,
            uint256 totalDebtBase,
            uint256 availableBorrowsBase,
            uint256 currentLiquidationThreshold,
            uint256 ltv,
            uint256 healthFactor
        );

        function liquidationCall(
            address collateralAsset,
            address debtAsset,
            address user,
            uint256 debtToCover,
            bool receiveAToken
        ) external;
    }

    #[sol(rpc)]
    interface IAaveDataProvider {
        function getAllReservesTokens() external view returns (TokenData[] memory);
        function getUserReserveData(address asset, address user) external view returns (
            uint256 currentATokenBalance,
            uint256 currentStableDebt,
            uint256 currentVariableDebt,
            uint256 principalStableDebt,
            uint256 scaledVariableDebt,
            uint256 stableBorrowRate,
            uint256 liquidityRate,
            uint40 stableRateLastUpdated,
            bool usageAsCollateralEnabled
        );

        struct TokenData {
            string symbol;
            address tokenAddress;
        }
    }
}

/// A tracked lending position that might become liquidatable
#[derive(Debug, Clone)]
pub struct TrackedPosition {
    pub user: Address,
    pub health_factor: f64,        // 1e18 scaled → f64
    pub total_collateral_usd: f64,
    pub total_debt_usd: f64,
    pub collateral_token: Address, // primary collateral
    pub debt_token: Address,       // primary debt
    pub last_updated: u64,
}

impl PartialEq for TrackedPosition {
    fn eq(&self, other: &Self) -> bool {
        self.user == other.user
    }
}
impl Eq for TrackedPosition {}

// Min-heap by health factor (lowest HF = highest priority)
impl PartialOrd for TrackedPosition {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}
impl Ord for TrackedPosition {
    fn cmp(&self, other: &Self) -> Ordering {
        // Reverse ordering for min-heap (lower HF = higher priority)
        other
            .health_factor
            .partial_cmp(&self.health_factor)
            .unwrap_or(Ordering::Equal)
    }
}

/// Represents a liquidation opportunity
#[derive(Debug, Clone)]
pub struct LiquidationOpportunity {
    pub protocol: LendingProtocol,
    pub user: Address,
    pub collateral_token: Address,
    pub debt_token: Address,
    pub debt_to_cover: U256,
    pub health_factor: f64,
    pub estimated_profit_eth: f64,
    pub liquidation_bonus_bps: u32,
}

#[derive(Debug, Clone, Copy)]
pub enum LendingProtocol {
    AaveV3,
    Radiant,
}

/// Manages position monitoring and liquidation detection
#[derive(Clone)]
pub struct LiquidationMonitor {
    /// Positions sorted by health factor (lowest first)
    pub positions: Arc<RwLock<Vec<TrackedPosition>>>,
    /// Known borrowers to track
    pub borrowers: Arc<RwLock<Vec<Address>>>,
}

impl LiquidationMonitor {
    pub fn new() -> Self {
        Self {
            positions: Arc::new(RwLock::new(Vec::new())),
            borrowers: Arc::new(RwLock::new(Vec::new())),
        }
    }

    /// Scan tracked positions for liquidatable ones on Aave V3
    pub async fn scan_positions<P: Provider + Clone>(
        &self,
        _provider: &P,
    ) -> Vec<LiquidationOpportunity> {
        let positions = self.positions.read().await;
        let mut opportunities = Vec::new();

        for pos in positions.iter() {
            if pos.health_factor >= 1.0 {
                continue; // Not liquidatable
            }

            // Liquidation bonus on Aave V3: typically 5-10%
            let bonus_bps = match pos.collateral_token {
                a if a == address!("82aF49447D8a07e3bd95BD0d56f35241523fBab1") => 500,  // WETH: 5%
                a if a == address!("2f2a2543B76A4166549F7aaB2e75Bef0aefC5B0f") => 650,  // WBTC: 6.5%
                a if a == address!("af88d065e77c8cC2239327C5EDb3A432268e5831") => 500,  // USDC: 5%
                a if a == address!("912CE59144191C1204E64559FE8253a0e49E6548") => 1000, // ARB: 10%
                _ => 500, // default 5%
            };

            // Max liquidatable: 50% of debt (Aave close factor)
            let debt_to_cover_usd = pos.total_debt_usd * 0.5;
            let profit_usd = debt_to_cover_usd * (bonus_bps as f64 / 10000.0) - 0.5; // minus gas
            let profit_eth = profit_usd / 2000.0; // rough ETH price

            if profit_eth > 0.0001 {
                opportunities.push(LiquidationOpportunity {
                    protocol: LendingProtocol::AaveV3,
                    user: pos.user,
                    collateral_token: pos.collateral_token,
                    debt_token: pos.debt_token,
                    debt_to_cover: U256::from((debt_to_cover_usd * 1e6) as u128), // USDC decimals approx
                    health_factor: pos.health_factor,
                    estimated_profit_eth: profit_eth,
                    liquidation_bonus_bps: bonus_bps,
                });
            }
        }

        opportunities
    }
}

// ─── Chainlink price feed addresses on Arbitrum ───
pub const CHAINLINK_ETH_USD: Address = address!("639Fe6ab55C921f74e7fac1ee960C0B6293ba612");
pub const CHAINLINK_BTC_USD: Address = address!("6ce185860a4963106506C203335A2910413708e9");
pub const CHAINLINK_ARB_USD: Address = address!("b2A824043730FE05F3DA2efaFa1CBbe83fa548D6");
pub const CHAINLINK_LINK_USD: Address = address!("86E53CF1B870786351Da77A57575e79CB55812CB");

// Top collateral tokens for resolving positions
const COLLATERAL_TOKENS: [(Address, &str); 6] = [
    (address!("82aF49447D8a07e3bd95BD0d56f35241523fBab1"), "WETH"),
    (address!("2f2a2543B76A4166549F7aaB2e75Bef0aefC5B0f"), "WBTC"),
    (address!("af88d065e77c8cC2239327C5EDb3A432268e5831"), "USDC"),
    (address!("Fd086bC7CD5C481DCC9C85ebE478A1C0b69FCbb9"), "USDT"),
    (address!("912CE59144191C1204E64559FE8253a0e49E6548"), "ARB"),
    (address!("f97f4df75117a78c1A5a0DBb814Af92458539FB4"), "LINK"),
];

/// Background task: refresh health factors per block (~250ms on Arbitrum)
pub async fn start_position_monitor<P: Provider + Clone + 'static>(
    provider: P,
    monitor: LiquidationMonitor,
) {
    info!("Starting liquidation position monitor (per-block)");

    let pool = IAavePool::new(AAVE_V3_POOL, &provider);
    let data_prov = IAaveDataProvider::new(AAVE_V3_DATA_PROVIDER, &provider);

    // Poll every 1s (every ~4 Arbitrum blocks). Faster than 30s, manageable RPC load.
    let mut interval = tokio::time::interval(std::time::Duration::from_secs(1));
    let mut full_refresh_counter = 0u32;

    loop {
        interval.tick().await;
        full_refresh_counter += 1;

        let borrowers = monitor.borrowers.read().await.clone();
        if borrowers.is_empty() { continue; }

        let mut updated_positions = Vec::new();

        // Batch: check up to 20 borrowers per tick to avoid RPC overload
        // Full refresh of all borrowers every 30 ticks (~30s)
        let batch_size = if full_refresh_counter % 30 == 0 { borrowers.len() } else { 20.min(borrowers.len()) };
        let offset = ((full_refresh_counter as usize) * 20) % borrowers.len().max(1);
        let batch: Vec<&Address> = if full_refresh_counter % 30 == 0 {
            borrowers.iter().collect()
        } else {
            borrowers.iter().cycle().skip(offset).take(batch_size).collect()
        };

        for user in &batch {
            match pool.getUserAccountData(**user).call().await {
                Ok(data) => {
                    let hf_raw = u256_to_f64(data.healthFactor);
                    let hf = hf_raw / 1e18;

                    // Track positions with HF < 2.0
                    if hf < 2.0 && hf > 0.0 {
                        let total_collateral_usd = u256_to_f64(data.totalCollateralBase) / 1e8;
                        let total_debt_usd = u256_to_f64(data.totalDebtBase) / 1e8;

                        // Resolve primary collateral and debt tokens for near-liquidation positions
                        let (collateral_token, debt_token) = if hf < 1.2 {
                            resolve_position_tokens(&data_prov, **user).await
                        } else {
                            (Address::ZERO, Address::ZERO)
                        };

                        updated_positions.push(TrackedPosition {
                            user: **user,
                            health_factor: hf,
                            total_collateral_usd,
                            total_debt_usd,
                            collateral_token,
                            debt_token,
                            last_updated: 0,
                        });
                    }
                }
                Err(e) => {
                    debug!(user = %user, error = %e, "Failed to get account data");
                }
            }
        }

        // Merge with existing positions (keep resolved tokens from previous iterations)
        let mut existing = monitor.positions.write().await;
        for new_pos in &updated_positions {
            if let Some(existing_pos) = existing.iter_mut().find(|p| p.user == new_pos.user) {
                existing_pos.health_factor = new_pos.health_factor;
                existing_pos.total_collateral_usd = new_pos.total_collateral_usd;
                existing_pos.total_debt_usd = new_pos.total_debt_usd;
                if new_pos.collateral_token != Address::ZERO {
                    existing_pos.collateral_token = new_pos.collateral_token;
                    existing_pos.debt_token = new_pos.debt_token;
                }
            } else {
                existing.push(new_pos.clone());
            }
        }

        // Remove positions that are now healthy (HF > 2.0)
        existing.retain(|p| p.health_factor < 2.0 && p.health_factor > 0.0);

        // Sort by health factor (lowest first)
        existing.sort_by(|a, b| {
            a.health_factor.partial_cmp(&b.health_factor).unwrap_or(Ordering::Equal)
        });

        let count = existing.len();
        // Log only on full refresh or if we have critical positions
        if full_refresh_counter % 30 == 0 && count > 0 {
            let lowest_hf = existing.first().map(|p| p.health_factor).unwrap_or(0.0);
            info!(
                tracked = count,
                lowest_hf = format!("{:.4}", lowest_hf),
                "Position monitor update"
            );
        }
    }
}

/// Resolve primary collateral and debt tokens for a borrower
async fn resolve_position_tokens<P: Provider + Clone>(
    data_prov: &IAaveDataProvider::IAaveDataProviderInstance<&P>,
    user: Address,
) -> (Address, Address) {
    let mut max_collateral = (Address::ZERO, 0.0f64);
    let mut max_debt = (Address::ZERO, 0.0f64);

    for (token, _name) in &COLLATERAL_TOKENS {
        match data_prov.getUserReserveData(*token, user).call().await {
            Ok(data) => {
                let collateral = u256_to_f64(data.currentATokenBalance);
                let debt = u256_to_f64(data.currentVariableDebt) + u256_to_f64(data.currentStableDebt);
                if collateral > max_collateral.1 {
                    max_collateral = (*token, collateral);
                }
                if debt > max_debt.1 {
                    max_debt = (*token, debt);
                }
            }
            Err(_) => continue,
        }
    }

    (max_collateral.0, max_debt.0)
}

/// Discover borrowers from recent Borrow events on Aave V3.
/// In production, scan Borrow events to build the borrower list incrementally.
pub async fn discover_borrowers<P: Provider + Clone + 'static>(
    provider: P,
    monitor: LiquidationMonitor,
) {
    use alloy::primitives::B256;
    use alloy::rpc::types::Filter;

    info!("Discovering Aave V3 borrowers from recent events");

    // Borrow event: keccak256("Borrow(address,address,address,uint256,uint8,uint256,uint16)")
    let borrow_topic: B256 =
        "0xb3d084820fb1a9decffb176436bd02558d15fac9b0ddfed8c465bc7359d7dce0"
            .parse()
            .unwrap_or_default();

    let current_block = match provider.get_block_number().await {
        Ok(b) => b,
        Err(e) => {
            warn!(error = %e, "Failed to get block number for borrower discovery");
            return;
        }
    };

    // Scan last ~40 min of blocks (~9900 blocks at 250ms)
    // Stay under 10k block limit for public RPCs
    let from_block = current_block.saturating_sub(9900);

    let filter = Filter::new()
        .address(AAVE_V3_POOL)
        .event_signature(borrow_topic)
        .from_block(from_block)
        .to_block(current_block);

    match provider.get_logs(&filter).await {
        Ok(logs) => {
            let mut borrowers = monitor.borrowers.write().await;
            let mut new_count = 0;

            for log in &logs {
                // Borrow event: topic1 = reserve, topic2 = onBehalfOf (the borrower)
                if let Some(user_topic) = log.topics().get(2) {
                    let user = Address::from_slice(&user_topic.0[12..]);
                    if !borrowers.contains(&user) {
                        borrowers.push(user);
                        new_count += 1;
                    }
                }
            }

            info!(
                total = borrowers.len(),
                new = new_count,
                "Borrowers discovered from Aave V3"
            );
        }
        Err(e) => {
            warn!(error = %e, "Failed to discover borrowers");
        }
    }
}

fn u256_to_f64(v: U256) -> f64 {
    v.to_string().parse::<f64>().unwrap_or(0.0)
}
