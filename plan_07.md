# Plan 07 — Estrategias Rentables: Flash Loan Arbitrage en Arbitrum

**Fecha**: 2026-03-31
**Estado**: En ejecución
**Contrato actual**: `0xaF96FA723D8C9823669F1329EaA795FF0fF530Eb`
**Wallet**: `0xd69F9856A569B1655B43B0395b7c2923a217Cfe0`

---

## Diagnóstico: Por qué 0 trades exitosos

El bot detecta ~1,056 oportunidades pero no confirma ningún éxito. Causas raíz:

| # | Problema | Severidad | Evidencia |
|---|----------|-----------|-----------|
| 1 | Simulación rota: `sim_bought`/`sim_sold` = 0 en el 100% de oportunidades | **Crítica** | Los logs muestran profit estimado como `spread × amount` sin price impact real |
| 2 | Receipt monitoring ausente en fast path | **Crítica** | Envía tx con `eth_sendRawTransaction` pero nunca poll receipts → no sabe si revirtió |
| 3 | Optimal sizing impreciso para V3/mixed pools | **Alta** | Usa "liquidity-scaled" genérico en vez de tick-math exacto |
| 4 | Liquidation polling cada 30s (debería ser 250ms) | **Alta** | Pierde liquidaciones a bots más rápidos |
| 5 | Timeboost deshabilitado | **Media** | 200ms de prioridad compensarían la latencia de AWS |

---

## Fase 1 — Fixes Críticos (Simulación + Receipts)

### 1.1 Simulación exacta de amountOut

**Archivo**: `src/arb/mod.rs`

El cálculo actual de profit es `spread × optimal_in` sin simular el output real de cada pool. Esto produce falsos positivos masivos.

**Implementar**:

#### V2 (Constant Product) — ya parcialmente implementado
```
amountOut = (amountIn × 997 × reserveOut) / (reserveIn × 1000 + amountIn × 997)
```
Para CamelotV2 (fee denominator 100000, fees direccionales):
```
fee = if zeroForOne { token0FeePercent } else { token1FeePercent }
amountInWithFee = amountIn × (100000 - fee)
amountOut = (amountInWithFee × reserveOut) / (reserveIn × 100000 + amountInWithFee)
```

#### V3 (Concentrated Liquidity) — FALTA
Necesitamos simular el swap tick-by-tick:
```rust
fn sim_v3_swap(sqrt_price_x96: U256, liquidity: u128, tick: i32, fee: u32, amount_in: U256, zero_for_one: bool) -> U256 {
    // 1. Calcular sqrtPriceTarget del siguiente tick inicializado
    // 2. Computar amountIn que se consume hasta ese tick
    // 3. Si amount_in > consumido, cruzar tick y repetir
    // 4. Aplicar fee: amount_in_after_fee = amount_in * (1_000_000 - fee) / 1_000_000
    // 5. Retornar amountOut acumulado
}
```

**Simplificación viable**: Para la mayoría de arbs pequeños (<1 ETH), el swap no cruza ticks. Usar single-tick approximation:
```
amountOut ≈ (amountIn × (1_000_000 - fee) × liquidity) / (sqrtPrice × 1_000_000)
// ajustado por dirección del swap
```

#### Curve (StableSwap)
```
// Curve usa Newton's method on-chain, replicar es complejo
// Alternativa: eth_call a pool.get_dy(i, j, dx) via rpc-cache
// Latencia aceptable porque Curve arbs persisten >5s
```

**Validación**: Después de simular localmente, comparar con `eth_call` al contrato en 100 oportunidades históricas. Si discrepancia >1%, ajustar modelo.

### 1.2 Receipt monitoring en fast path

**Archivo**: `src/executor/mod.rs`

El fast path envía la tx raw pero el task de monitoring no se ejecuta correctamente.

```rust
// Actual (broken):
// Envía tx → log "FAST tx sent" → fin

// Fix:
// Envía tx → spawn task → poll eth_getTransactionReceipt cada 500ms por 30s
// Si status=1: log SUCCESS + notificar Telegram + actualizar stats
// Si status=0: log REVERT + decodear revert reason + cooldown del par
// Si timeout: log TIMEOUT + recheck nonce
```

**Decodear revert reason** es crucial para diagnosticar por qué fallan:
- `"INSUFFICIENT_OUTPUT"` → simulación imprecisa
- `"STF"` (SafeTransferFrom) → token con tax/fee-on-transfer
- `"LOK"` (Locked) → reentrancy, pool en uso
- Out of gas → aumentar gas limit para ese tipo de pool

---

## Fase 2 — Estrategias con Mayor Probabilidad de Profit

### 2.1 Cross-DEX Long Tail (PRIORIDAD MÁXIMA)

