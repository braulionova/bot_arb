use dashmap::DashMap;
use http_body_util::{BodyExt, Full};
use hyper::{body::Incoming, server::conn::http1, Request, Response};
use hyper_util::rt::TokioIo;
use redis::AsyncCommands;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    net::SocketAddr,
    sync::{
        atomic::{AtomicU64, AtomicUsize, Ordering},
        Arc,
    },
};
use tokio::net::TcpListener;
use tracing::{info, warn};

// ─── Function selectors for pool state calls ───

const SEL_GET_RESERVES: &str = "0x0902f1ac"; // getReserves() — Uniswap V2
const SEL_SLOT0: &str = "0x3850c7bd";        // slot0() — Uniswap V3
const SEL_LIQUIDITY: &str = "0x1a686502";    // liquidity() — Uniswap V3
const SEL_TOKEN0: &str = "0x0dfe1681";       // token0()
const SEL_TOKEN1: &str = "0xd21220a7";       // token1()
const SEL_FEE: &str = "0xddca3f43";          // fee()

/// Known V3 pool addresses (lowercase, no 0x prefix used in comparisons)
const TOP_V3_POOLS: &[(&str, bool, &str)] = &[
    // (address, is_v3, label) — is_v3=true for all entries here
    ("c6962004f452be9203591991d15f6b388e09e8d0", true, "WETH/USDC 0.05% UniV3"),
    ("c31e54c7a869b9fcbecc14363cf510d1c41fa443", true, "WETH/USDC 0.3% UniV3"),
    ("2f5e87c9312fa29aed5c179e456625d79015299c", true, "WETH/USDT 0.05% UniV3"),
    ("80a9ae39310abf666a87c743d6ebbd0e8c42158e", true, "WETH/ARB 0.05% UniV3"),
    ("641c00a822e8b671738d32a431a4fb6074e5c79d", true, "WETH/ARB 0.3% UniV3"),
    ("d845f7d4f4deb9ff3bcece5a4e2d2b3f74b22dc4", true, "WETH/WBTC 0.05% UniV3"),
    ("149e36e72726e0bcea5c59d40f8e22e42e07fce2", true, "WBTC/WETH 0.3% UniV3"),
    ("2391ddc81cd63aaead9bde63b00bb63e60ddbe9c", true, "USDC/USDT 0.01% UniV3"),
    ("b791ad21ba45c76629003b4a2f04c0d544406e37", true, "ARB/USDT 0.05% UniV3"),
    ("fae2ae0a9f87fd35b5b0e24b47bac796a7eefea1", true, "ARB/USDC 0.05% UniV3"),
    ("35218a1cbac5bbc3e57fd9bd38219d37571b3537", true, "wstETH/WETH 0.01% UniV3"),
    ("1aeedD3727a6431b8f070c0afaa81cc74f273882", true, "GMX/WETH 0.3% UniV3"),
    ("a2ae929bffbda42ea0cda0a62f7e38a20105f313", true, "LINK/WETH 0.3% UniV3"),
    ("dbaeb7f0dfe3a0aafd798ccecb5b22e708f7852c", true, "USDC.e/USDC 0.01% UniV3"),
    ("17c14d2c404d167802b16c450d3c99f88f2c4f4d", true, "WETH/USDC.e 0.05% UniV3"),
    ("6f38e884725a116c9c7fbf208e79fe8828a2595f", true, "WETH/USDC 0.01% UniV3"),
    // SushiSwap V3
    ("18d3284d9eff64fc97b64ab2b871738677ae3632", true, "WETH/USDC 0.05% SushiV3"),
    ("01428e1f5e3e8c5baab288f68df6ebe0296d9ff9", true, "WETH/ARB 0.3% SushiV3"),
    // PancakeSwap V3
    ("7fcdc35463e3770c2fb992716cd070b63540b947", true, "WETH/USDC 0.01% PCS"),
    ("d9e2a1a61b6e61b275cec326465d417e52c1b95c", true, "WETH/USDC 0.05% PCS"),
    ("7e928afb59f5de9d2f4d162f754c6eb40c88aa8e", true, "USDT/USDC 0.01% PCS"),
    ("93cce474015007b38da0ecea96671ee4dc3d40ad", true, "ARB/USDC 0.01% PCS"),
    ("0d7c4b40018969f81750d0a164c3839a77353efb", true, "ARB/WETH 0.05% PCS"),
    ("54076c901d4fdf76c1fa1f77fafc3fc1022adbe5", true, "WBTC/WETH 0.05% PCS"),
    ("4bfc22a4da7f31f8a912a79a7e44a822398b4390", true, "WETH/WBTC 0.01% PCS"),
    ("11d53ec50bc8f54b9357fbffe2a7de034fc00f8b3", true, "ARB/WETH 0.01% PCS"),
    ("f5bfda16f9e57f0b7a67c57b42407c33c31349b6", true, "WETH/GMX 0.25% PCS"),
    ("0ba3d55678c019b8101061855fe4ea8d3ece784f", true, "WETH/LINK 0.25% PCS"),
    ("d5d1f85e65ce58a4782852f4a845b1d6ca71f1a2", true, "USDC/DAI 0.01% PCS"),
    // Camelot V3
    ("b1026b8e7276e7ac75410f1fcbbe21796e8f7526", true, "WETH/USDC CamelotV3"),
    ("7cccba38e2d959fe135e79aebb57ccb27b128358", true, "WETH/USDT CamelotV3"),
    ("a748e35d18fc8b9543b17c0b3e5f8e84a87e5749", true, "WETH/ARB CamelotV3"),
    ("59c7c246e42f5de40fc1e8f72de4e9a5e27a9d26", true, "USDC/USDT CamelotV3"),
    ("e1509bab38beb1753bc37e1f6a942c52523fbac5", true, "ARB/USDC CamelotV3"),
];

