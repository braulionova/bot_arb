use alloy::primitives::{address, Address, U256};
use alloy::providers::Provider;
use alloy::sol;
use eyre::Result;
use tracing::{info, warn};

use super::{Pool, PoolState};
use crate::decoder::DexType;

sol! {
    #[sol(rpc)]
    interface ICamelotPair {
        function token0() external view returns (address);
        function token1() external view returns (address);
        function getReserves() external view returns (uint112 reserve0, uint112 reserve1, uint16 token0FeePercent, uint16 token1FeePercent);
        function token0FeePercent() external view returns (uint16);
        function token1FeePercent() external view returns (uint16);
    }

    #[sol(rpc)]
    interface ICurvePool {
        function balances(uint256 i) external view returns (uint256);
        function A() external view returns (uint256);
        function fee() external view returns (uint256);
        function coins(uint256 i) external view returns (address);
        function get_dy(int128 i, int128 j, uint256 dx) external view returns (uint256);
    }

    #[sol(rpc)]
    interface IUniswapV2Factory {
        function allPairsLength() external view returns (uint256);
        function allPairs(uint256 index) external view returns (address);
        function getPair(address tokenA, address tokenB) external view returns (address pair);
    }

    #[sol(rpc)]
    interface IUniswapV2Pair {
        function token0() external view returns (address);
        function token1() external view returns (address);
        function getReserves() external view returns (uint112 reserve0, uint112 reserve1, uint32 blockTimestampLast);
    }

    #[sol(rpc)]
    interface IUniswapV3Factory {
        function getPool(address tokenA, address tokenB, uint24 fee) external view returns (address);
    }

    #[sol(rpc)]
    interface IUniswapV3Pool {
        function token0() external view returns (address);
        function token1() external view returns (address);
        function fee() external view returns (uint24);
        function slot0() external view returns (uint160 sqrtPriceX96, int24 tick, uint16 observationIndex, uint16 observationCardinality, uint16 observationCardinalityNext, uint8 feeProtocol, bool unlocked);
        function liquidity() external view returns (uint128);
    }
}

// ─── Factory addresses ───

pub const UNISWAP_V3_FACTORY: Address = address!("1F98431c8aD98523631AE4a59f267346ea31F984");
// NOTE: Camelot V3 uses Algebra protocol (different interface). Factory below is for reference only.
// Actual indexing requires IAlgebraFactory.poolByPair() + IAlgebraPool.globalState()
pub const CAMELOT_V3_FACTORY: Address = address!("1a3c9B1d2F0529e84FcE159b82A4E4C9Db632399");
pub const CAMELOT_V2_FACTORY: Address = address!("6EcCab422D763aC031210895C81787E87B43A652");
pub const SUSHISWAP_V3_FACTORY: Address = address!("1af415a1EbA07a4986a52B6f2e7dE7003D82231e");
pub const SUSHISWAP_V2_FACTORY: Address = address!("c35DADB65012eC5796536bD9864eD8773aBc74C4");
pub const TRADERJOE_V1_FACTORY: Address = address!("aE4EC9901c3076D0DdBe76A520F9E90a6227aCB7");
pub const PANCAKESWAP_V3_FACTORY: Address = address!("0BFbCF9fa4f9C56B0F40a671Ad40E0805A091865");
pub const ZYBERSWAP_V2_FACTORY: Address = address!("aC2ee06A14c52570Ef3B9812Ed240BCe359772e7");
pub const CHRONOS_V2_FACTORY: Address = address!("Ce9240869391928253Ed9cc9Bcb8cb98CB5B0722");
pub const UNISWAP_V2_FACTORY: Address = address!("f1D7CC64Fb4452F05c498126312eBE29f30Fbcf9");
pub const RAMSES_V2_FACTORY: Address = address!("AAA20D08e59F6561f242b08513D36266C5A29415");

// ─── Top tokens ───