**Por qué funciona**: Los bots competitivos ignoran pools de baja liquidez. Los spreads persisten 1-5 segundos (vs <250ms en pools populares).

**Pools target por competencia baja**:

| DEX A | DEX B | Par ejemplo | Fee total |
|-------|-------|-------------|-----------|
| CamelotV2 | UniV3 | GRAIL/WETH | 0.3% + 0.3% = 0.6% |
| SushiV2 | CamelotV2 | MAGIC/WETH | 0.3% + 0.3% = 0.6% |
| Curve | UniV3 | USDC/USDT | ~0.04% + 0.01% = 0.05% |
| RamsesV2 | UniV3 | ARB/WETH | 0.3% + 0.05% = 0.35% |
| CamelotV2 | SushiV3 | PENDLE/WETH | 0.3% + 0.3% = 0.6% |

**Spread mínimo para profit** (incluyendo gas ~0.000012 ETH):
- Con flash loan Balancer (0%): spread > fee_pool_A + fee_pool_B + 0.000012/amountIn
- Para 0.1 ETH in: spread > 0.6% + 0.012% ≈ 0.62%
- Para 1 ETH in: spread > 0.6% + 0.0012% ≈ 0.60%

**Tokens long-tail prioritarios en Arbitrum**:
```
GRAIL    (0x3d9907F9a2194FFBf698693F7f6DA1c85c94D8Fc) — Camelot governance
MAGIC    (0x539bdE0d7Dbd336b79148AA742883198BBF60342) — Treasure ecosystem
RDNT     (0x3082CC23568eA640225c2467653dB90e9250AaA0) — Radiant Capital
PENDLE   (0x0c880f6761F1af8d9Aa9C466984b80DAb9a8c9e8) — Pendle Finance
JOE      (0x371c7ec6D8039ff7933a2AA28EB827Ffe1F52f07) — TraderJoe
JONES    (0x10393c20975cF177a3513071bC110f7962CD67da) — Jones DAO
DPX      (0x6C2C06790b3E3E3c38e12Ee22F8183b37a13EE55) — Dopex
GMX      (0xfc5A1A6EB076a2C7aD06eD22C90d7E710E35ad0a) — GMX
LINK     (0xf97f4df75117a78c1A5a0DBb814Af92458539FB4) — Chainlink
```

**Acción**:
- [ ] Agregar pools de estos tokens en indexer.rs (factories CamelotV2, SushiV2, RamsesV2)
- [ ] Reducir el filtro de spread mínimo de 0.1% a 0.05% para pares stable
- [ ] Implementar simulación V2 exacta con fees Camelot direccionales

### 2.2 Triangular Arbitrage 3-Hop

**Por qué funciona**: Exponencialmente más rutas que 2-hop → menos competencia por ruta.

**Rutas más probables en Arbitrum**:
```
WETH → ARB → USDC → WETH
  (CamelotV2) (UniV3)  (UniV3)

WETH → MAGIC → USDC → WETH
  (SushiV2)  (UniV3)  (UniV3)

WETH → GMX → USDC → WETH
  (UniV3)  (CamelotV2) (UniV3)

USDC → USDT → WETH → USDC
  (Curve)  (UniV3)  (UniV3)
```

**Fix requerido**: El amount fijo de 0.05 ETH es subóptimo. Implementar sizing dinámico:
```rust
// Para 3-hop, el optimal_in se calcula iterativamente:
// 1. Empezar con amount_test = 0.01 ETH
// 2. Simular las 3 legs → profit_1
// 3. Duplicar amount → profit_2
// 4. Si profit_2 > profit_1, seguir duplicando
// 5. Binary search entre último profitable y primer unprofitable
// Razón: en 3-hop el price impact se amplifica, el óptimo es menor que en 2-hop
```

**Gas estimado**: ~500k (3 swaps + flash loan callback) ≈ $0.002 en Arbitrum

**Acción**:
- [ ] Implementar sizing dinámico para 3-hop en `arb/mod.rs`
- [ ] Expandir `token_neighbors` graph con los 9 tokens long-tail
- [ ] Agregar `executeMultiHopFlashLoan()` call en executor para rutas 3-hop

### 2.3 Stablecoin Depeg Arbitrage

**Por qué funciona**: Curve stable pools usan invariante `x³y + xy³ = k` que resiste depegs. Después de un trade grande, el rebalanceo es lento porque los arbitrageurs necesitan capital (flash loans resuelven esto).

**Pools Curve en Arbitrum**:
```
2pool:  0x7f90122BF0700F9E7e1F688fe926940E8839F353  (USDC/USDT)
MIM:    0x30dF229cefa463e991e29D42DB0bae2e122B2AC7  (MIM/2pool)
frxETH: 0x1DeB3b1cA6afb63040c46E552C765aE1e0FC8A7d  (frxETH/WETH)
```

