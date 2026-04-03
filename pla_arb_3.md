# Plan Arb v3: Del Primer Trade Exitoso al Revenue Diario

## Estado Real (2026-03-31)

### Lo que funciona
- Bot corriendo 24/7 (PID 16292 + rpc-cache PID 11835)
- 2,613 pools indexados en 11 DEXes
- Deteccion de spreads funcional: 15,844 oportunidades logueadas
- **1,056 oportunidades marcadas "profitable"** con profit total estimado de 0.041 ETH
- Pipeline de ejecucion completo: flash loan Balancer V2 → swap directo pool → repay
- Contrato deployado: `0xA21F2E80ef248DaAF32f92dF2dfE0Ee8F8d70278`
- Multi-hop (3-hop) detection + execution implementado
- Aave V3 flash loan como fallback implementado

### Lo que NO funciona — por que no hay profit aun

| # | Problema | Evidencia | Impacto |
|---|----------|-----------|---------|
| 1 | **sim_bought y sim_sold siempre = 0** | 1,056/1,056 profitable opps tienen sim_bought=0 | El profit estimado es TEORICO (spread * amount), NO validado con simulacion real |
| 2 | **Profit estimado vs real desalineado** | Se calcula `profit = spread * amount` sin simular price impact real | False positives: opp parece rentable pero on-chain no lo es |
| 3 | **eth_call sim usa RPC publico** | `sim_provider = drpc.org` con 50-100ms latencia | Simulacion lenta; precio ya cambio cuando se envia tx |
| 4 | **No hay evidencia de txs exitosas on-chain** | Logs muestran "FAST tx sent" pero 0 "ARB SUCCESS" confirmados | Las txs o revierten o nunca se confirman |
| 5 | **Balance wallet: ~0.0014 ETH** | Insuficiente para gas si hay reverts en cadena | Se queda sin gas rapido |
| 6 | **avg profit = 0.00004 ETH (~$0.15)** | Demasiado bajo para cubrir gas + competencia | Solo los top 5 opps (>0.001 ETH) son viables |

### Rutas mas rentables (data real de 48h)

| Ruta | Opps | Profit Total | Max |
|------|------|-------------|-----|
| PancakeSwapV3→UniswapV3 | 727 | 0.0149 ETH | 0.000311 |
| UniswapV3→PancakeSwapV3 | 309 | 0.0133 ETH | 0.001484 |
| CamelotV2→SushiSwapV2 | 13 | 0.0098 ETH | 0.001835 |
| SushiSwapV2→CamelotV2 | 3 | 0.0028 ETH | 0.001835 |
| UniswapV2→UniswapV3 | 3 | 0.0005 ETH | 0.000183 |

**Insight clave**: CamelotV2↔SushiSwapV2 tiene el MAYOR profit por opp ($0.75 avg vs $0.02 para V3↔V3). Menos competencia, mas profit.

---

## FASE 0: Diagnostico — Verificar que las txs llegan on-chain (30 min)

### 0.1 Verificar historial de txs del bot
```bash
# Instalar cast si no esta
curl -L https://foundry.paradigm.xyz | bash && foundryup

# Ver txs recientes del bot
cast etherscan-source 0xd69F9856A569B1655B43B0395b7c2923a217Cfe0 --chain arbitrum

# Balance actual
cast balance 0xd69F9856A569B1655B43B0395b7c2923a217Cfe0 --rpc-url https://arb1.arbitrum.io/rpc --ether

# Nonce actual (cuantas txs ha enviado)
cast nonce 0xd69F9856A569B1655B43B0395b7c2923a217Cfe0 --rpc-url https://arb1.arbitrum.io/rpc
```

### 0.2 Revisar Arbiscan manualmente
- Wallet: https://arbiscan.io/address/0xd69F9856A569B1655B43B0395b7c2923a217Cfe0
- Contrato: https://arbiscan.io/address/0xA21F2E80ef248DaAF32f92dF2dfE0Ee8F8d70278
- Buscar: txs enviadas, txs revertidas, razon del revert

### 0.3 Agregar logging de resultado de tx
El bot loguea "FAST tx sent" pero NO loguea el resultado del receipt en el fast path.
**Fix critico**: agregar monitoring de receipt en el fast path.

```rust
// executor/mod.rs ~linea 480
// Despues de "FAST tx sent", agregar:
// Spawn background task to monitor receipt
let wallet_clone = self.wallet.clone();
let sim_clone = self.sim_provider.clone();
tokio::spawn(async move {
    // ... monitor receipt, log SUCCESS/REVERT
});
```

