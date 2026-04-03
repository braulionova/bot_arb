# Plan: Estrategias de Arb con Alta Latencia (400ms)

## Realidad Actual
- RTT al sequencer: 400ms (1.6 bloques de Arbitrum)
- Spreads en pools top: negativos después de fees (-0.04% a -0.06%)
- Bots co-locados: 10-20ms RTT capturan todo en pools top
- Balance: ~0.009 ETH ($18)
- Pools indexados: 225
- Contrato: ArbExecutor con flash loans Balancer (0%) + Aave V3
- Timeboost: activo en modo smart (bid on-demand)

## Cambio de Paradigma
NO competir por velocidad en pools populares.
Competir por COBERTURA (más pools, más rutas) y CORRECTITUD (AMMs exóticos que otros bots ignoran).

---

## FASE 1: New Pool Sniping (Mayor profit, ventana 5-60s)

### Concepto
Cuando se crea un pool nuevo en un DEX, el precio inicial a menudo no está alineado
con el mismo par en otros DEXes. Ventana de 5-60 segundos antes de que bots lo indexen.
400ms de latencia es irrelevante aquí.

### Profit estimado: $5-$200 por evento, 1-3/día

### Implementación

#### 1.1 Suscribirse a eventos de factories
Escuchar PairCreated/PoolCreated de todas las factories:

```
Uniswap V3:    0x1F98431c8aD98523631AE4a59f267346ea31F984
               event PoolCreated(address,address,uint24,int24,address)
               topic: 0x783cca1c0412dd0d695e784568c96da2e9c22ff989357a2e8b1d9b2b4e6b7118

Uniswap V2:    0xf1D7CC64Fb4452F05c498126312eBE29f30Fbcf9
Camelot V2:    0x6EcCab422D763aC031210895C81787E87B43A652
SushiSwap V2:  0xc35DADB65012eC5796536bD9864eD8773aBc74C4
Ramses V2:     0xAAA20D08e59F6561f242b08513D36266C5A29415
               event PairCreated(address,address,address,uint256)
               topic: 0x0d3648bd0f6ba80134a33ba9275ac585d9d315f0ad8355cddefde31afa28d0e9
```

#### 1.2 Lógica de sniping
- Archivo: src/scanner/sniper.rs (nuevo)
- Cuando se detecta nuevo pool:
  1. Extraer token0, token1 del evento
  2. Verificar si AMBOS tokens ya existen en pools conocidos
  3. Si sí → leer precio del nuevo pool (getReserves/slot0)
  4. Comparar con precio en pools existentes
  5. Si spread > fee total → flash loan arb
- SKIP tokens desconocidos (riesgo de rug pull)

#### 1.3 Seguridad anti-rug
- Solo tradear tokens que ya están en nuestro índice (WETH, USDC, USDT, ARB, WBTC, etc.)
- Verificar que el pool tiene liquidez mínima ($1000+)
- Max arb amount: 0.5 ETH (limitar exposición)

### Archivos a modificar
- src/scanner/sniper.rs (nuevo)
- src/main.rs (spawn sniper task)
- Reutiliza executor existente para flash loan

---

## FASE 2: Long-Tail Token Coverage (Más frecuente, steady income)

### Concepto
Tokens de media capitalización listados en 2+ DEXes con menos competencia de bots.
Los bots sofisticados ignoran estos porque el profit por evento es $1-20.
En Arbitrum el gas es $0.001 — estos arbs SON rentables.

### Profit estimado: $1-$20 por evento, 5-30/día

### Tokens target en Arbitrum
| Token | Pools conocidos | Por qué tiene spread |
|-------|----------------|---------------------|
| MAGIC | Camelot V2 + SushiSwap + UniV3 | Bajo volumen, reserves desfasadas |
| GRAIL | Camelot V2 + Camelot V3 | Solo en Camelot, spreads entre V2/V3 |
| RDNT | Camelot V2 + SushiSwap + UniV3 | Tres venues, spreads frecuentes |
| DPX | SushiSwap + UniV3 | Bajo volumen |
| PENDLE | Camelot V3 + UniV3 | Dos venues V3 con diferente liquidez |
| GMX | TraderJoe + UniV3 + Camelot | Tres venues |
| JOE | TraderJoe + SushiSwap | Baja competencia |
| STG | UniV3 + SushiSwap | Bridges token, volumen en spikes |
| LINK | UniV3 + Camelot + SushiSwap | Tres venues |
| UNI | UniV3 + SushiSwap | Dos venues |

