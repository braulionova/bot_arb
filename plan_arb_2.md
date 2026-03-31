# Plan Arb v2: De 0 a Revenue On-Chain

## Estado Actual (2026-03-29)

### Pipeline verificado on-chain al 100%
- Flash loan Balancer V2 (0% fee) → swap leg 1 → swap leg 2 → profit check → repay
- Contrato: `0x8B45cDed167E40187007Fa6c7388e1924E7c9B0A`
- Todas las simulaciones eth_call llegan hasta "insufficient profit" (pipeline completo, profit insuficiente)
- Wallet: 0.0022 ETH (suficiente para ~100 arbs en Arbitrum)

### Por que no hay profit aun
1. **Solo ~300 pools indexados** de >9,000 disponibles — pocas combinaciones cross-DEX
2. **Camelot V3 roto** — factory muerta, interfaz Algebra no soportada
3. **Bug: address duplicada** en pool list (GMX/WETH comparte address con PCS WETH/USDC)
4. **Solo arbs de 2 hops** — competidores hacen 3+ hops (USDC.e→USDT→WETH→USDC.e)
5. **Spreads de 0.1-0.6%** se pierden por price impact en pools concentrados

### Competencia (observada on-chain)
| Bot | Txs | Ventaja |
|-----|-----|---------|
| `0xF7F2d68B` | 209,007 | Llama pools DIRECTO (sin router, -30k gas), Camelot V3 + UniV3 |
| `0x96B07f3e` | 60,384 | Arbs 3-hop: USDC.e→USDT→WETH→USDC.e, Balancer FL |
| `0xA5679C42` | 123 | Aave FL, 6+ pools por tx, Odos Router aggregation |

---

## Fase 1: Quick Wins — Pools que faltan (dia 1)

### 1.1 Fix bug address duplicada
```
Archivo: src/pools/indexer.rs linea 90
Actual:  0x7fCdC354... → DexType::UniswapV3, "GMX/WETH 0.3%" (INCORRECTO)
Fix:     0x1aEEdD3727A6431b8F070C0aFaA81Cc74f273882 → DexType::UniswapV3, "GMX/WETH 0.3%"
```
Ese address es PCS V3 WETH/USDC, no UniV3 GMX/WETH.

### 1.2 Agregar pool faltante de alto volumen
```
UniV3 WETH/USDC 0.01%: 0x6f38e884725a116C9C7fBF208e79FE8828a2595F
```
Pool de mayor volumen, no esta en nuestra lista.

### 1.3 Agregar Uniswap V2 Factory (8,546 pools)
```
Factory: 0xf1D7CC64Fb4452F05c498126312eBE29f30Fbcf9
Router:  0x4752ba5DBc23f44D87826276BF6Fd6b1C372aD24 (ya soportado)
```
Agregar a `v2_factories` en `index_background()`. Misma interfaz V2, sin cambios en executor.
**Impacto**: 10x mas combinaciones cross-DEX V2↔V3.

### 1.4 Agregar Ramses V2 (311 pools)
```
Factory: 0xAAA20D08e59F6561f242b08513D36266C5A29415
Router:  0xAAA87963EFeB6f7E0a2711F397663105Acb1805e
```
Fork Solidly. Interfaz V2 standard (`getReserves`, `swapExactTokensForTokens`).
Agregar a `v2_factories` + `DexType::RamsesV2` + router en executor.

### 1.5 Aprobar routers nuevos en contrato
```bash
cast send $ARB_CONTRACT "setApproval(address,address,uint256)" $TOKEN $RAMSES_ROUTER $MAX_UINT
```
Para cada token (WETH, USDC, USDT, ARB, WBTC) × cada router nuevo.

**Resultado Fase 1**: De ~300 a ~9,000+ pools. Multiplica oportunidades por 10-30x.

---

## Fase 2: Camelot V3 / Algebra (dia 2-3)

### 2.1 El problema
Camelot V3 usa Algebra, NO Uniswap V3:
- `globalState()` en vez de `slot0()`
- `poolByPair(tokenA, tokenB)` sin fee tiers (1 pool por par)
- Router: `0x1F721E2E82F6676FCE4eA07A5958cF098D339e18` (verificar si esta vivo)
- Factory: buscar la correcta (la actual `0x1a3c9B1d...` esta muerta)

### 2.2 Cambios necesarios