/// RPC methods cached per-block in memory (hot path, ~0ms)
const CACHEABLE_METHODS: &[&str] = &[
    "eth_call",
    "eth_getBalance",
    "eth_getTransactionCount",
    "eth_gasPrice",
    "eth_blockNumber",
    "eth_chainId",
    "eth_getCode",
    "eth_getStorageAt",
];

/// Static methods cached forever in Redis
const STATIC_METHODS: &[&str] = &["eth_chainId", "net_version", "web3_clientVersion"];

/// Methods cached in Redis with longer TTL (survives restarts)
const REDIS_CACHED_METHODS: &[&str] = &[
    "eth_call",
    "eth_getCode",
    "eth_getStorageAt",
    "eth_getLogs",
];

#[derive(Clone)]
struct CacheProxy {
    /// L1: In-memory per-block cache (hot, ~0ms)
    mem_cache: Arc<DashMap<String, String>>,
    /// L1: Static cache (eth_chainId, etc.)
    static_cache: Arc<DashMap<String, String>>,
    /// L2: Redis connection (warm, ~0.1ms local)
    redis: redis::aio::ConnectionManager,
    /// Current block number
    current_block: Arc<AtomicU64>,
    cache_block: Arc<AtomicU64>,
    /// Upstream RPC endpoints
    upstreams: Vec<String>,
    /// Round-robin counter
    rr_counter: Arc<AtomicUsize>,
    /// HTTP client
    client: Client,
    /// Stats
    mem_hits: Arc<AtomicU64>,
    redis_hits: Arc<AtomicU64>,
    misses: Arc<AtomicU64>,
    /// Pool state hits (served from local Redis store)
    pool_hits: Arc<AtomicU64>,
}

#[derive(Deserialize)]
struct JsonRpcRequest {
    jsonrpc: Option<String>,
    method: String,
    params: Option<Value>,
    id: Value,
}

#[derive(Serialize)]
struct JsonRpcError {
    jsonrpc: String,
    error: JsonRpcErrorObj,
    id: Value,
}

#[derive(Serialize)]
struct JsonRpcErrorObj {
    code: i64,
    message: String,
}

impl CacheProxy {
    async fn new(upstreams: Vec<String>, redis_url: &str) -> Self {
        let client = Client::builder()
            .pool_max_idle_per_host(20)
            .pool_idle_timeout(std::time::Duration::from_secs(60))
            .timeout(std::time::Duration::from_secs(10))
            .build()
            .expect("failed to build HTTP client");

        let redis_client = redis::Client::open(redis_url).expect("invalid redis URL");
        let redis = redis::aio::ConnectionManager::new(redis_client)
            .await
            .expect("failed to connect to Redis");

        Self {
            mem_cache: Arc::new(DashMap::with_capacity(4096)),
            static_cache: Arc::new(DashMap::with_capacity(16)),
            redis,
            current_block: Arc::new(AtomicU64::new(0)),
            cache_block: Arc::new(AtomicU64::new(0)),
            rr_counter: Arc::new(AtomicUsize::new(0)),
            upstreams,
            client,
            mem_hits: Arc::new(AtomicU64::new(0)),
            redis_hits: Arc::new(AtomicU64::new(0)),
            misses: Arc::new(AtomicU64::new(0)),
            pool_hits: Arc::new(AtomicU64::new(0)),
        }
    }

    fn cache_key(method: &str, params: &Option<Value>) -> String {
        match params {
            Some(p) => format!("rpc:{}:{}", method, p),
            None => format!("rpc:{}", method),
        }
    }

    fn is_cacheable(method: &str) -> bool {
        CACHEABLE_METHODS.contains(&method)
    }

    fn is_static(method: &str) -> bool {
        STATIC_METHODS.contains(&method)
    }

    fn is_redis_cached(method: &str) -> bool {
        REDIS_CACHED_METHODS.contains(&method)
    }