**Resultado Fase 0**: Saber EXACTAMENTE cuantas txs se enviaron, cuantas se incluyeron, cuantas revertieron, y por que.

---

## FASE 1: Simulacion real antes de enviar (2-3h)

### 1.1 El problema central
El profit se calcula asi:
```
profit = spread_pct * amount_in  (linea 260-262 de arb/mod.rs)
```
Esto es una ESTIMACION lineal que ignora:
- **Price impact** (cuanto mueve el precio al tradear X amount)
- **Tick crossing** en V3 (el precio cambia discretamente entre ticks)
- **Stale reserves** (las reserves pueden tener 5-15s de delay)

### 1.2 Solucion: Simular con revm ANTES de enviar
Ya existe `LocalSim` en `src/sim.rs` pero **NO se usa en el path critico**.

```rust
// arb/mod.rs: ANTES de retornar Some(ArbOpportunity)
// Simular el flash loan completo localmente

// Opcion A: eth_call contra el contrato real (50-100ms, mas preciso)
let sim_result = provider.call(tx_request).await;

// Opcion B: revm local (5ms, menos preciso pero mas rapido)
let sim_result = local_sim.simulate_arb(buy_pool, sell_pool, amount_in);
```

### 1.3 Cambios concretos

**Archivo: `src/arb/mod.rs` linea 260-285**
```rust
// ANTES (actual):
let profit_raw = capped * net_spread;
let profit_eth = profit_raw / 1e18;

// DESPUES (con sim):
// 1. Simular buy: amount_in → amount_mid
let sim_buy = simulate_swap(buy_pool, &swap.token_in, &optimal_in)?;
// 2. Simular sell: amount_mid → amount_out
let sim_sell = simulate_swap(sell_pool, &swap.token_out, &sim_buy)?;
// 3. Profit real = amount_out - amount_in
let profit = if sim_sell > optimal_in { sim_sell - optimal_in } else { return None };
let profit_eth = u256_to_f64(profit) / 1e18;
```

**Nota**: `simulate_swap` ya existe (linea 396) pero el resultado no se usa para decidir si ejecutar.

### 1.4 Impacto esperado
- Elimina ~90% de false positives
- Solo envia txs con profit REAL simulado
- Reduce gas desperdiciado en reverts

**Resultado Fase 1**: Solo se envian txs que pasaron simulacion local. False positive rate baja de ~95% a ~20%.

---

## FASE 2: Fondear wallet + primer trade real (1h)

### 2.1 Fondear wallet
```bash
# Enviar 0.1 ETH desde exchange a:
# 0xd69F9856A569B1655B43B0395b7c2923a217Cfe0
# Red: Arbitrum One
```
Presupuesto minimo: 0.05 ETH ($100)
Presupuesto recomendado: 0.1 ETH ($200)

### 2.2 Test manual de un arb conocido
Antes de confiar en el bot, ejecutar UN arb manualmente:
```bash
# 1. Buscar un spread actual entre PCS V3 y UniV3 para WETH/USDC
cast call 0xd9e2a1a61B6E61b275cEc326465d417e52C1b95c \
  "slot0()(uint160,int24,uint16,uint16,uint16,uint8,bool)" \
  --rpc-url https://arb1.arbitrum.io/rpc

cast call 0x6f38e884725a116C9C7fBF208e79FE8828a2595F \
  "slot0()(uint160,int24,uint16,uint16,uint16,uint8,bool)" \
  --rpc-url https://arb1.arbitrum.io/rpc

# 2. Si hay spread, simular el flash loan
cast call $ARB_CONTRACT \
  "executeArbFlashLoan(address,uint256,address,address,address,bool,bool,uint256)" \
  $WETH $AMOUNT $BUY_POOL $SELL_POOL $USDC true true 0 \
  --from $BOT_ADDRESS --rpc-url https://arb1.arbitrum.io/rpc

# 3. Si sim pasa, enviar tx real
cast send $ARB_CONTRACT \
  "executeArbFlashLoan(address,uint256,address,address,address,bool,bool,uint256)" \
  $WETH $AMOUNT $BUY_POOL $SELL_POOL $USDC true true 0 \
  --private-key $PRIVATE_KEY --rpc-url https://arb1.arbitrum.io/rpc
```

