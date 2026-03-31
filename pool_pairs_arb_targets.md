# Arbitrum One: Cross-DEX Arbitrage Pool Targets

Research date: 2026-03-28

---

## Token Addresses (Arbitrum One)

| Token  | Address                                      |
|--------|----------------------------------------------|
| WETH   | `0x82aF49447D8a07e3bd95BD0d56f35241523fBab1` |
| USDC   | `0xaf88d065e77c8cC2239327C5EDb3A432268e5831` (native Circle) |
| USDC.e | `0xFF970A61A04b1cA14834A43f5dE4533eBDDB5CC8` (bridged) |
| USDT   | `0xFd086bC7CD5C481DCC9C85ebE478A1C0b69FCbb9` |
| WBTC   | `0x2f2a2543B76A4166549F7aaB2e75Bef0aefC5B0f` |
| ARB    | `0x912CE59144191C1204E64559FE8253a0e49E6548` |

## DEX Factory Addresses

| DEX           | Factory Address                                |
|---------------|------------------------------------------------|
| Uniswap V3    | `0x1F98431c8aD98523631AE4a59f267346ea31F984`   |
| SushiSwap V3  | `0x1af415a1EbA07a4986a52B6f2e7dE7003D82231e`   |
| PancakeSwap V3| `0x0BFbCF9fa4f9C56B0F40a671Ad40E0805A091865`   |
| Camelot V3    | Algebra-based (dynamic fees)                    |

---

## CROSS-DEX ARB TARGETS (same pair, different DEX)

### TIER 1 - BEST: Ultra-low combined fees (<=0.02%)

These are the most profitable opportunities because both legs have very low fees.

#### WETH/USDC (0.01% + 0.01% = 0.02% total fee)

| DEX            | Pool Address                                   | Fee   | TVL       |
|----------------|------------------------------------------------|-------|-----------|
| Uniswap V3     | `0x6f38e884725a116c9c7fbf208e79fe8828a2595f`   | 0.01% | ~$155K    |
| PancakeSwap V3 | `0x7fCDc35463E3770c2fB992716Cd070B63540b947`   | 0.01% | moderate  |
| Camelot V3     | `0xb1026b8e7276e7ac75410f1fcbbe21796e8f7526`   | 0.01% (dynamic) | ~$516K |

**3 DEXes with 0.01% fee on the same pair = prime arb territory.**
Any 2-leg combo costs only 0.02% total. Need >0.02% spread to profit.

#### USDT/USDC (0.01% + 0.01% = 0.02% total fee)

| DEX            | Pool Address                                   | Fee   | TVL       |
|----------------|------------------------------------------------|-------|-----------|
| Uniswap V3     | `0x8c9d230d45d6cfee39a6680fb7cb7e8de7ea8e71`   | 0.01% | ~$194K    |
| SushiSwap V3   | `0xd9e96f78b3c68ba79fd4dfad4ddf4f27bd1e2ecf`   | 0.01% | ~$1.1K    |
| PancakeSwap V3 | `0x7e928afb59f5de9d2f4d162f754c6eb40c88aa8e`   | 0.01% | moderate  |

Stablecoin pair - very tight spreads but very low fee overhead.

#### WETH/USDT (0.01% + 0.01% = 0.02% total fee)

| DEX            | Pool Address                                   | Fee   | TVL       |
|----------------|------------------------------------------------|-------|-----------|
| Uniswap V3     | `0x42161084d0672e1d3f26a9b53e653be2084ff19c`   | 0.01% | ~$15K     |
| Camelot V3     | `0x7cccba38e2d959fe135e79aebb57ccb27b128358`   | 0.015% (dynamic) | ~$33K |

---

### TIER 2 - GOOD: Low combined fees (0.02% - 0.10%)

#### WETH/USDC across different fee tiers

| DEX            | Pool Address                                   | Fee   | TVL       |
|----------------|------------------------------------------------|-------|-----------|
| Uniswap V3     | `0xC6962004f452bE9203591991D15f6b388e09E8D0`   | 0.05% | ~$52M     |
| Uniswap V3     | `0xc31e54c7a869b9fcbecc14363cf510d1c41fa443`   | 0.05% | moderate  |
| Uniswap V3     | `0x6f38e884725a116c9c7fbf208e79fe8828a2595f`   | 0.01% | ~$155K    |
| PancakeSwap V3 | `0x7fCDc35463E3770c2fB992716Cd070B63540b947`   | 0.01% | moderate  |
| Camelot V3     | `0xb1026b8e7276e7ac75410f1fcbbe21796e8f7526`   | 0.01% | ~$516K    |
| Camelot V3     | `0x84652bb2539513baf36e225c930fdd8eaa63ce27`   | dynamic | ~$157K  |

Best combo: Uniswap V3 0.01% <-> PancakeSwap V3 0.01% = 0.02% total
Also good: Uniswap V3 0.01% <-> Camelot V3 0.01% = 0.02% total

#### WETH/USDT across DEXes