    fn is_upstream_error(response: &str) -> bool {
        let r = response.to_lowercase();
        r.contains("rate limit")
            || r.contains("too many requests")
            || r.contains("limit exceeded")
            || r.contains("retry after")
            || r.contains("unauthorized")
            || r.contains("api key")
            || r.contains("authenticate")
            || r.contains("sign up to")
    }

    fn is_success(response: &str) -> bool {
        if let Ok(parsed) = serde_json::from_str::<Value>(response) {
            return parsed.get("result").is_some();
        }
        false
    }

    fn maybe_invalidate(&self, new_block: u64) {
        let cached_block = self.cache_block.load(Ordering::Relaxed);
        if new_block > cached_block {
            self.mem_cache.clear();
            self.cache_block.store(new_block, Ordering::Relaxed);
            self.current_block.store(new_block, Ordering::Relaxed);
        }
    }

    /// Forward request using round-robin with rate limit detection
    async fn forward(&self, body: &str) -> Result<String, String> {
        let n = self.upstreams.len();
        let start_idx = self.rr_counter.fetch_add(1, Ordering::Relaxed) % n;

        for i in 0..n {
            let idx = (start_idx + i) % n;
            let upstream = &self.upstreams[idx];

            match self
                .client
                .post(upstream)
                .header("Content-Type", "application/json")
                .body(body.to_string())
                .send()
                .await
            {
                Ok(resp) => {
                    let status = resp.status();
                    match resp.text().await {
                        Ok(text) => {
                            if status.as_u16() == 429 || Self::is_upstream_error(&text) {
                                warn!(upstream, "error/rate-limited, rotating");
                                continue;
                            }
                            return Ok(text);
                        }
                        Err(e) => {
                            warn!(upstream, error = %e, "read failed");
                            continue;
                        }
                    }
                }
                Err(e) => {
                    warn!(upstream, error = %e, "upstream failed");
                    continue;
                }
            }
        }
        Err("all upstreams failed or rate limited".to_string())
    }

    // ─── Pool state interception ───────────────────────────────────────────────

    /// Normalise an address string to 40 lowercase hex chars (no 0x prefix).
    fn normalise_addr(s: &str) -> String {
        s.trim_start_matches("0x").to_lowercase()
    }

    /// Returns true if `addr` is a known tracked pool.
    fn is_tracked_pool(addr: &str) -> bool {
        let norm = Self::normalise_addr(addr);
        TOP_V3_POOLS.iter().any(|(a, _, _)| *a == norm)
    }

    /// Extract (to_addr_norm, selector_hex) from eth_call params.
    /// eth_call params: [{"to": "0x...", "data": "0x..."}, "latest"]
    fn parse_eth_call(params: &Value) -> Option<(String, String)> {
        let arr = params.as_array()?;
        let obj = arr.first()?.as_object()?;
        let to = obj.get("to")?.as_str()?;
        let data = obj.get("data")?.as_str().unwrap_or("");
        // selector = first 4 bytes = 8 hex chars + "0x" prefix
        let data_stripped = data.trim_start_matches("0x");
        if data_stripped.len() < 8 {
            return None;
        }
        let selector = format!("0x{}", &data_stripped[..8].to_lowercase());
        Some((Self::normalise_addr(to), selector))
    }

    /// Pad a hex string (no prefix) to 32 bytes (64 hex chars), left-padded with zeros.
    fn pad32(hex: &str) -> String {
        let s = hex.trim_start_matches("0x");
        if s.len() >= 64 {
            s[s.len() - 64..].to_string()
        } else {
            format!("{:0>64}", s)
        }
    }

    /// ABI-encode getReserves() return: (uint112 r0, uint112 r1, uint32 ts)
    /// Returns "0x" + 3×32-byte slots.
    fn encode_reserves(reserve0: &str, reserve1: &str) -> String {
        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        let ts_hex = format!("{:x}", ts);
        format!(
            "0x{}{}{}",
            Self::pad32(reserve0),
            Self::pad32(reserve1),
            Self::pad32(&ts_hex)
        )
    }

    /// ABI-encode slot0() return:
    /// (uint160 sqrtPriceX96, int24 tick, uint16 obsIdx, uint16 obsCard, uint16 obsCardNext, uint8 feeProtocol, bool unlocked)
    /// 7 × 32-byte slots; only sqrtPriceX96 and tick are meaningful for arb.
    fn encode_slot0(sqrt_price: &str, tick: i32) -> String {
        // tick is int24 — ABI encodes as two's complement padded to 32 bytes
        let tick_hex = if tick < 0 {
            // two's complement in 256 bits
            let v = u256_from_i32_twos_complement(tick);
            format!("{:0>64x}", v)
        } else {
            format!("{:0>64x}", tick as u64)
        };
        // obsIdx=0, obsCard=1, obsCardNext=1, feeProtocol=0, unlocked=1
        format!(
            "0x{}{}{}{}{}{}{}",
            Self::pad32(sqrt_price),
            tick_hex,
            Self::pad32("0"),    // observationIndex
            Self::pad32("1"),    // observationCardinality
            Self::pad32("1"),    // observationCardinalityNext
            Self::pad32("0"),    // feeProtocol
            Self::pad32("1"),    // unlocked = true
        )
    }