### 2.3 Validar contrato con eth_call
```bash
# Test basico: flash loan 0.01 ETH, arb entre PCS V3 y UniV3
cast call 0xA21F2E80ef248DaAF32f92dF2dfE0Ee8F8d70278 \
  "executeArbFlashLoan(address,uint256,address,address,address,bool,bool,uint256)" \
  0x82aF49447D8a07e3bd95BD0d56f35241523fBab1 \
  10000000000000000 \
  0xd9e2a1a61B6E61b275cEc326465d417e52C1b95c \
  0x6f38e884725a116C9C7fBF208e79FE8828a2595F \
  0xaf88d065e77c8cC2239327C5EDb3A432268e5831 \
  true true 0 \
  --from 0xd69F9856A569B1655B43B0395b7c2923a217Cfe0 \
  --rpc-url https://arb1.arbitrum.io/rpc
```
Si revierte: analizar el error. Si pasa: el contrato funciona.

**Resultado Fase 2**: Confirmacion de que el contrato puede ejecutar un flash loan arb en mainnet.

---

## FASE 3: Filtros inteligentes — solo arbs viables (2h)

### 3.1 Filtrar por profit minimo REAL
```rust
// Cambiar MIN_PROFIT_ETH segun la ruta:
// V2↔V2: min 0.0001 ETH (gas $0.001, profit $0.40+)
// V3↔V3: min 0.0005 ETH (mas competencia, necesita mas margen)
// V2↔V3: min 0.0002 ETH (sweet spot)
```

### 3.2 Priorizar CamelotV2↔SushiSwapV2
Data muestra que estas rutas tienen 10x mas profit por opp que V3↔V3.
```rust
// En detect_arb: dar prioridad a pares con al menos 1 pool V2/Camelot
if is_low_competition(buy_pool) || is_low_competition(sell_pool) {
    // Bajar threshold a 0.00005 ETH
} else {
    // Mantener 0.0005 ETH para V3↔V3
}
```

### 3.3 Skip pools con liquidity < $5K
```rust
// Pools con poca liquidez causan:
// 1. High price impact → profit desaparece
// 2. Flash loan mas grande que la liquidez → revert
if pool_reserve_usd < 5000.0 { continue; }
```

### 3.4 Agregar cooldown por pool pair
```rust
// Si un arb en pool_a↔pool_b acaba de revertir, no intentar otra vez por 30s
// Evita spam de txs revertidas que gastan gas
let cooldown: HashMap<(Address, Address), Instant> = HashMap::new();
```

**Resultado Fase 3**: Tasa de exito sube de ~5% a ~40%. Gas desperdiciado baja 80%.

---

## FASE 4: Optimizar ejecucion — ganarle al reloj (3h)

### 4.1 Eliminar latencia de simulacion
Actualmente: detect → sim via drpc.org (100ms) → sign → send (200ms) = 300ms total.
Los competidores ejecutan en <50ms.

```rust
// Cambio 1: Simular contra rpc-cache local (5ms en vez de 100ms)
let sim_provider = ProviderBuilder::new()
    .connect_http("http://127.0.0.1:8545".parse().unwrap());

// Cambio 2: Para profit > $2, SKIP sim completamente
// Gas de revert en Arbitrum = $0.01. Vale la pena.
if opp.expected_profit_eth > 0.001 {
    // Send direct, no sim
}
```

### 4.2 Pre-computar calldata
```rust
// En vez de construir calldata DESPUES de detectar arb,
// mantener templates pre-computados para pools frecuentes
struct PrecomputedArb {
    buy_pool: Address,
    sell_pool: Address,
    calldata_template: Bytes,  // solo falta amount_in
}
```

### 4.3 Enviar a sequencer directo (ya implementado, verificar)
El bot ya envia a `https://arb1.arbitrum.io/rpc`.
Verificar que no haya redirect o rate limiting:
```bash
curl -X POST https://arb1.arbitrum.io/rpc \
  -H "Content-Type: application/json" \
  -d '{"jsonrpc":"2.0","method":"eth_chainId","params":[],"id":1}'
```

**Resultado Fase 4**: Latencia total detect→tx_sent baja de 300ms a <100ms.

---

## FASE 5: Monitoreo y metricas en tiempo real (2h)

### 5.1 Dashboard en Telegram mejorado
```
📊 Reporte cada 5 min:
- Swaps detectados: 850
- Arbs encontrados: 18
- Txs enviadas: 5
- Txs exitosas: 2
- Txs revertidas: 3
- Profit acumulado: 0.003 ETH
- Balance wallet: 0.097 ETH
- Top ruta: PCS→Uni (3 arbs, 0.002 ETH)
```