pub const WETH: Address = address!("82aF49447D8a07e3bd95BD0d56f35241523fBab1");
pub const USDC: Address = address!("af88d065e77c8cC2239327C5EDb3A432268e5831");
pub const USDC_E: Address = address!("FF970A61A04b1cA14834A43f5dE4533eBDDB5CC8");
pub const USDT: Address = address!("Fd086bC7CD5C481DCC9C85ebE478A1C0b69FCbb9");
pub const WBTC: Address = address!("2f2a2543B76A4166549F7aaB2e75Bef0aefC5B0f");
pub const ARB: Address = address!("912CE59144191C1204E64559FE8253a0e49E6548");
pub const DAI: Address = address!("DA10009cBd5D07dd0CeCc66161FC93D7c9000da1");
pub const GMX: Address = address!("fc5A1A6EB076a2C7aD06eD22C90d7E710E35ad0a");
pub const LINK: Address = address!("f97f4df75117a78c1A5a0DBb814Af92458539FB4");
pub const UNI: Address = address!("Fa7F8980b0f1E64A2062791cc3b0871572f1F7f0");
pub const PENDLE: Address = address!("0c880f6761F1af8d9Aa9C466984b80DAb9a8c9e8");
pub const RDNT: Address = address!("3082CC23568eA640225c2467653dB90e9250AaA0");
pub const MAGIC: Address = address!("539bdE0d7Dbd336b79148AA742883198BBF60342");
pub const GRAIL: Address = address!("3d9907F9a368ad0a51Be60f7Da3b97cf940982D8");
pub const DPX: Address = address!("6C2C06790b3E3E3c38e12Ee22F8183b37a13EE55");
pub const STG: Address = address!("6694340fc020c5E6B96567843da2df01b2CE1eb6");
pub const JOE: Address = address!("371c7ec6D8039ff7933a2AA28EB827Ffe1F52f07");
pub const WSTETH: Address = address!("5979D7b546E38E414F7E9822514be443A4800529");
pub const FRAX: Address = address!("17FC002b466eEc40DaE837Fc4bE5c67993ddBd6F");
pub const JONES: Address = address!("10393c20975cF177a3513071bC110f7962CD67da");
pub const WINR: Address = address!("D77B108d4f6cefaa0Cae9506A934e825BEccA46E");
pub const SILO: Address = address!("0341C0C0ec423328042697530bd65EB2E4c32E6D");
pub const PREMIA: Address = address!("51fC0f6660482Ea73330E414eFd7808811a57Fa2");
pub const RAIN: Address = address!("25118290e6a5f4139381d072181157035864099d");
pub const YBR: Address = address!("11920f139a3121c2836e01551d43f95b3c31159c");
pub const VCNT: Address = address!("60bf4e7cf16ff34513514b968483b54beff42a81");
pub const WEETH: Address = address!("35751007a407ca6FEFfE80b3cB397736D2cf4dbe");
pub const GHO: Address = address!("7dfF72693f6A4149b17e7C6314655f6A9F7c8B33");

// ─── Curve stable pool addresses on Arbitrum ───

pub const CURVE_2POOL: Address = address!("7f90122BF0700F9E7e1F688fe926940E8839F353");      // USDC/USDT
pub const CURVE_TRICRYPTO: Address = address!("960ea3e3C7FB317332d990873d354E18d7645590");   // USDC/WBTC/WETH
pub const CURVE_USDC_USDCE: Address = address!("3aDf984c937FA6846a5ABDc261D19CEba31F8340"); // USDC/USDC.e
pub const CURVE_WSTETH_WETH: Address = address!("6eB2dc694eB516B16Dc9FBc678C60052BbdD7d80"); // wstETH/WETH
pub const CURVE_FRXETH_WETH: Address = address!("d9e8A84AEeBecaD23Cb6DBac3BE8F89D1D24F0b4"); // frxETH/WETH

// ─── Phase 1: ZERO-RPC instant startup from hardcoded pool metadata ───