    /// ABI-encode a single uint128 (liquidity).
    fn encode_uint128(hex: &str) -> String {
        format!("0x{}", Self::pad32(hex))
    }

    /// ABI-encode a single address (token0 / token1).
    fn encode_address(addr_norm: &str) -> String {
        // address pads to 32 bytes (leading zeros, no 0x in pad)
        format!("0x{:0>64}", addr_norm)
    }

    /// ABI-encode a single uint24 (fee).
    fn encode_uint24(fee: u32) -> String {
        format!("0x{}", Self::pad32(&format!("{:x}", fee)))
    }

    /// Redis key helpers
    fn redis_key_reserves(addr: &str) -> String {
        format!("pool:{}:reserves", addr)
    }
    fn redis_key_slot0(addr: &str) -> String {
        format!("pool:{}:slot0", addr)
    }
    fn redis_key_meta(addr: &str) -> String {
        format!("pool:{}:meta", addr)
    }

    /// Try to serve an eth_call from local pool state store.
    /// Returns a complete JSON-RPC response string if served, None to fall through.
    async fn try_local_pool_call(&self, params: &Value, req_id: &Value) -> Option<String> {
        let (addr_norm, selector) = Self::parse_eth_call(params)?;

        if !Self::is_tracked_pool(&addr_norm) {
            return None;
        }

        let mut redis = self.redis.clone();

        let result: Option<String> = match selector.as_str() {
            SEL_GET_RESERVES => {
                let key = Self::redis_key_reserves(&addr_norm);
                let raw: Option<String> = redis.get(&key).await.ok()?;
                let v: Value = serde_json::from_str(&raw?).ok()?;
                let r0 = v["reserve0"].as_str()?;
                let r1 = v["reserve1"].as_str()?;
                Some(Self::encode_reserves(r0, r1))
            }
            SEL_SLOT0 => {
                let key = Self::redis_key_slot0(&addr_norm);
                let raw: Option<String> = redis.get(&key).await.ok()?;
                let v: Value = serde_json::from_str(&raw?).ok()?;
                let sqrt = v["sqrt_price_x96"].as_str()?;
                let tick = v["tick"].as_i64()? as i32;
                Some(Self::encode_slot0(sqrt, tick))
            }
            SEL_LIQUIDITY => {
                let key = Self::redis_key_slot0(&addr_norm);
                let raw: Option<String> = redis.get(&key).await.ok()?;
                let v: Value = serde_json::from_str(&raw?).ok()?;
                let liq = v["liquidity"].as_str()?;
                Some(Self::encode_uint128(liq))
            }
            SEL_TOKEN0 => {
                let key = Self::redis_key_meta(&addr_norm);
                let raw: Option<String> = redis.get(&key).await.ok()?;
                let v: Value = serde_json::from_str(&raw?).ok()?;
                let tok = v["token0"].as_str()?;
                Some(Self::encode_address(tok))
            }
            SEL_TOKEN1 => {
                let key = Self::redis_key_meta(&addr_norm);
                let raw: Option<String> = redis.get(&key).await.ok()?;
                let v: Value = serde_json::from_str(&raw?).ok()?;
                let tok = v["token1"].as_str()?;
                Some(Self::encode_address(tok))
            }
            SEL_FEE => {
                let key = Self::redis_key_meta(&addr_norm);
                let raw: Option<String> = redis.get(&key).await.ok()?;
                let v: Value = serde_json::from_str(&raw?).ok()?;
                let fee = v["fee"].as_u64()? as u32;
                Some(Self::encode_uint24(fee))
            }
            _ => None,
        };

        let encoded = result?;

        Some(serde_json::json!({
            "jsonrpc": "2.0",
            "result": encoded,
            "id": req_id
        }).to_string())
    }