| DEX            | Pool Address                                   | Fee   | TVL       |
|----------------|------------------------------------------------|-------|-----------|
| Uniswap V3     | `0x641c00a822e8b671738d32a431a4fb6074e5c79d`   | 0.05% | ~$13M     |
| Uniswap V3     | `0x42161084d0672e1d3f26a9b53e653be2084ff19c`   | 0.01% | ~$15K     |
| Camelot V3     | `0x7cccba38e2d959fe135e79aebb57ccb27b128358`   | 0.015%| ~$33K     |

#### ARB/WETH across DEXes

| DEX            | Pool Address                                   | Fee   | TVL       |
|----------------|------------------------------------------------|-------|-----------|
| Uniswap V3     | `0xC6F780497A95E246EB9449F5e4770916DCd6396A`   | 0.05% | ~$1.6M    |
| PancakeSwap V3 | `0x11d53ec50bc8f54b9357fbfe2a7de034fc00f8b3`   | 0.01% | ~$99K     |
| Camelot V3     | `0xe51635ae8136abac44906a8f230c2d235e9c195f`   | 0.081%| ~$50K     |
| SushiSwap V3   | `0xb3942c9ffa04efbc1fa746e146be7565c76e3dc1`   | 0.3%  | ~$14K     |

Best combo: PancakeSwap 0.01% <-> Uniswap 0.05% = 0.06% total

#### ARB/USDC across DEXes

| DEX            | Pool Address                                   | Fee   | TVL       |
|----------------|------------------------------------------------|-------|-----------|
| Uniswap V3     | `0xb0f6ca40411360c03d41c5ffc5f179b8403cdcf8`   | 0.05% | moderate  |
| PancakeSwap V3 | `0x93cce474015007b38da0ecea96671ee4dc3d40ad`   | 0.01% | moderate  |
| Camelot V3     | `0xfae2ae0a9f87fd35b5b0e24b47bac796a7eefea1`   | 0.025%| ~$32K     |

Best combo: PancakeSwap 0.01% <-> Camelot 0.025% = 0.035% total

#### WBTC/WETH across DEXes

| DEX            | Pool Address                                   | Fee   | TVL       |
|----------------|------------------------------------------------|-------|-----------|
| Uniswap V3     | `0x2f5e87c9312fa29aed5c179e456625d79015299c`   | 0.05% | ~$49M     |

(No WBTC/WETH pool found on SushiSwap V3 or PancakeSwap V3 on Arbitrum)

#### WBTC/USDC across DEXes

| DEX            | Pool Address                                   | Fee   | TVL       |
|----------------|------------------------------------------------|-------|-----------|
| Uniswap V3     | `0xac70bd92f89e6739b3a08db9b6081a923912f73d`   | 0.05% | moderate  |
| PancakeSwap V3 | `0x5a17cbf5f866bde11c28861a2742764fac0eba4b`   | 0.01% | moderate  |
| SushiSwap V3   | `0x699f628a8a1de0f28cf9181c1f8ed848ebb0bbdf`   | 0.05% | low       |

Best combo: PancakeSwap 0.01% <-> Uniswap 0.05% = 0.06% total

#### WBTC/USDT across DEXes

| DEX            | Pool Address                                   | Fee   | TVL       |
|----------------|------------------------------------------------|-------|-----------|
| Uniswap V3     | `0x53c6ca2597711ca7a73b6921faf4031eedf71339`   | 0.3%  | moderate  |
| PancakeSwap V3 | `0xeaf03f385c642b02b5c563640bba7c4fbf96c27d`   | 0.01% | moderate  |
| SushiSwap V3   | `0xafadba8a2a51654987cdc385bd302443c461679e`   | 0.05% | low       |

Best combo: PancakeSwap 0.01% <-> SushiSwap 0.05% = 0.06% total

---

### TIER 3 - LST/LRT Pairs (correlated assets, very low fees)

| Pair          | DEX            | Pool Address                                   | Fee    |
|---------------|----------------|------------------------------------------------|--------|
| wstETH/WETH   | Camelot V3     | `0xdeb89de4bb6ecf5bfed581eb049308b52d9b2da7`   | 0.005% |
| weETH/WETH    | Camelot V3     | `0x293dfd996d5cd72bed712b0eeab96dbe400c0416`   | 0.005% |
| weETH/WETH    | PancakeSwap V3 | `0x64dae6685725dbd0a0e63fe522c9134d0eaa7258`   | 0.01%  |
| cbBTC/WBTC    | Uniswap V3     | `0x9b42809aaae8d088ee01fe637e948784730f0386`   | 0.01%  |
| cbBTC/WBTC    | PancakeSwap V3 | `0x70db444986a997eb74e520f19d367e13d75ef97f`   | 0.01%  |
| BTC.b/WBTC    | Uniswap V3     | `0x014079e1eef0e734c40fd133e10c4874221fab70`   | 0.01%  |