**Condición de entrada**:
```
spread = abs(curve_price - univ3_price)
if spread > 0.05% para USDC/USDT:
    direction = curve_price > 1.0 ? buy_in_univ3_sell_curve : buy_in_curve_sell_univ3
    amount = min(curve_depth * 0.1, 100_000 USDC)  // máx 10% del pool
    profit_est = spread * amount - flash_loan_fee(0%) - gas(0.000012)
```

**Acción**:
- [ ] Implementar `get_dy` call a Curve pools via rpc-cache para simulación exacta
- [ ] Monitorear Curve prices cada bloque (250ms) vs UniV3 USDC/USDT 0.01% pool
- [ ] Threshold: ejecutar cuando spread > 0.03% (≈$15 profit en $50k trade)

### 2.4 Aave V3 Liquidations

**Profit potencial**: 5-10% del collateral liquidado. Una liquidación de $10k WETH con 5% bonus = $500 profit.

**Estado actual**: Polling cada 30s, solo borrowers recientes.

**Mejoras**:

```
PRIORIDAD 1: Reducir polling a cada bloque (250ms)
- Subscribir a newHeads via WebSocket
- En cada bloque, recalcular HF para posiciones con HF < 1.5

PRIORIDAD 2: Indexar TODOS los borrowers activos
- Query getLogs(Borrow) desde bloque 0 hasta now
- Filtrar por posiciones con HF < 2.0 (threshold de monitoreo)
- ~500-2000 posiciones activas en Aave V3 Arbitrum

PRIORIDAD 3: Predicción de liquidación
- Monitorear Chainlink price feeds de los collaterals principales:
  ETH/USD:  0x639Fe6ab55C921f74e7fac1ee960C0B6293ba612
  BTC/USD:  0x6ce185860a4963106506C203335A2910413708e9
  ARB/USD:  0xb2A824043730FE05F3DA2efaFa1CBbe83fa548D6
  LINK/USD: 0x86E53CF1B870786351Da77A57575e79CB55812CB

- Calcular: ¿a qué precio el HF cruza 1.0?
  price_liquidation = (debt_USD × HF_threshold) / (collateral_amount × LTV)
  
- Pre-construir tx de liquidación cuando precio está a <2% del trigger
- Enviar en el mismo bloque que el precio cruza
```

**Flash loan flow** (ya implementado en contrato):
```
Balancer flash loan(debtToken, debtAmount)
  → approve Aave
  → liquidationCall(collateral, debt, user, debtAmount, false)
  → receive collateral con 5-10% bonus
  → swap collateral → debtToken en DEX más líquido
  → repay flash loan
  → profit queda en contrato
```

**Acción**:
- [ ] Refactorizar `liquidation/mod.rs` para polling por bloque
- [ ] Indexar borrowers históricos (backfill desde bloque de deploy de Aave V3 Arbitrum)
- [ ] Agregar monitoreo de Chainlink feeds para predicción
- [ ] Test en fork: simular liquidación con foundry

---

## Fase 3 — Optimizaciones de Infraestructura

### 3.1 Habilitar Timeboost

**Archivo**: `src/timeboost/mod.rs` (ya implementado, solo deshabilitado)

Timeboost da 200ms de prioridad en el sequencer. Esto compensa parcialmente la latencia de AWS.

**Estrategia de bidding**:
```
- Solo bidear cuando hay arb detectado con profit > bid_cost × 3
- Bid máximo: min(profit_estimado × 0.3, 0.001 ETH)
- Bidear para el round actual + 2 siguientes (3 min de cobertura)
- ROI esperado: si 1 de cada 10 bids produce un arb exitoso,
  profit promedio de $5 vs bid de $0.50 → 10x ROI
```

**Acción**:
- [ ] Descomentar inicialización de Timeboost en `main.rs`
- [ ] Condicionar bid a actividad de arb reciente (ya implementado como "signal-based")
- [ ] Monitorear ROI de bids vs arbs exitosos

### 3.2 Mejorar RPC Cache para simulación

El rpc-cache refresca V3 pools cada 250ms, pero para simulación necesitamos estado al momento exacto del arb.

**Fix**: Cuando se detecta un arb, hacer `eth_call` directo (bypass cache) para obtener `slot0` + `liquidity` frescos de ambos pools antes de simular.

```rust
// En arb/mod.rs, antes de simular:
let fresh_state_a = provider.call(pool_a.slot0()).await;  // bypass cache
let fresh_state_b = provider.call(pool_b.slot0()).await;
// Simular con estado fresco
// Solo si profit > threshold → ejecutar
```

### 3.3 Nodo local vs RPC públicos