    async fn handle_request(&self, req: JsonRpcRequest) -> String {
        let method = &req.method;
        let key = Self::cache_key(method, &req.params);

        // ─── L0: Static memory cache (eth_chainId, etc.) ───
        if Self::is_static(method) {
            if let Some(cached) = self.static_cache.get(&key) {
                self.mem_hits.fetch_add(1, Ordering::Relaxed);
                return Self::with_id(&cached, &req.id);
            }
        }

        // ─── L1: In-memory per-block cache (~0ms) ───
        if Self::is_cacheable(method) && !Self::is_static(method) {
            if let Some(cached) = self.mem_cache.get(&key) {
                self.mem_hits.fetch_add(1, Ordering::Relaxed);
                return Self::with_id(&cached, &req.id);
            }
        }

        // ─── Pool state intercept: serve getReserves/slot0/etc from local store ───
        if method == "eth_call" {
            if let Some(params) = &req.params {
                if let Some(response) = self.try_local_pool_call(params, &req.id).await {
                    self.pool_hits.fetch_add(1, Ordering::Relaxed);
                    return response;
                }
            }
        }

        // ─── L2: Redis cache (~0.1ms) ───
        if Self::is_redis_cached(method) || Self::is_static(method) {
            let mut redis = self.redis.clone();
            if let Ok(Some(cached)) = redis.get::<_, Option<String>>(&key).await {
                self.redis_hits.fetch_add(1, Ordering::Relaxed);
                // Promote to L1
                if Self::is_static(method) {
                    self.static_cache.insert(key.clone(), cached.clone());
                } else if Self::is_cacheable(method) {
                    self.mem_cache.insert(key.clone(), cached.clone());
                }
                return Self::with_id(&cached, &req.id);
            }
        }

        // ─── Cache miss: forward to upstream ───
        self.misses.fetch_add(1, Ordering::Relaxed);

        let forward_body = serde_json::json!({
            "jsonrpc": req.jsonrpc.as_deref().unwrap_or("2.0"),
            "method": req.method,
            "params": req.params.clone().unwrap_or(Value::Array(vec![])),
            "id": req.id.clone()
        });

        match self.forward(&forward_body.to_string()).await {
            Ok(response) => {
                if Self::is_success(&response) {
                    // Store in appropriate caches
                    if Self::is_static(method) {
                        self.static_cache.insert(key.clone(), response.clone());
                        // Redis: no expiry for static
                        let mut redis = self.redis.clone();
                        let _ = redis.set::<_, _, ()>(&key, &response).await;
                    } else {
                        if Self::is_cacheable(method) {
                            self.mem_cache.insert(key.clone(), response.clone());
                        }
                        if Self::is_redis_cached(method) {
                            // Redis: TTL based on method
                            let ttl = Self::redis_ttl(method);
                            let mut redis = self.redis.clone();
                            let _ = redis.set_ex::<_, _, ()>(&key, &response, ttl).await;
                        }
                    }

                    // Track block number
                    if method == "eth_blockNumber" {
                        if let Ok(parsed) = serde_json::from_str::<Value>(&response) {
                            if let Some(hex) = parsed["result"].as_str() {
                                if let Ok(block) =
                                    u64::from_str_radix(hex.trim_start_matches("0x"), 16)
                                {
                                    self.maybe_invalidate(block);
                                }
                            }
                        }
                    }
                }
                response
            }
            Err(e) => {
                let err = JsonRpcError {
                    jsonrpc: "2.0".to_string(),
                    error: JsonRpcErrorObj {
                        code: -32603,
                        message: e,
                    },
                    id: req.id,
                };
                serde_json::to_string(&err).unwrap()
            }
        }
    }

    /// TTL in seconds for Redis-cached methods
    fn redis_ttl(method: &str) -> u64 {
        match method {
            "eth_getCode" => 86400,      // 24h — contract code rarely changes
            "eth_getStorageAt" => 1,     // 1s — storage changes per block
            "eth_call" => 1,             // 1s — state-dependent
            "eth_getLogs" => 5,          // 5s — recent logs
            _ => 2,
        }
    }

    /// Replace the "id" field in a cached response
    fn with_id(cached: &str, id: &Value) -> String {
        if let Ok(mut parsed) = serde_json::from_str::<Value>(cached) {
            parsed["id"] = id.clone();
            return serde_json::to_string(&parsed).unwrap_or_else(|_| cached.to_string());
        }
        cached.to_string()
    }

    async fn handle_http(&self, body: &str) -> String {
        if let Ok(batch) = serde_json::from_str::<Vec<JsonRpcRequest>>(body) {
            let mut results = Vec::with_capacity(batch.len());
            for req in batch {
                results.push(self.handle_request(req).await);
            }
            return format!("[{}]", results.join(","));
        }

        if let Ok(req) = serde_json::from_str::<JsonRpcRequest>(body) {
            return self.handle_request(req).await;
        }

        let err = JsonRpcError {
            jsonrpc: "2.0".to_string(),
            error: JsonRpcErrorObj {
                code: -32700,
                message: "parse error".to_string(),
            },
            id: Value::Null,
        };
        serde_json::to_string(&err).unwrap()
    }
}

// ─── Helpers ─────────────────────────────────────────────────────────────────

/// Convert a negative i32 (tick) to its 256-bit two's complement as a u128 pair
/// suitable for formatting with {:0>64x}.
/// Since tick fits in i24, the two's-complement in 256 bits is just the u256 value
/// of (2^256 + tick). For formatting we only need the lower 64 hex chars.
fn u256_from_i32_twos_complement(v: i32) -> u128 {
    // Two's complement in 256 bits: all upper bits are 1 for negative values.
    // For display purposes we format as a 64-char hex string representing a 256-bit int.
    // i32 negative: 256-bit value = 2^256 + v. Lower 128 bits = 2^128 + v (since upper 128 bits are all 0xff).
    // Simpler: cast to i128 then to u128 (handles sign extension to 64 hex chars).
    v as i128 as u128
}