/// Insert all known pools with full metadata — NO RPC calls needed.
/// Reserves/prices start at 0 and get filled by rpc-cache pool_refresher (250ms).
pub async fn index_priority_pools<P: Provider + Clone + 'static>(
    _provider: P,
    pool_state: &PoolState,
) -> Result<()> {
    info!("Phase 1: inserting hardcoded pools (zero RPC)...");

    // Top V3 pools by volume — hardcoded for instant availability
    // ── ALL POOLS WITH FULL METADATA — ZERO RPC CALLS ──
    // (pool_addr, dex, token0, token1, fee_bps, name)
    // Reserves start at 0 — rpc-cache pool_refresher fills them in 250ms
    let all_pools: &[(Address, DexType, Address, Address, u32, &str)] = &[
        // ── V3 pools (used for price reference, filtered by high-latency filter) ──
        (address!("C6962004f452bE9203591991D15f6b388e09E8D0"), DexType::UniswapV3, WETH, USDC, 5, "UniV3 WETH/USDC 0.05%"),
        (address!("C31E54c7a869B9FcBEcc14363CF510d1c41fa443"), DexType::UniswapV3, WETH, USDC, 30, "UniV3 WETH/USDC 0.3%"),
        (address!("6f38e884725a116C9C7fBF208e79FE8828a2595F"), DexType::UniswapV3, WETH, USDC, 1, "UniV3 WETH/USDC 0.01%"),
        (address!("2f5e87C9312fa29aed5c179E456625D79015299c"), DexType::UniswapV3, WETH, USDT, 5, "UniV3 WETH/USDT 0.05%"),
        (address!("80A9ae39310abf666A87C743d6ebBD0E8C42158E"), DexType::UniswapV3, WETH, ARB, 5, "UniV3 WETH/ARB 0.05%"),
        (address!("641C00A822e8b671738d32a431a4Fb6074E5c79d"), DexType::UniswapV3, WETH, ARB, 30, "UniV3 WETH/ARB 0.3%"),
        (address!("d845f7D4f4DeB9Ff3bCeCe5A4E2D2B3f74b22Dc4"), DexType::UniswapV3, WETH, WBTC, 5, "UniV3 WETH/WBTC 0.05%"),
        (address!("2391DDC81Cd63aAEaD9BDe63B00bB63e60DdBE9c"), DexType::UniswapV3, USDC, USDT, 1, "UniV3 USDC/USDT 0.01%"),
        (address!("faE2AE0a9f87FD35b5b0E24B47BAC796A7EEfEa1"), DexType::UniswapV3, ARB, USDC, 5, "UniV3 ARB/USDC 0.05%"),
        (address!("35218a1cbaC5Bbc3E57fd9Bd38219D37571b3537"), DexType::UniswapV3, WSTETH, WETH, 1, "UniV3 wstETH/WETH 0.01%"),
        (address!("1aEEdD3727A6431b8F070C0aFaA81Cc74f273882"), DexType::UniswapV3, GMX, WETH, 30, "UniV3 GMX/WETH 0.3%"),
        (address!("a2AE929bfFbDA42eA0cdA0a62f7E38a20105f313"), DexType::UniswapV3, LINK, WETH, 30, "UniV3 LINK/WETH 0.3%"),
        (address!("dbaEb7f0DFe3a0AAFD798CCECB5b22E708f7852c"), DexType::UniswapV3, USDC_E, USDC, 1, "UniV3 USDC.e/USDC 0.01%"),
        // SushiSwap V3
        (address!("18D3284d9EFf64Fc97b64aB2b871738677AE3632"), DexType::SushiSwapV3, WETH, USDC, 5, "SushiV3 WETH/USDC 0.05%"),
        // PancakeSwap V3
        (address!("7fCdC35463E3770c2fB992716Cd070B63540b947"), DexType::PancakeSwapV3, WETH, USDC, 1, "PcsV3 WETH/USDC 0.01%"),
        (address!("d9e2A1a61B6E61b275cEc326465d417e52C1b95c"), DexType::PancakeSwapV3, WETH, USDC, 5, "PcsV3 WETH/USDC 0.05%"),
        (address!("7e928afb59f5dE9D2f4d162f754c6eB40C88Aa8e"), DexType::PancakeSwapV3, USDT, USDC, 1, "PcsV3 USDT/USDC 0.01%"),
        (address!("93CCE474015007b38dA0ecea96671Ee4dC3d40AD"), DexType::PancakeSwapV3, ARB, USDC, 1, "PcsV3 ARB/USDC 0.01%"),
        (address!("0d7c4b40018969f81750D0A164c3839a77353EFB"), DexType::PancakeSwapV3, ARB, WETH, 5, "PcsV3 ARB/WETH 0.05%"),
        // ── LOW-COMPETITION V2 — our edge (Camelot, Sushi) ──
        (address!("54B26fAf3671677C19F70c4B879A6f7B898F732c"), DexType::CamelotV2, WETH, USDC, 30, "Camelot WETH/USDC"),
        (address!("97b192198d164C2a1834295e302B713bc32C8F1d"), DexType::CamelotV2, WETH, USDT, 30, "Camelot WETH/USDT"),
        (address!("a6c5C7D189fA4eB5Af8ba34E63dCDD3a635D433f"), DexType::CamelotV2, WETH, ARB, 30, "Camelot WETH/ARB"),
        (address!("E8b2C9cBfd52CF9A157724e6416440566fA03150"), DexType::CamelotV2, WETH, MAGIC, 30, "Camelot WETH/MAGIC"),
        (address!("dc2167F4A5DeC5401EcEFF1CB55C3573A13F24bD"), DexType::CamelotV2, WETH, GMX, 30, "Camelot WETH/GMX"),
        (address!("f82105aA473560CfBF8Cbc6Fd83dB14Eb4028117"), DexType::CamelotV2, WETH, GRAIL, 30, "Camelot WETH/GRAIL"),
        (address!("BfCa4230115DE8341F3A3d5e8845fFb3337B2Be3"), DexType::CamelotV2, WETH, PENDLE, 30, "Camelot WETH/PENDLE"),
        (address!("65Cfd8fB82213971076457756dFEdB6143391983"), DexType::CamelotV2, WETH, LINK, 30, "Camelot WETH/LINK"),
        (address!("928916c247df3c3dBB09a4cd0Af4d6fB9E1752Ad"), DexType::CamelotV2, WETH, DPX, 30, "Camelot WETH/DPX"),
        (address!("96059759C6492fb4e8a9777b65f307F2C811a34F"), DexType::CamelotV2, WETH, WBTC, 30, "Camelot WETH/WBTC"),
        (address!("57b85FEf094e10b5eeCDF350Af688299E9553378"), DexType::SushiSwapV2, WETH, USDC, 30, "Sushi WETH/USDC"),
        (address!("B7E50106A5bd3Cf21AF210A755F9C8740890A8c9"), DexType::SushiSwapV2, WETH, MAGIC, 30, "Sushi WETH/MAGIC"),
        (address!("0C1Cf6883efA1B496B01f654E247B9b419873054"), DexType::SushiSwapV2, WETH, DPX, 30, "Sushi WETH/DPX"),
        (address!("7050A8908E2a60899D8788015148241f0993a3FD"), DexType::SushiSwapV2, WETH, LINK, 30, "Sushi WETH/LINK"),
        // ── HOT POOLS from on-chain arb analysis (where real arbs happen) ──
        // #1 most arbed: CamelotV3 (Algebra) WETH/USDC — 45 arbs observed
        (address!("b1026b8e7276e7ac75410f1fcbbe21796e8f7526"), DexType::CamelotV3, WETH, USDC, 5, "CamelotV3 WETH/USDC"),
        // #2: UniV3 WETH/USDC 0.05% (different from 0xC696) — 20 arbs
        (address!("f3Eb87C1F6020982173C908E7eB31aA66c1f0296"), DexType::UniswapV3, WETH, USDC, 5, "UniV3-B WETH/USDC 0.05%"),
        // USDC/USDC.e bridge — 11 arbs
        (address!("c86eb7b85807020b4548ee05b54bfc956eebbfcd"), DexType::CamelotV3, USDC, USDC_E, 1, "CamelotV3 USDC/USDC.e"),
        // RAIN/WETH cross-fee — 6+17 arbs (0.01% and 0.05%)
        (address!("d13040d4fe917ee704158cfcb3338dcd2838b245"), DexType::UniswapV3, RAIN, WETH, 1, "UniV3 RAIN/WETH 0.01%"),
        (address!("3bf5960990576b658dce513027e3466fcff1eb72"), DexType::UniswapV3, RAIN, WETH, 5, "UniV3 RAIN/WETH 0.05%"),
        // YBR/USDT — 9 arbs
        (address!("b18e2e8b2f6c4f3f2e5afc1229d9d7654b0ddaa3"), DexType::UniswapV3, YBR, USDT, 30, "UniV3 YBR/USDT 0.3%"),
        // VCNT/USDC + VCNT/WETH — 7+6 arbs (triangular)
        (address!("ec8151f44c57a2c1b9bdfd22fcf5054983542197"), DexType::UniswapV3, VCNT, USDC, 30, "UniV3 VCNT/USDC 0.3%"),
        (address!("7db52bd874148a3cf32e7a53b2d1e0d75c94f1c4"), DexType::UniswapV3, VCNT, WETH, 30, "UniV3 VCNT/WETH 0.3%"),
        // PENDLE/WETH — V2+V3
        (address!("b08a8794a5d3ccca3725d92964696858d3201909"), DexType::UniswapV3, PENDLE, WETH, 5, "UniV3 PENDLE/WETH 0.05%"),
        // wstETH/WETH extra pools
        (address!("d845f7d4f4deb9ff5bcf09d140ef13718f6f6c71"), DexType::CamelotV3, WBTC, WETH, 5, "CamelotV3 WBTC/WETH"),
        // USDC/USDT — multiple pools for stable arb
        (address!("bce73c2e5a623054b0e8e2428e956f4b9d0412a5"), DexType::UniswapV3, USDC, USDT, 5, "UniV3 USDC/USDT 0.05%"),
        // LINK pools across DEXes
        (address!("32a5746ba682cdca465eda5a25d81f3d9a8f6b49"), DexType::UniswapV3, LINK, USDT, 30, "UniV3 LINK/USDT 0.3%"),
    ];

    for (addr, dex, t0, t1, fee, name) in all_pools {
        pool_state.insert_pool(Pool {
            address: *addr,
            dex: *dex,
            token0: *t0,
            token1: *t1,
            reserve0: U256::ZERO,
            reserve1: U256::ZERO,
            fee_bps: *fee,
            sqrt_price_x96: None,
            tick: None,
            liquidity: None,
            fee_bps_token0: None,
            fee_bps_token1: None,
        }).await;
    }

    let total = pool_state.pool_count().await;
    info!(total, "Phase 1 complete — {} pools loaded instantly (zero RPC)", total);
    Ok(())
}