**Situación actual**: El nodo Nitro local (docker) NO está en `RPC_UPSTREAMS`. El bot usa solo RPCs públicos vía cache.

**Recomendación**:
```
Si el nodo está sincronizado:
  - Agregar 127.0.0.1:8547 como PRIMER upstream en RPC_UPSTREAMS
  - Latencia: <1ms vs 20-50ms de RPCs públicos
  - Sin rate limits
  
Si el nodo NO está sincronizado o no corre:
  - Mantener solo RPCs públicos (estado actual)
  - Priorizar dRPC (más consistente)
```

**Acción**:
- [ ] Verificar si el nodo Nitro está sincronizado: `curl localhost:8547 -X POST -d '{"method":"eth_syncing",...}'`
- [ ] Si sí, agregar como primer upstream

---

## Fase 4 — Nuevas Estrategias Avanzadas

### 4.1 Sandwich Detection → Counter-Arb

No hacer sandwich (ético + legal risk). Pero sí detectar cuando OTROS bots hacen sandwich:

```
1. Detectar en sequencer feed: tx_frontrun → tx_victim → tx_backrun (mismo bot)
2. El backrun del sandwich deja el pool en un precio ligeramente diferente al original
3. Si hay otro pool con el precio pre-sandwich → arb entre pools
4. Ventana: ~250ms después del sandwich
```

### 4.2 Cross-L2 Arbitrage (futuro)

Arbitrum ↔ Optimism ↔ Base via bridges rápidos (Across, Stargate):
```
Precio WETH en Arbitrum UniV3: $3,500.00
Precio WETH en Base UniV3:     $3,502.50 (+0.07%)

Flash loan WETH en Arbitrum → bridge a Base → sell → bridge USDC back → repay
Problema: bridge latency 1-15 min, no es atómico
Solución: mantener inventario en ambas chains, rebalancear periódicamente
```

Esto requiere capital propio y es para una fase posterior.

### 4.3 JIT (Just-In-Time) Liquidity

Detectar swaps grandes en sequencer feed → proveer liquidez concentrada en UniV3 justo antes del swap → retirar después. Profit = fees del swap.

```
Requiere: Timeboost activo (para garantizar orden de txs)
Capital: Flash loan para la liquidez
Profit: 0.01-0.05% del volumen del swap
Risk: El swap podría no ejecutarse (revert)
```

---

## Orden de Ejecución

```
Semana 1: Fase 1 (Simulación + Receipts)
  ├─ 1.1 Simulación V2 exacta (incluyendo Camelot fees)
  ├─ 1.1 Simulación V3 single-tick
  ├─ 1.2 Receipt monitoring + revert decoder
  └─ Validar: comparar sim local vs eth_call en 100 oportunidades

Semana 2: Fase 2.1 + 2.2 (Long Tail + Triangular)
  ├─ Agregar pools long-tail (9 tokens × factories)
  ├─ Sizing dinámico para 3-hop
  ├─ Reducir spread mínimo para stables
  └─ Validar: primeros trades exitosos confirmados

Semana 3: Fase 2.3 + 2.4 (Stables + Liquidations)
  ├─ Curve get_dy integration
  ├─ Liquidation polling por bloque
  ├─ Chainlink feed monitoring
  └─ Test en fork con foundry

Semana 4: Fase 3 (Infra + Timeboost)
  ├─ Habilitar Timeboost
  ├─ Fresh state para simulación
  ├─ Nodo local como upstream
  └─ Análisis de ROI y ajuste de parámetros
```

---

## Métricas de Éxito

| Métrica | Actual | Target Semana 2 | Target Semana 4 |
|---------|--------|-----------------|-----------------|
| Trades exitosos/día | 0 | 5-10 | 20-50 |
| Profit/día | $0 | $1-5 | $10-50 |
| Tasa de revert | Desconocida | <50% | <20% |
| Oportunidades detectadas/día | ~100 | ~200 | ~500 |
| Liquidaciones capturadas/semana | 0 | 0-1 | 2-5 |

---

## Resumen de Estrategias por Viabilidad

| Estrategia | Viabilidad | Profit/trade | Frecuencia | Capital req. |
|------------|-----------|-------------|------------|-------------|
| Long-tail 2-hop | **Alta** | $0.50-5 | 10-30/día | 0 (flash loan) |
| Triangular 3-hop | **Media-Alta** | $1-10 | 5-15/día | 0 (flash loan) |
| Stable depeg | **Media** | $5-50 | 1-5/día | 0 (flash loan) |
| Aave liquidation | **Media** | $50-500 | 0-2/semana | 0 (flash loan) |
| Pool sniping | **Baja-Media** | $10-100 | 0-3/día | 0 (flash loan) |
| GMX divergence | **Baja** | $5-20 | Raro | 0 (flash loan) |