// ─── Pool pre-warming at startup ─────────────────────────────────────────────

/// Fetch slot0+liquidity for a V3 pool and write to Redis.
async fn fetch_and_store_v3(proxy: &CacheProxy, addr: &str) {
    let addr_norm = CacheProxy::normalise_addr(addr);

    // Build multicall-style: call slot0 and liquidity individually
    let slot0_call = serde_json::json!({
        "jsonrpc": "2.0",
        "method": "eth_call",
        "params": [{"to": format!("0x{}", addr_norm), "data": SEL_SLOT0}, "latest"],
        "id": 1
    });
    let liq_call = serde_json::json!({
        "jsonrpc": "2.0",
        "method": "eth_call",
        "params": [{"to": format!("0x{}", addr_norm), "data": SEL_LIQUIDITY}, "latest"],
        "id": 2
    });
    let token0_call = serde_json::json!({
        "jsonrpc": "2.0",
        "method": "eth_call",
        "params": [{"to": format!("0x{}", addr_norm), "data": SEL_TOKEN0}, "latest"],
        "id": 3
    });
    let token1_call = serde_json::json!({
        "jsonrpc": "2.0",
        "method": "eth_call",
        "params": [{"to": format!("0x{}", addr_norm), "data": SEL_TOKEN1}, "latest"],
        "id": 4
    });
    let fee_call = serde_json::json!({
        "jsonrpc": "2.0",
        "method": "eth_call",
        "params": [{"to": format!("0x{}", addr_norm), "data": SEL_FEE}, "latest"],
        "id": 5
    });

    // Batch all five calls
    let batch_body = serde_json::json!([slot0_call, liq_call, token0_call, token1_call, fee_call]).to_string();
    let resp_str = match proxy.forward(&batch_body).await {
        Ok(r) => r,
        Err(e) => {
            warn!(pool = addr_norm, error = e, "prewarm batch failed");
            return;
        }
    };

    let responses: Vec<Value> = match serde_json::from_str(&resp_str) {
        Ok(v) => v,
        Err(_) => {
            warn!(pool = addr_norm, "prewarm: failed to parse batch response");
            return;
        }
    };

    if responses.len() < 5 {
        warn!(pool = addr_norm, "prewarm: incomplete batch response");
        return;
    }

    // slot0 response: 7 × 32-byte words
    let slot0_hex = match responses[0]["result"].as_str() {
        Some(s) if s.len() > 2 => s.trim_start_matches("0x"),
        _ => return,
    };
    // liq response: 1 × 32-byte word
    let liq_hex = match responses[1]["result"].as_str() {
        Some(s) if s.len() > 2 => s.trim_start_matches("0x"),
        _ => return,
    };
    // token0: 32-byte padded address (last 40 chars)
    let tok0_hex = match responses[2]["result"].as_str() {
        Some(s) if s.len() >= 42 => {
            let stripped = s.trim_start_matches("0x");
            &stripped[stripped.len().saturating_sub(40)..]
        }
        _ => return,
    };
    let tok1_hex = match responses[3]["result"].as_str() {
        Some(s) if s.len() >= 42 => {
            let stripped = s.trim_start_matches("0x");
            &stripped[stripped.len().saturating_sub(40)..]
        }
        _ => return,
    };
    let fee_hex = match responses[4]["result"].as_str() {
        Some(s) if s.len() > 2 => {
            let stripped = s.trim_start_matches("0x");
            u64::from_str_radix(stripped, 16).unwrap_or(3000)
        }
        _ => 3000,
    };

    // Decode sqrtPriceX96 (first 32 bytes = 64 chars) and tick (next 32 bytes, sign-extended)
    if slot0_hex.len() < 128 {
        return;
    }
    let sqrt_price = slot0_hex[..64].trim_start_matches('0');
    let sqrt_price = if sqrt_price.is_empty() { "0" } else { sqrt_price };

    let tick_word = &slot0_hex[64..128];
    let tick_val = decode_int24_from_padded_hex(tick_word);

    let liq_trimmed = liq_hex.trim_start_matches('0');
    let liq_trimmed = if liq_trimmed.is_empty() { "0" } else { liq_trimmed };

    // Store slot0 + liquidity
    let slot0_json = serde_json::json!({
        "sqrt_price_x96": sqrt_price,
        "tick": tick_val,
        "liquidity": liq_trimmed,
    })
    .to_string();

    // Store meta
    let meta_json = serde_json::json!({
        "token0": tok0_hex,
        "token1": tok1_hex,
        "fee": fee_hex,
        "is_v3": true,
    })
    .to_string();

    let mut redis = proxy.redis.clone();
    let slot0_key = CacheProxy::redis_key_slot0(&addr_norm);
    let meta_key = CacheProxy::redis_key_meta(&addr_norm);

    // Store with 10-minute TTL (background refresher will keep it fresh)
    let _: Result<(), _> = redis.set_ex(&slot0_key, &slot0_json, 600).await;
    let _: Result<(), _> = redis.set_ex(&meta_key, &meta_json, 86400).await;
}