/// Index a single V2 pool by address (hardcoded, no factory needed)
async fn index_single_v2_pool<P: Provider + Clone>(
    provider: P,
    pair_addr: Address,
    dex: DexType,
    default_fee_bps: u32,
    pool_state: &PoolState,
) -> Result<bool> {
    if dex == DexType::CamelotV2 {
        // Camelot V2 has different getReserves signature: (uint112, uint112, uint16, uint16)
        // Retry up to 3 times (public RPCs rotate and some don't support eth_call)
        let mut token0 = Address::ZERO;
        let mut token1 = Address::ZERO;
        let mut r0 = 0u128;
        let mut r1 = 0u128;
        let mut f0 = 0u16;
        let mut f1 = 0u16;
        let mut success = false;
        for _ in 0..3 {
            let pair = ICamelotPair::new(pair_addr, &provider);
            let t0_b = pair.token0();
            let t1_b = pair.token1();
            let r_b = pair.getReserves();
            match tokio::try_join!(t0_b.call(), t1_b.call(), r_b.call()) {
                Ok((t0, t1, reserves)) => {
                    token0 = t0;
                    token1 = t1;
                    r0 = reserves.reserve0.to::<u128>();
                    r1 = reserves.reserve1.to::<u128>();
                    f0 = reserves.token0FeePercent;
                    f1 = reserves.token1FeePercent;
                    if r0 > 0 && r1 > 0 {
                        success = true;
                        break;
                    }
                }
                Err(_) => {
                    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
                }
            }
        }
        if !success || r0 == 0 || r1 == 0 {
            return Ok(false);
        }
        let reserves = (r0, r1, f0, f1);

        // Directional fees from getReserves response
        let bps0 = (reserves.2 as u32).max(1);
        let bps1 = (reserves.3 as u32).max(1);
        let eff = bps0.max(bps1);

        pool_state.insert_pool(Pool {
            address: pair_addr,
            dex,
            token0,
            token1,
            reserve0: U256::from(reserves.0),
            reserve1: U256::from(reserves.1),
            fee_bps: eff,
            sqrt_price_x96: None,
            tick: None,
            liquidity: None,
            fee_bps_token0: Some(bps0),
            fee_bps_token1: Some(bps1),
        }).await;

        return Ok(true);
    }

    // Standard V2 (Sushi, Ramses, etc.)
    let pair = IUniswapV2Pair::new(pair_addr, &provider);
    let t0_b = pair.token0();
    let t1_b = pair.token1();
    let r_b = pair.getReserves();
    let (token0, token1, reserves) = match tokio::try_join!(
        t0_b.call(), t1_b.call(), r_b.call(),
    ) {
        Ok(v) => v,
        Err(_) => return Ok(false),
    };

    if reserves.reserve0 == 0 || reserves.reserve1 == 0 {
        return Ok(false);
    }

    pool_state.insert_pool(Pool {
        address: pair_addr,
        dex,
        token0,
        token1,
        reserve0: U256::from(reserves.reserve0),
        reserve1: U256::from(reserves.reserve1),
        fee_bps: default_fee_bps,
        sqrt_price_x96: None,
        tick: None,
        liquidity: None,
        fee_bps_token0: None,
        fee_bps_token1: None,
    }).await;

    Ok(true)
}