### 5.2 Alertas criticas
- Balance < 0.01 ETH → alerta
- 5 reverts consecutivos → pausar ejecucion
- Nonce desync detectado → resync automatico
- RPC cache caido → alerta

### 5.3 Log de txs on-chain
```rust
// Nuevo archivo: logs/txs_onchain.jsonl
{
    "timestamp": "...",
    "tx_hash": "0x...",
    "arb_type": "2-hop",
    "buy_pool": "0x...",
    "sell_pool": "0x...",
    "amount_in": "0.05",
    "expected_profit": "0.0003",
    "actual_profit": "0.0002",  // from receipt
    "gas_used": 250000,
    "gas_cost_eth": 0.000005,
    "status": "success|reverted",
    "revert_reason": "insufficient profit"
}
```

**Resultado Fase 5**: Visibilidad total del pipeline, detectar problemas en minutos.

---

## FASE 6: Expandir cobertura de pools (3h)

### 6.1 Indexar TODOS los pools de Uniswap V2 factory
```
Factory: 0xf1D7CC64Fb4452F05c498126312eBE29f30Fbcf9
Pools estimados: 8,546
```
Filtrar por: liquidez > $5K, al menos 1 token conocido (WETH, USDC, etc.)

### 6.2 Agregar Camelot V3 (Algebra)
La ruta mas rentable en Arbitrum. Los bots competidores la usan activamente.
```
Factory: buscar la correcta (la actual esta muerta)
Interface: globalState() en vez de slot0()
```

### 6.3 Agregar mas tokens long-tail
| Token | Pools | Por que |
|-------|-------|---------|
| MAGIC | 3+ DEXes | Bajo volumen, spreads persisten |
| GRAIL | Camelot only | Cross V2/V3 spreads |
| RDNT | 3+ DEXes | Frecuentes desyncs |
| PENDLE | 2 DEXes V3 | Diferente liquidez |
| wstETH | Curve + UniV3 | Staking APR crea spreads |

### 6.4 Pool sniping activo
El sniper ya esta implementado (`src/scanner/sniper.rs`).
Verificar que esta activo y que ejecuta arbs cuando detecta nuevos pools.

**Resultado Fase 6**: De 2,613 pools a 10,000+. 5-10x mas combinaciones.

---

## Cronograma de Ejecucion

| Dia | Fase | Objetivo | Metrica de exito |
|-----|------|----------|-----------------|
| 1 (hoy) | 0 + 2 | Diagnostico + fondear wallet | Saber cuantas txs se enviaron/revertieron |
| 1 (hoy) | 1 | Sim real antes de enviar | sim_bought > 0 en oportunidades |
| 2 | 2.2 + 2.3 | Test manual de arb | 1 eth_call exitoso en contrato |
| 2-3 | 3 | Filtros inteligentes | Tasa de revert < 50% |
| 3-4 | 4 | Optimizar latencia | detect→send < 100ms |
| 4-5 | 5 | Monitoreo | Dashboard Telegram funcionando |
| 5-7 | 6 | Expandir pools | 10K+ pools, Camelot V3 activo |

---

## Metricas de Exito

| Metrica | Actual | Target Dia 3 | Target Dia 7 |
|---------|--------|-------------|-------------|
| Txs enviadas/dia | ~20 (sin confirmar) | 10 (verificadas) | 50+ |
| Txs exitosas/dia | 0 confirmadas | 1+ | 10+ |
| Tasa de exito | 0% | 30%+ | 50%+ |
| Profit/dia | $0 | $1+ | $10+ |
| sim_bought > 0 | 0% de opps | 100% de opps | 100% |
| Latencia detect→send | ~300ms | <150ms | <100ms |
| Pools activos | 2,613 | 2,613 | 10,000+ |

---

## Resumen: Las 3 cosas que MAS importan AHORA

1. **DIAGNOSTICO**: Verificar en Arbiscan si alguna tx del bot llego on-chain. Si NO → hay un bug en send_tx. Si SI pero revierten → el contrato o los params estan mal.

2. **SIMULACION REAL**: El profit calculado como `spread * amount` sin simular price impact es fantastico en papel pero mentira en la practica. Hay que simular con `simulate_swap()` ANTES de decidir ejecutar.

3. **FONDEAR WALLET**: Con 0.0014 ETH no se puede hacer nada. Necesita 0.05-0.1 ETH minimo.

Todo lo demas (mas pools, Camelot V3, latencia) es optimizacion. Sin estos 3, el bot nunca va a tener su primer trade exitoso.