/// Decode a two's-complement int24 from a 64-char (32-byte) padded hex word.
fn decode_int24_from_padded_hex(hex64: &str) -> i32 {
    // The value is right-aligned. Read as u32 (lower 24 bits = the int24).
    // Full 256-bit two's complement: if top bit of the 24-bit value is set, it's negative.
    let lower8 = &hex64[hex64.len().saturating_sub(8)..]; // last 4 bytes = 8 hex chars
    let raw = u32::from_str_radix(lower8, 16).unwrap_or(0);
    // int24 sign extension: if bit 23 is set, the value is negative
    if raw & 0x0080_0000 != 0 {
        // sign extend: set upper bits
        (raw | 0xFF00_0000) as i32
    } else {
        raw as i32
    }
}

/// Pre-warm pool state at startup by fetching all known top pools.
async fn prewarm_pools(proxy: &CacheProxy) {
    info!(pools = TOP_V3_POOLS.len(), "pre-warming pool state cache...");
    let mut ok = 0usize;
    let mut fail = 0usize;

    // Batch in groups of 5 to avoid hammering RPCs
    for chunk in TOP_V3_POOLS.chunks(5) {
        let futs: Vec<_> = chunk
            .iter()
            .map(|(addr, _is_v3, _label)| fetch_and_store_v3(proxy, addr))
            .collect();
        futures::future::join_all(futs).await;

        for (addr, _, label) in chunk {
            let key = CacheProxy::redis_key_slot0(&CacheProxy::normalise_addr(addr));
            let mut redis = proxy.redis.clone();
            match redis.exists::<_, bool>(&key).await {
                Ok(true) => {
                    ok += 1;
                    info!(pool = label, "pre-warmed");
                }
                _ => {
                    fail += 1;
                    warn!(pool = label, "pre-warm failed or empty");
                }
            }
        }

        // Small pause between batches to respect rate limits
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    }

    info!(ok, fail, "pool pre-warm complete");
}

// ─── Background pool state refresher ─────────────────────────────────────────

/// Every ~250ms, fetch updated slot0+liquidity for all tracked V3 pools and
/// write them to Redis. This keeps pool data ≤250ms stale, eliminating the
/// need for the bot to make its own pool-refresh RPC calls.
async fn pool_refresher(proxy: CacheProxy) {
    let mut last_block = 0u64;
    let mut refresh_count = 0u64;

    loop {
        tokio::time::sleep(std::time::Duration::from_millis(250)).await;

        // Check if block advanced
        let block_req = serde_json::json!({
            "jsonrpc": "2.0", "method": "eth_blockNumber", "params": [], "id": 99
        });
        let block_resp = match proxy.forward(&block_req.to_string()).await {
            Ok(r) => r,
            Err(_) => continue,
        };
        let block: u64 = if let Ok(v) = serde_json::from_str::<Value>(&block_resp) {
            if let Some(hex) = v["result"].as_str() {
                u64::from_str_radix(hex.trim_start_matches("0x"), 16).unwrap_or(0)
            } else {
                continue;
            }
        } else {
            continue;
        };

        if block <= last_block {
            continue;
        }
        last_block = block;
        proxy.maybe_invalidate(block);

        refresh_count += 1;

        // Refresh all tracked pools in parallel batches of 8
        for chunk in TOP_V3_POOLS.chunks(8) {
            let futs: Vec<_> = chunk
                .iter()
                .map(|(addr, _is_v3, _label)| fetch_and_store_v3(&proxy, addr))
                .collect();
            futures::future::join_all(futs).await;
        }

        if refresh_count % 40 == 0 {
            info!(block, pools = TOP_V3_POOLS.len(), "pool state refresh cycle");
        }
    }
}

// ─── Block poller & stats ─────────────────────────────────────────────────────

async fn block_poller(proxy: CacheProxy) {
    let req = serde_json::json!({
        "jsonrpc": "2.0", "method": "eth_blockNumber", "params": [], "id": 1
    });
    let body = req.to_string();

    loop {
        if let Ok(resp) = proxy.forward(&body).await {
            if let Ok(parsed) = serde_json::from_str::<Value>(&resp) {
                if let Some(hex) = parsed["result"].as_str() {
                    if let Ok(block) = u64::from_str_radix(hex.trim_start_matches("0x"), 16) {
                        proxy.maybe_invalidate(block);
                    }
                }
            }
        }
        tokio::time::sleep(std::time::Duration::from_millis(1000)).await;
    }
}