**Indexer** (`src/pools/indexer.rs`):
```rust
// Nueva interfaz para Algebra
sol! {
    interface IAlgebraFactory {
        function poolByPair(address tokenA, address tokenB) external view returns (address);
    }
    interface IAlgebraPool {
        function globalState() external view returns (
            uint160 price, int24 tick, uint16 fee, ...
        );
        function liquidity() external view returns (uint128);
        function token0() external view returns (address);
        function token1() external view returns (address);
    }
}
```

**Executor** (`src/executor/mod.rs`):
- Camelot V3 router usa interfaz `exactInputSingle` con parametros diferentes
- O mejor: llamar al pool directo (como hacen los bots competidores)

**Contrato** (`ArbExecutor.sol`):
```solidity
interface IAlgebraPool {
    function swap(
        address recipient,
        bool zeroForOne,
        int256 amountSpecified,
        uint160 sqrtPriceLimitX96,
        bytes calldata data
    ) external returns (int256 amount0, int256 amount1);
}
```
Agregar soporte para swap directo en pool Algebra (Camelot V3).

### 2.3 Por que es critico
Los bots competidores arbian **activamente** entre Camelot V3 y UniV3. Es una de las rutas mas rentables porque Camelot tiene fees dinamicos que crean spreads temporales mas grandes.

**Resultado Fase 2**: Acceso a la ruta de arb mas activa en Arbitrum.

---

## Fase 3: Arbs Multi-Hop (dia 3-4)

### 3.1 El problema
Nuestro contrato solo hace: borrow A → swap A→B → swap B→A → repay.
Los competidores hacen: borrow A → swap A→B → swap B→C → swap C→A → repay.

### 3.2 Ruta mas rentable observada on-chain
```
USDC.e → USDT (Camelot V3)
USDT   → WETH (UniV3 0.05%)
WETH   → USDC.e (SushiSwap V2)
Profit: ~$0.10 por ejecucion, 60k+ ejecuciones
```

### 3.3 Cambios necesarios

**Contrato** — nueva funcion multi-hop:
```solidity
struct SwapStep {
    address router;
    address tokenIn;
    address tokenOut;
    uint24 fee;
}

function executeArbMultiHop(
    address flashToken,
    uint256 flashAmount,
    SwapStep[] calldata steps,
    uint256 minProfit
) external onlyOwner { ... }
```

**Rust** — detector de triangular arb:
```rust
// Para cada swap detectado, buscar rutas de 3 pools:
// token_in → token_mid → token_out → token_in
// donde cada leg esta en un DEX diferente
fn detect_triangular_arb(swap, pools, token_pairs) -> Option<TriangularArb>
```

**Resultado Fase 3**: Acceso a la estrategia mas lucrativa de los competidores.

---

## Fase 4: Swaps Directos a Pool (dia 4-5)

### 4.1 Por que importa
El bot #1 (209k txs) NO usa routers. Llama `pool.swap()` directamente.
- Ahorra ~30k gas por swap (60k por arb de 2 hops)
- En Arbitrum gas es ~$0.01 por tx, asi que ahorra ~$0.006 por arb
- Con 1000 arbs/dia = $6/dia en gas savings
- Mas importante: permite ejecutar arbs mas marginales

### 4.2 Cambios necesarios

**Contrato**:
```solidity
function _swapV3Direct(
    address pool,
    bool zeroForOne,
    int256 amountSpecified
) internal returns (int256 amount0, int256 amount1) {
    return IUniswapV3Pool(pool).swap(
        address(this),
        zeroForOne,
        amountSpecified,
        zeroForOne ? MIN_SQRT_RATIO + 1 : MAX_SQRT_RATIO - 1,
        ""
    );
}
```

Requiere implementar el callback `uniswapV3SwapCallback` en el contrato.

**Executor** — pasar address del pool en vez del router:
```rust
fn build_direct_arb(&self, opp: &ArbOpportunity) -> Bytes {
    // Pass pool addresses directly instead of router addresses
    contract.executeArbDirect(
        opp.buy_pool.address,
        opp.sell_pool.address,
        ...
    )
}
```

**Resultado Fase 4**: -60k gas por arb, permite ejecutar arbs mas marginales.

---

## Fase 5: Aave V3 Flash Loans como Backup (dia 5)