// ─── Curve pool indexing ───

/// Index hardcoded Curve stable pools on Arbitrum.
/// Returns the number of pools successfully indexed.
pub async fn index_curve_pools<P: Provider + Clone + 'static>(
    provider: P,
    pool_state: &PoolState,
) -> Result<usize> {
    // Only index 2-token Curve pools (tricrypto is 3-token, needs special handling)
    let curve_pools: &[(Address, &str)] = &[
        (CURVE_2POOL,       "2pool USDC/USDT"),
        (CURVE_USDC_USDCE,  "USDC/USDC.e"),
        (CURVE_WSTETH_WETH, "wstETH/WETH"),
        (CURVE_FRXETH_WETH, "frxETH/WETH"),
    ];

    let mut indexed = 0usize;

    for (pool_addr, name) in curve_pools {
        let c = ICurvePool::new(*pool_addr, &provider);

        let coins0_b = c.coins(U256::ZERO);
        let coins1_b = c.coins(U256::from(1u64));
        let bal0_b   = c.balances(U256::ZERO);
        let bal1_b   = c.balances(U256::from(1u64));
        let fee_b    = c.fee();

        let result = tokio::try_join!(
            coins0_b.call(),
            coins1_b.call(),
            bal0_b.call(),
            bal1_b.call(),
            fee_b.call(),
        );

        match result {
            Ok((token0, token1, balance0, balance1, fee)) => {
                if balance0.is_zero() || balance1.is_zero() {
                    warn!(pool = name, "Curve pool has zero balances, skipping");
                    continue;
                }

                // Curve fee() returns value in 1e10 format.
                // Convert to basis points: fee_bps = fee * 10000 / 1e10
                let fee_bps = (fee * U256::from(10000u64) / U256::from(10_000_000_000u64))
                    .try_into()
                    .unwrap_or(4u32);
                // Clamp to sensible range (Curve stable pools: 1–10 bps)
                let fee_bps = if fee_bps == 0 { 4 } else { fee_bps };

                pool_state.insert_pool(Pool {
                    address: *pool_addr,
                    dex: DexType::CurveStable,
                    token0,
                    token1,
                    reserve0: balance0,
                    reserve1: balance1,
                    fee_bps,
                    sqrt_price_x96: None,
                    tick: None,
                    liquidity: None,
                    fee_bps_token0: None,
                    fee_bps_token1: None,
                }).await;

                info!(pool = name, fee_bps, "Indexed Curve pool");
                indexed += 1;
            }
            Err(e) => {
                warn!(pool = name, error = %e, "Failed to index Curve pool");
            }
        }
    }

    Ok(indexed)
}