async fn stats_logger(proxy: CacheProxy) {
    loop {
        tokio::time::sleep(std::time::Duration::from_secs(60)).await;
        let mem = proxy.mem_hits.load(Ordering::Relaxed);
        let redis = proxy.redis_hits.load(Ordering::Relaxed);
        let pool = proxy.pool_hits.load(Ordering::Relaxed);
        let miss = proxy.misses.load(Ordering::Relaxed);
        let total = mem + redis + pool + miss;
        let hit_rate = if total > 0 {
            ((mem + redis + pool) as f64 / total as f64) * 100.0
        } else {
            0.0
        };
        let block = proxy.current_block.load(Ordering::Relaxed);
        let mem_entries = proxy.mem_cache.len();

        // Get Redis key count
        let mut redis_conn = proxy.redis.clone();
        let redis_keys: u64 = redis::cmd("DBSIZE")
            .query_async(&mut redis_conn)
            .await
            .unwrap_or(0);

        info!(
            mem_hits = mem,
            redis_hits = redis,
            pool_hits = pool,
            misses = miss,
            hit_rate = format!("{:.1}%", hit_rate),
            block,
            mem_entries,
            redis_keys,
            "cache stats"
        );
    }
}

// ─── Entry point ─────────────────────────────────────────────────────────────

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "rpc_cache=info".parse().unwrap()),
        )
        .init();

    let bind_addr: SocketAddr = std::env::var("RPC_BIND")
        .unwrap_or_else(|_| "127.0.0.1:8547".to_string())
        .parse()?;

    let redis_url = std::env::var("REDIS_URL")
        .unwrap_or_else(|_| "redis://127.0.0.1:6379/0".to_string());

    let upstreams: Vec<String> = std::env::var("RPC_UPSTREAMS")
        .unwrap_or_else(|_| {
            [
                "https://arbitrum-one-rpc.publicnode.com",
                "https://arbitrum.drpc.org",
                "https://arbitrum.meowrpc.com",
                "https://arb1.arbitrum.io/rpc",
            ]
            .join(",")
        })
        .split(',')
        .map(|s| s.trim().to_string())
        .collect();

    info!(?bind_addr, %redis_url, upstreams = upstreams.len(), "starting rpc-cache proxy");
    for (i, u) in upstreams.iter().enumerate() {
        info!(idx = i, url = u, "upstream");
    }

    let proxy = CacheProxy::new(upstreams, &redis_url).await;

    // Preload static cache from Redis
    {
        let mut redis = proxy.redis.clone();
        for method in STATIC_METHODS {
            let key = format!("rpc:{}", method);
            if let Ok(Some(val)) = redis.get::<_, Option<String>>(&key).await {
                proxy.static_cache.insert(key, val);
                info!(method, "preloaded from Redis");
            }
        }
    }

    info!("waiting for upstream connection...");
    let req_body = serde_json::json!({
        "jsonrpc": "2.0", "method": "eth_blockNumber", "params": [], "id": 1
    });
    loop {
        if let Ok(resp) = proxy.forward(&req_body.to_string()).await {
            if let Ok(parsed) = serde_json::from_str::<Value>(&resp) {
                if let Some(hex) = parsed["result"].as_str() {
                    if let Ok(block) = u64::from_str_radix(hex.trim_start_matches("0x"), 16) {
                        proxy.maybe_invalidate(block);
                        info!(block, "upstream connected");
                        break;
                    }
                }
            }
        }
        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
    }

    // Pre-warm pool state cache from upstream
    prewarm_pools(&proxy).await;

    // Spawn background tasks
    tokio::spawn(block_poller(proxy.clone()));
    tokio::spawn(stats_logger(proxy.clone()));
    tokio::spawn(pool_refresher(proxy.clone()));

    let listener = TcpListener::bind(bind_addr).await?;
    info!(%bind_addr, "rpc-cache listening (memory + Redis + pool state)");

    loop {
        let (stream, _addr) = listener.accept().await?;
        let proxy = proxy.clone();

        tokio::spawn(async move {
            let service = hyper::service::service_fn(move |req: Request<Incoming>| {
                let proxy = proxy.clone();
                async move {
                    let body_bytes = req.collect().await?.to_bytes();
                    let body_str = String::from_utf8_lossy(&body_bytes);

                    let response = proxy.handle_http(&body_str).await;

                    Ok::<_, hyper::Error>(
                        Response::builder()
                            .header("Content-Type", "application/json")
                            .header("Access-Control-Allow-Origin", "*")
                            .body(Full::new(bytes::Bytes::from(response)))
                            .unwrap(),
                    )
                }
            });

            if let Err(e) = http1::Builder::new()
                .serve_connection(TokioIo::new(stream), service)
                .await
            {
                if !e.to_string().contains("connection closed") {
                    warn!(error = %e, "connection error");
                }
            }
        });
    }
}