### 5.1 El problema BAL#528
Algunos arbs fallan porque Balancer vault no tiene el token. Aave V3 tiene liquidez de WETH ($200M+), USDC ($150M+), USDT, WBTC en Arbitrum.

### 5.2 Cambios necesarios

**Contrato** — agregar interfaz Aave:
```solidity
interface IAavePool {
    function flashLoanSimple(
        address receiverAddress, address asset,
        uint256 amount, bytes calldata params, uint16 referralCode
    ) external;
}

function executeArbAaveFL(
    address tokenIn, uint256 amountIn,
    address buyRouter, address sellRouter,
    address tokenOut, uint24 buyFee, uint24 sellFee,
    uint256 minProfit
) external onlyOwner {
    aavePool.flashLoanSimple(address(this), tokenIn, amountIn, params, 0);
}
```

**Executor** — fallback logic:
```rust
// Si Balancer falla con BAL#528, intentar con Aave
if err.contains("BAL#528") {
    let aave_calldata = self.build_aave_flash_arb(opp);
    self.send_tx(aave_calldata, opp).await
}
```

**Resultado Fase 5**: Cobertura de flash loans para ~95% de tokens en Arbitrum.

---

## Fase 6: Optimizaciones de Latencia (semana 2)

### 6.1 Nodo local Arbitrum
```bash
docker compose up -d  # nitro-node ya configurado
# Sync: 24-48h, 560GB+ storage
```
Reduce latencia de 50-100ms (RPC publico) a <5ms.

### 6.2 Connection pooling
- HTTP keep-alive al nodo local
- WS persistente al sequencer feed
- Pre-signed nonces para envio instantaneo

### 6.3 Timeboost (cuando disponible)
- Subasta de 200ms express lane
- Pagar para tener prioridad temporal
- Ya implementado en `src/timeboost/mod.rs`, falta activar

---

## Cronograma y Prioridad

| Fase | Dias | Impacto en Revenue | Dificultad |
|------|------|-------------------|-----------|
| 1. Pools V2 + Ramses | 1 | **ALTO** — 10x mas oportunidades | Facil |
| 2. Camelot V3 Algebra | 2 | **ALTO** — ruta mas activa | Media |
| 3. Multi-hop arbs | 2 | **ALTO** — estrategia mas lucrativa | Media |
| 4. Swaps directos | 1 | MEDIO — -60k gas por arb | Media |
| 5. Aave FL backup | 1 | MEDIO — mas tokens cubiertos | Facil |
| 6. Nodo local + latencia | 3 | ALTO — llegar antes que competencia | Infra |

**Orden recomendado**: 1 → 2 → 3 → 6 → 4 → 5

---

## Metricas de Exito

| Metrica | Actual | Target Fase 1 | Target Fase 3 | Target Fase 6 |
|---------|--------|--------------|--------------|--------------|
| Pools indexados | 300 | 9,000+ | 9,000+ | 9,000+ |
| DEXes activos | 5 | 7 | 8 | 8 |
| Arbs detectados/hora | ~5 | ~50 | ~200 | ~200 |
| Sim passed/hora | 0 | ~5 | ~20 | ~50 |
| Txs sent/hora | 0 | ~2 | ~10 | ~30 |
| Success rate | 0% | >30% | >50% | >70% |
| Profit/dia | $0 | $5-20 | $50-200 | $200-1000 |

---

## Estimacion de Revenue

Basado en competidores observados on-chain:
- Bot #1 (209k txs en ~30 dias) = ~7,000 arbs/dia
- Si profit promedio = $0.05-0.10 por arb exitoso
- Revenue bruto = $350-700/dia

Para nuestro bot con RPCs publicos (sin nodo local):
- Capturariamos ~5-10% de las oportunidades (latencia)
- Revenue estimado Fase 1-3: **$20-70/dia**
- Revenue estimado Fase 6 (nodo local): **$100-300/dia**

---

## Riesgos

| Riesgo | Mitigacion |
|--------|-----------|
| Competidores mas rapidos | Nodo local + Timeboost |
| Smart contract exploit | Tests Foundry con fork, montos limitados |
| Gas spikes | Gas ceiling (ya implementado, 0.5 gwei) |
| Stale reserves → false arbs | Filtro spread >5%, eth_call pre-flight |
| Private key expuesta | Rotar key, usar KMS en produccion |
| Balancer vault sin liquidez | Fallback a Aave V3 |