// ─── Phase 2: Background indexing (runs while bot trades) ───

/// Gradually index more pools in background, by priority.
/// Adds ~50 pools per batch with 2s delay between batches to respect rate limits.
pub async fn index_background<P: Provider + Clone + 'static>(
    provider: P,
    pool_state: PoolState,
) {
    info!("Phase 2: background pool indexing started");

    // Priority 1: V2 factories — MORE pools = MORE cross-DEX arb opportunities
    // V2↔V2 arbs are the most profitable (CamelotV2↔SushiV2 = $14K in 1 trade)
    let v2_factories = [
        // High priority: most pools and volume
        (UNISWAP_V2_FACTORY, DexType::UniswapV2, "Uniswap V2", 1000u64),
        (CAMELOT_V2_FACTORY, DexType::CamelotV2, "Camelot V2", 500),
        (SUSHISWAP_V2_FACTORY, DexType::SushiSwapV2, "SushiSwap V2", 500),
        (RAMSES_V2_FACTORY, DexType::RamsesV2, "Ramses V2", 300),
        // Lower priority
        (TRADERJOE_V1_FACTORY, DexType::UniswapV2, "TraderJoe V1", 200),
        (ZYBERSWAP_V2_FACTORY, DexType::UniswapV2, "Zyberswap V2", 100),
        (CHRONOS_V2_FACTORY, DexType::UniswapV2, "Chronos V2", 100),
    ];

    for (factory_addr, dex, name, max) in &v2_factories {
        match index_v2_factory_throttled(
            provider.clone(), *factory_addr, *dex, &pool_state, *max,
        ).await {
            Ok(count) => {
                let total = pool_state.pool_count().await;
                info!(count, dex = *name, total, "+V2 pools");
            }
            Err(e) => {
                let msg = e.to_string();
                warn!(error = msg, dex = *name, "V2 factory failed");
            }
        }
        tokio::time::sleep(std::time::Duration::from_secs(5)).await;
    }

    // Priority 2: V3 factory discovery (finds pools not in hardcoded list)
    let v3_factories = [
        (UNISWAP_V3_FACTORY, DexType::UniswapV3, "Uniswap V3"),
        (PANCAKESWAP_V3_FACTORY, DexType::PancakeSwapV3, "PancakeSwap V3"),
        (SUSHISWAP_V3_FACTORY, DexType::SushiSwapV3, "SushiSwap V3"),
        // NOTE: Camelot V3 removed — uses Algebra interface, indexed via hardcoded pools above
    ];

    let top_tokens = &[WETH, USDC, USDC_E, USDT, WBTC, ARB, GMX, DAI, LINK, UNI, PENDLE, RDNT, MAGIC, GRAIL, DPX, STG, JOE, WSTETH, FRAX, JONES, WINR, SILO, PREMIA, RAIN, YBR, VCNT, WEETH, GHO];
    let fees = &[100u32, 500, 3000, 10000];

    for (factory_addr, dex, name) in &v3_factories {
        match index_v3_factory_throttled(
            provider.clone(), *factory_addr, *dex, &pool_state, top_tokens, fees,
        ).await {
            Ok(count) => {
                let total = pool_state.pool_count().await;
                info!(count, dex = *name, total, "+V3 pools");
            }
            Err(e) => {
                let msg = e.to_string();
                warn!(error = msg, dex = *name, "V3 factory failed");
            }
        }
        tokio::time::sleep(std::time::Duration::from_secs(5)).await;
    }

    // Priority 3: Long-tail V2 pairs — manually check high-value pairs
    let long_tail_pairs = [
        // WETH pairs — primary liquidity
        (MAGIC, WETH), (GRAIL, WETH), (RDNT, WETH), (DPX, WETH),
        (PENDLE, WETH), (STG, WETH), (JOE, WETH), (WSTETH, WETH),
        (JONES, WETH), (WINR, WETH), (SILO, WETH), (PREMIA, WETH),
        (UNI, WETH), (LINK, WETH), (FRAX, WETH),
        // USDC pairs — secondary liquidity
        (MAGIC, USDC), (RDNT, USDC), (PENDLE, USDC), (GMX, USDC),
        (ARB, USDC), (LINK, USDC), (STG, USDC), (JONES, USDC),
        (GRAIL, USDC), (DPX, USDC), (WBTC, USDC),
        // ARB pairs
        (MAGIC, ARB), (GMX, ARB), (PENDLE, ARB), (RDNT, ARB),
        (GRAIL, ARB), (JONES, ARB),
        // USDT pairs
        (WETH, USDT), (ARB, USDT), (WBTC, USDT),
    ];

    let v2_factories_for_longtail = [
        (UNISWAP_V2_FACTORY, DexType::UniswapV2),
        (CAMELOT_V2_FACTORY, DexType::CamelotV2),
        (SUSHISWAP_V2_FACTORY, DexType::SushiSwapV2),
        (RAMSES_V2_FACTORY, DexType::RamsesV2),
        (TRADERJOE_V1_FACTORY, DexType::UniswapV2),
    ];

    let mut lt_count = 0usize;
    for (token_a, token_b) in &long_tail_pairs {
        for (factory_addr, dex) in &v2_factories_for_longtail {
            let factory = IUniswapV2Factory::new(*factory_addr, &provider);
            // IUniswapV2Factory.getPair — use allPairs enumeration is slow;
            // instead call getPair via the standard factory interface.
            // IUniswapV2Factory has no getPair in the existing sol! definition,
            // so we query via the pair address from getPool pattern.
            // We use a separate sol! call via a helper.
            match index_longtail_v2_pair(provider.clone(), *factory_addr, *dex, *token_a, *token_b, &pool_state).await {
                Ok(true) => lt_count += 1,
                _ => {}
            }
        }
    }
    {
        let total = pool_state.pool_count().await;
        info!(lt_count, total, "+long-tail V2 pairs");
    }

    // Curve stable pools
    match index_curve_pools(provider.clone(), &pool_state).await {
        Ok(count) => {
            let total = pool_state.pool_count().await;
            info!(count, total, "+Curve stable pools");
        }
        Err(e) => {
            warn!(error = %e, "Curve pool indexing failed");
        }
    }

    let total = pool_state.pool_count().await;
    info!(total, "Phase 2 complete — all pools indexed");
}