weETH/WETH: Camelot 0.005% <-> PancakeSwap 0.01% = 0.015% total (very low!)
cbBTC/WBTC: Uniswap 0.01% <-> PancakeSwap 0.01% = 0.02% total

---

## SushiSwap V2 (0.3% fixed fee) - For reference

| Pair       | Pool Address                                   | TVL       |
|------------|------------------------------------------------|-----------|
| USDC/WETH  | `0x905dfcd5649217c42684f23958568e533c711aa3`   | ~$622K    |
| WETH/USDT  | `0xcb0e5bfa72bbb4d16ab5aa0c60601c438f04b4ad`   | moderate  |
| MAGIC/WETH | `0xb7e50106a5bd3cf21af210a755f9c8740890a8c9`   | moderate  |
| GMX/WETH   | `0x05c6f695ad50c16299bedca3fe9059b56550082f`   | moderate  |

SushiSwap V2 pools all have 0.3% fee - not ideal for arb legs unless spread is large.

---

## KEY FINDINGS & RECOMMENDATIONS

### Finding 1: SushiSwap V3 on Arbitrum has VERY LOW liquidity
SushiSwap V3 on Arbitrum does ~$315K daily volume (tiny). Most pools have <$15K TVL.
The WETH/USDC pair on SushiSwap V3 appears to NOT EXIST on Arbitrum (only on Base/Optimism).
SushiSwap is NOT a good arb leg for major pairs on Arbitrum.

### Finding 2: The real opportunity is Uniswap V3 <-> PancakeSwap V3 <-> Camelot V3
All three support 0.01% fee tiers on major pairs (WETH/USDC, USDT/USDC).
Combined fee = 0.02%, meaning you only need >0.02% spread to profit.

### Finding 3: Camelot V3 uses Algebra dynamic fees
Fees change based on volatility. Currently observed:
- WETH/USDC: 0.01% (can change)
- WETH/USDT: 0.015%
- ARB/WETH: 0.081%
- ARB/USDC: 0.025%
- wstETH/WETH: 0.005%
- weETH/WETH: 0.005%

Must read the fee from the pool contract at execution time, not hardcode it.

### Finding 4: PancakeSwap V3 is underrated for arb
PancakeSwap V3 has 0.01% pools for WETH/USDC, WBTC/USDC, WBTC/USDT, ARB/WETH, ARB/USDC.
These overlap with Uniswap V3 and Camelot V3 pairs perfectly.

### TOP 5 ARB PAIRS TO IMPLEMENT FIRST

1. **WETH/USDC**: Uniswap V3 (0.01%) <-> PancakeSwap V3 (0.01%) <-> Camelot V3 (0.01%)
   - 3 pools, 3 possible 2-leg combos, all at 0.02% total fee
   - Highest volume pair on Arbitrum

2. **USDT/USDC**: Uniswap V3 (0.01%) <-> SushiSwap V3 (0.01%) <-> PancakeSwap V3 (0.01%)
   - Stablecoin pair - very tight but very low fees
   - 0.02% total fee

3. **ARB/USDC**: PancakeSwap V3 (0.01%) <-> Camelot V3 (0.025%)
   - 0.035% total fee
   - ARB is volatile = more spread opportunities

4. **WBTC/USDC**: PancakeSwap V3 (0.01%) <-> Uniswap V3 (0.05%)
   - 0.06% total fee
   - BTC pairs move with large volume

5. **weETH/WETH**: Camelot V3 (0.005%) <-> PancakeSwap V3 (0.01%)
   - 0.015% total fee (lowest possible!)
   - LST/ETH pair - correlated but depegs happen

### FLASH LOAN SOURCES

For 2-leg arb with flash loans on Arbitrum:
- **Uniswap V3 flash()**: Borrow from the pool itself, fee = pool fee tier
- **Balancer V2 Vault**: `0xBA12222222228d8Ba445958a75a0704d566BF2C8` - 0% flash loan fee
- **Aave V3**: Flash loan available but has a small fee

**Recommended: Use Balancer V2 flash loans (0% fee) to fund the arb, then swap through the two DEX legs.**

---

## IMPLEMENTATION NOTES

### Reading Camelot V3 dynamic fees on-chain
Camelot V3 (Algebra) pools have a `globalState()` function that returns the current fee.
Call `pool.globalState()` and read the `fee` field (in hundredths of a bip).

### Pool interface differences
- Uniswap V3 / SushiSwap V3 / PancakeSwap V3: Standard IUniswapV3Pool interface
- Camelot V3 (Algebra): IAlgebraPool interface - different function signatures
  - `swap()` has different parameters
  - Uses `globalState()` instead of `slot0()`
  - Fee is dynamic and returned from `globalState()`

### USDC variants matter
- Native USDC (`0xaf88d065...`) and bridged USDC.e (`0xFF970A61...`) are DIFFERENT tokens
- A pool with native USDC on Uniswap V3 cannot be arbed against a USDC.e pool on Camelot
- Always verify the exact USDC variant in each pool
