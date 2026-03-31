pub mod indexer;
pub mod tracker;

use alloy::primitives::{Address, U256};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use tokio::sync::RwLock;

use crate::decoder::DexType;

/// Represents a liquidity pool on a DEX
#[derive(Debug, Clone)]
pub struct Pool {
    pub address: Address,
    pub dex: DexType,
    pub token0: Address,
    pub token1: Address,
    pub reserve0: U256,
    pub reserve1: U256,
    /// Fee in basis points (e.g., 30 = 0.3%)
    pub fee_bps: u32,
    /// For V3 pools: current sqrt price
    pub sqrt_price_x96: Option<U256>,
    /// For V3 pools: current tick
    pub tick: Option<i32>,
    /// For V3 pools: liquidity
    pub liquidity: Option<u128>,
    /// For Camelot V2: directional fees (buy fee may differ from sell fee)
    pub fee_bps_token0: Option<u32>,  // fee when selling token0
    pub fee_bps_token1: Option<u32>,  // fee when selling token1
}

/// Token pair key for quick lookup
#[derive(Debug, Clone, Hash, PartialEq, Eq)]
pub struct PairKey {
    pub token0: Address,
    pub token1: Address,
}

impl PairKey {
    pub fn new(a: Address, b: Address) -> Self {
        if a < b {
            Self { token0: a, token1: b }
        } else {
            Self { token0: b, token1: a }
        }
    }
}

/// Shared pool state accessible from multiple tasks
#[derive(Clone)]
pub struct PoolState {
    /// All pools indexed by address
    pub pools: Arc<RwLock<HashMap<Address, Pool>>>,
    /// Pools indexed by token pair for fast arb lookups
    pub pair_pools: Arc<RwLock<HashMap<PairKey, Vec<Address>>>>,
    /// Token adjacency: for each token, which other tokens are reachable in 1 hop
    pub token_neighbors: Arc<RwLock<HashMap<Address, HashSet<Address>>>>,
}

impl PoolState {
    pub fn new() -> Self {
        Self {
            pools: Arc::new(RwLock::new(HashMap::new())),
            pair_pools: Arc::new(RwLock::new(HashMap::new())),
            token_neighbors: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    pub async fn insert_pool(&self, pool: Pool) {
        let addr = pool.address;
        let pair_key = PairKey::new(pool.token0, pool.token1);

        let t0 = pool.token0;
        let t1 = pool.token1;
        self.pools.write().await.insert(addr, pool);

        let mut pair_pools = self.pair_pools.write().await;
        pair_pools
            .entry(pair_key)
            .or_insert_with(Vec::new)
            .push(addr);
        drop(pair_pools);

        // Update token neighbor graph
        let mut neighbors = self.token_neighbors.write().await;
        neighbors.entry(t0).or_default().insert(t1);
        neighbors.entry(t1).or_default().insert(t0);
    }

    pub async fn update_reserves(&self, pool_addr: Address, reserve0: U256, reserve1: U256) {
        if let Some(pool) = self.pools.write().await.get_mut(&pool_addr) {
            pool.reserve0 = reserve0;
            pool.reserve1 = reserve1;
        }
    }

    pub async fn update_v3_state(
        &self,
        pool_addr: Address,
        sqrt_price_x96: U256,
        tick: i32,
        liquidity: u128,
    ) {
        if let Some(pool) = self.pools.write().await.get_mut(&pool_addr) {
            pool.sqrt_price_x96 = Some(sqrt_price_x96);
            pool.tick = Some(tick);
            pool.liquidity = Some(liquidity);
        }
    }

    /// Get all pools for a given token pair
    pub async fn get_pools_for_pair(&self, token_a: Address, token_b: Address) -> Vec<Pool> {
        let pair_key = PairKey::new(token_a, token_b);
        let pair_pools = self.pair_pools.read().await;
        let pools = self.pools.read().await;

        pair_pools
            .get(&pair_key)
            .map(|addrs| {
                addrs
                    .iter()
                    .filter_map(|addr| pools.get(addr).cloned())
                    .collect()
            })
            .unwrap_or_default()
    }

    pub async fn pool_count(&self) -> usize {
        self.pools.read().await.len()
    }

    pub async fn has_pool(&self, addr: Address) -> bool {
        self.pools.read().await.contains_key(&addr)
    }

    /// Get all tokens reachable from `token` in one hop
    pub async fn get_neighbors(&self, token: Address) -> HashSet<Address> {
        self.token_neighbors
            .read()
            .await
            .get(&token)
            .cloned()
            .unwrap_or_default()
    }
}