// ─── Implementation helpers ───

pub async fn index_single_v3_pool<P: Provider + Clone>(
    provider: P,
    pool_addr: Address,
    dex: DexType,
    pool_state: &PoolState,
) -> Result<bool> {
    let c = IUniswapV3Pool::new(pool_addr, &provider);
    let t0_b = c.token0();
    let t1_b = c.token1();
    let s0_b = c.slot0();
    let liq_b = c.liquidity();
    let fee_b = c.fee();

    let (token0, token1, slot0, liq, fee) = match tokio::try_join!(
        t0_b.call(), t1_b.call(), s0_b.call(), liq_b.call(), fee_b.call(),
    ) {
        Ok(v) => v,
        Err(_) => return Ok(false),
    };

    if liq == 0 {
        return Ok(false);
    }

    // Skip pools with 0 fee (Algebra/Camelot V3 return 0 from fee())
    if fee == 0 {
        return Ok(false);
    }

    pool_state.insert_pool(Pool {
        address: pool_addr,
        dex,
        token0,
        token1,
        reserve0: U256::ZERO,
        reserve1: U256::ZERO,
        fee_bps: u32::try_from(fee).unwrap_or(3000) / 100,
        sqrt_price_x96: Some(U256::from(slot0.sqrtPriceX96)),
        tick: Some(slot0.tick.as_i32()),
        liquidity: Some(liq),
        fee_bps_token0: None,
        fee_bps_token1: None,
    }).await;

    Ok(true)
}

async fn index_v2_factory_throttled<P: Provider + Clone>(
    provider: P,
    factory_addr: Address,
    dex: DexType,
    pool_state: &PoolState,
    max_pairs: u64,
) -> Result<usize> {
    let factory = IUniswapV2Factory::new(factory_addr, &provider);
    let total_pairs: u64 = factory.allPairsLength().call().await?.try_into().unwrap_or(0);
    let cap = total_pairs.min(max_pairs);
    let mut indexed = 0usize;

    for i in 0..cap {
        let pair_addr = match factory.allPairs(U256::from(i)).call().await {
            Ok(addr) => addr,
            Err(_) => continue,
        };

        let pair = IUniswapV2Pair::new(pair_addr, &provider);
        let t0_b = pair.token0();
        let t1_b = pair.token1();
        let r_b = pair.getReserves();
        let (token0, token1, reserves) = match tokio::try_join!(
            t0_b.call(), t1_b.call(), r_b.call(),
        ) {
            Ok(v) => v,
            Err(_) => continue,
        };

        if reserves.reserve0 == 0 || reserves.reserve1 == 0 {
            continue;
        }

        // For Camelot V2: read directional fees
        let (fee_bps, fee_bps_token0, fee_bps_token1) = if dex == DexType::CamelotV2 {
            read_camelot_fees(provider.clone(), pair_addr).await
        } else {
            (30u32, None, None)
        };

        pool_state.insert_pool(Pool {
            address: pair_addr,
            dex,
            token0,
            token1,
            reserve0: U256::from(reserves.reserve0),
            reserve1: U256::from(reserves.reserve1),
            fee_bps,
            sqrt_price_x96: None,
            tick: None,
            liquidity: None,
            fee_bps_token0,
            fee_bps_token1,
        }).await;

        indexed += 1;

        // Throttle: pause every 20 pools to stay under rate limits
        if indexed % 20 == 0 {
            tokio::time::sleep(std::time::Duration::from_secs(2)).await;
        }
    }

    Ok(indexed)
}