### Implementación

#### 2.1 Expandir pool index
- Archivo: src/pools/indexer.rs
- Agregar addresses de estos tokens
- Buscar en todas las factories (V2 + V3) para cada par token/WETH y token/USDC
- Target: pasar de 225 a 500+ pools

#### 2.2 Bajar threshold de profit
- MIN_PROFIT_ETH = 0.0000001 (cualquier profit positivo)
- Dejar que el contrato haga el check final (revert si no hay profit)
- Gas de revert en Arbitrum: $0.001 — aceptable

#### 2.3 Optimizar amount para pools pequeñas
- Calcular optimal_amount basado en liquidez del pool más pequeño
- Para pools con $10K-$50K liquidez: arb con 0.01-0.05 ETH
- Para pools con $50K-$500K: arb con 0.05-0.5 ETH

---

## FASE 3: Camelot/Ramses V2 Spreads (Menos competencia)

### Concepto
Camelot V2 tiene fees DIRECCIONALES (diferente fee buy vs sell) que confunde
a la mayoría de bots. Ramses V2 tiene gauge incentives que causan shifts de liquidez.
Menos bots implementan estos correctamente → spreads persisten más.

### Profit estimado: $0.5-$10 por evento, 10-50/día

### Implementación

#### 3.1 Fix Camelot V2 directional fees
- Camelot V2 pairs: token0FeePercent != token1FeePercent
- Archivo: src/arb/mod.rs → check_pair_arb()
- Leer fees reales: stableSwap(), token0FeePercent(), token1FeePercent()
- Actualizar getAmountOut para usar fee correcta según dirección

#### 3.2 Ramses gauge rebalance detection
- Monitorear NotifyReward/GaugeDeposit events de Ramses
- Después de gauge rebalance: check cross-DEX spreads
- Archivo: src/scanner/ramses.rs (nuevo)

#### 3.3 V2↔V3 cross-type arbs
- Camelot V2 ↔ Uniswap V3 para mismo par
- Los V2 pools solo actualizan precio cuando alguien tradea
- Si nadie tradea por minutos, el precio drift → arb contra V3 spot

---

## FASE 4: Stablecoin Depeg Arbs (Baja frecuencia, alto profit)

### Concepto
Micro-depegs de 0.05-0.3% en USDC/USDT/DAI pools.
Flash loan el lado barato, vender en el lado caro.
Los depegs duran minutos → 400ms latencia no importa.

### Profit estimado: $0.5-$500 por evento, 10-50/semana

### Implementación
- Ya tenemos Curve pools indexados (Fase 1 del plan_loan.md)
- Agregar: Uniswap V3 USDC/USDT 0.01% pool como referencia de "true price"
- Monitorear desviación de Curve 2pool vs Uni V3
- Cuando spread > 0.05%: flash loan USDT → swap en Curve → swap en Uni → repay
- También: USDC.e/USDC pools (bridge lag crea depegs)

---

## FASE 5: Multi-Hop Complex Routes (Menos bots calculan)

### Concepto
Rutas de 3-4 hops por DEXes exóticos (Curve + Camelot + Uni).
Pocos bots tienen la cobertura para simular estas rutas.
Ya tenemos 4-hop detection — optimizar para incluir Curve y Camelot.

### Profit estimado: $1-$30 por evento, 5-15/día

### Implementación
- Ya tenemos detect_triangular_arb() con 3 y 4 hops
- Agregar Curve pools como intermediarios en el graph
- Agregar pools long-tail como puentes
- Ejemplo: WETH → USDC (Curve) → ARB (Camelot V2) → WETH (Uni V3)

---

## Orden de Implementación

| Prioridad | Fase | Esfuerzo | Impacto | Income estimado/día |
|-----------|------|---------|---------|---------------------|
| 1 | Pool Sniping | 2-3h | ALTO | $5-200 (variable) |
| 2 | Long-tail coverage | 1-2h | MEDIO | $5-50 |
| 3 | Camelot fees fix | 1h | MEDIO | $5-50 |
| 4 | Stablecoin depegs | 2h | BAJO (frecuencia) | $2-20 |
| 5 | Multi-hop complex | 1h | BAJO-MEDIO | $5-30 |

Total estimado: $20-300/día dependiendo de volatilidad del mercado.

---

## Principio Clave
> Con 400ms de latencia, no ganas por ser el más rápido.
> Ganas por estar donde otros no están: pools exóticos, tokens long-tail,
> rutas complejas, y AMMs con math no estándar.