async fn index_v3_factory_throttled<P: Provider + Clone>(
    provider: P,
    factory_addr: Address,
    dex: DexType,
    pool_state: &PoolState,
    tokens: &[Address],
    fees: &[u32],
) -> Result<usize> {
    let factory = IUniswapV3Factory::new(factory_addr, &provider);
    let mut indexed = 0usize;

    for i in 0..tokens.len() {
        for j in (i + 1)..tokens.len() {
            for &fee in fees {
                let pool_addr = match factory
                    .getPool(tokens[i], tokens[j], fee.try_into().unwrap())
                    .call().await
                {
                    Ok(addr) if addr != Address::ZERO => addr,
                    _ => continue,
                };

                // Skip if already indexed
                if pool_state.has_pool(pool_addr).await {
                    continue;
                }

                match index_single_v3_pool(provider.clone(), pool_addr, dex, pool_state).await {
                    Ok(true) => indexed += 1,
                    _ => {}
                }

                // Throttle
                if indexed % 10 == 0 && indexed > 0 {
                    tokio::time::sleep(std::time::Duration::from_secs(2)).await;
                }
            }
        }
    }

    Ok(indexed)
}

/// Read directional fees for a Camelot V2 pair.
/// Returns (effective_fee_bps, fee_bps_token0, fee_bps_token1).
/// Camelot fees are stored in basis points / 100 (e.g. 300 = 0.3%).
async fn read_camelot_fees<P: Provider + Clone>(
    provider: P,
    pair_addr: Address,
) -> (u32, Option<u32>, Option<u32>) {
    let pair = ICamelotPair::new(pair_addr, &provider);
    let b0 = pair.token0FeePercent();
    let b1 = pair.token1FeePercent();
    match tokio::try_join!(b0.call(), b1.call()) {
        Ok((f0, f1)) => {
            // Camelot feePercent is in 1/100 of a basis point (e.g. 30000 = 0.3%)
            // Divide by 1000 to get basis points
            let bps0 = (f0 as u32).saturating_add(999) / 1000;
            let bps1 = (f1 as u32).saturating_add(999) / 1000;
            let effective = bps0.max(bps1).max(1);
            (effective, Some(bps0.max(1)), Some(bps1.max(1)))
        }
        Err(_) => (30u32, None, None),
    }
}

/// Check a specific V2 factory for a token pair using getPair, index it if found.
async fn index_longtail_v2_pair<P: Provider + Clone>(
    provider: P,
    factory_addr: Address,
    dex: DexType,
    token_a: Address,
    token_b: Address,
    pool_state: &PoolState,
) -> Result<bool> {
    let factory = IUniswapV2Factory::new(factory_addr, &provider);
    let pair_addr = match factory.getPair(token_a, token_b).call().await {
        Ok(addr) if addr != Address::ZERO => addr,
        _ => return Ok(false),
    };

    // Skip if already indexed
    if pool_state.has_pool(pair_addr).await {
        return Ok(false);
    }

    let pair = IUniswapV2Pair::new(pair_addr, &provider);
    let t0_b = pair.token0();
    let t1_b = pair.token1();
    let r_b = pair.getReserves();

    let (token0, token1, reserves) = match tokio::try_join!(t0_b.call(), t1_b.call(), r_b.call()) {
        Ok(v) => v,
        Err(_) => return Ok(false),
    };

    if reserves.reserve0 == 0 || reserves.reserve1 == 0 {
        return Ok(false);
    }

    let (fee_bps, fee_bps_token0, fee_bps_token1) = if dex == DexType::CamelotV2 {
        read_camelot_fees(provider.clone(), pair_addr).await
    } else {
        (30u32, None, None)
    };

    pool_state.insert_pool(Pool {
        address: pair_addr,
        dex,
        token0,
        token1,
        reserve0: U256::from(reserves.reserve0),
        reserve1: U256::from(reserves.reserve1),
        fee_bps,
        sqrt_price_x96: None,
        tick: None,
        liquidity: None,
        fee_bps_token0,
        fee_bps_token1,
    }).await;

    info!(%pair_addr, ?dex, %token_a, %token_b, "Indexed long-tail V2 pair");
    Ok(true)
}
