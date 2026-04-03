# Plan ML-1: Modelo Predictivo de Arbitraje Basado en Datos Reales

**Fecha**: 2026-03-31
**Estado**: Implementación

---

## Descubrimiento Clave

Análisis de txs exitosas de MEV bots en Arbitrum reveló:

| Lo que hacíamos | Lo que funciona |
|----------------|----------------|
| Flash loans Balancer (0% fee) | Capital propio o Aave lending |
| 2-hop simple (pool A → pool B) | Multi-path: split en 3-5 pools simultáneamente |
| Gas 0.5 gwei | Gas 0.01-0.02 gwei |
| Solo DEX↔DEX | DEX + stablecoin bridges + wrappers |
| Pools hardcodeados (30 pools) | Descubrimiento dinámico de ALL pools |
| Sim local con datos stale | Sim on-chain sequencer antes de enviar |
| Envío ciego de oportunidades | ML filtra y solo ejecuta alta confianza |

## Arquitectura del Sistema ML

```
┌─────────────────────────────────────────────────────────┐
│                    DATA PIPELINE                         │
│                                                          │
│  [Sequencer Feed] ──→ [Feature Extractor] ──→ [JSONL]   │
│       swaps            enriquece con:          datos     │
│       en tiempo        - pool state fresco     crudos    │
│       real             - liquidity depth                 │
│                        - gas price actual                │
│                        - hora/volumen                    │
│                        - sim sequencer result            │
└──────────────────────────┬──────────────────────────────┘
                           │
                           ▼
┌─────────────────────────────────────────────────────────┐
│                   ML MODEL (LightGBM)                    │
│                                                          │
│  Features:                    Target:                    │
│  - spread, net_spread         sequencer_sim_passed AND   │
│  - pool types (V2/V3/Curve)   would_profit_after_gas     │
│  - liquidity depth                                       │
│  - gas price actual           Output:                    │
│  - hora del día               confidence 0-1             │
│  - token pair type            threshold > 0.85 → execute │
│  - competition score                                     │
│  - route complexity                                      │
└──────────────────────────┬──────────────────────────────┘
                           │
                           ▼
┌─────────────────────────────────────────────────────────┐
│                 EXECUTION ENGINE                         │
│                                                          │
│  1. Bot detecta oportunidad                              │
│  2. Extrae features                                      │
│  3. Consulta ML scorer (HTTP localhost:8090)              │
│  4. Si confidence > threshold:                           │
│     a. Sim en sequencer (confirmación final)             │
│     b. Firma + envía en <50ms                            │
│  5. Log resultado para reentrenamiento                   │
│                                                          │
│  Protecciones:                                           │
│  - Cooldown 60s por ruta que falla                       │
│  - Gas limit 350k                                        │
│  - Gas price 0.01 gwei (como los bots exitosos)          │
│  - Max 1 tx por bloque (250ms)                           │
│  - Budget diario: max 0.001 ETH en gas                   │
└─────────────────────────────────────────────────────────┘

## Fase 1: Recolección de datos reales (2-4 horas)

### 1.1 Observador on-chain de arbs exitosos
Monitorea txs de MEV bots exitosos en Arbitrum para aprender:
- Qué pools usan
- Qué rutas toman
- A qué hora operan
- Qué spreads capturan
- Qué gas pagan

### 1.2 Observador pasivo de oportunidades
Bot en OBSERVE mode:
- Detecta spreads entre pools
- Simula en sequencer (eth_call)
- Etiqueta: pasó sim = 1, no pasó = 0
- Mide latencia de la sim

### 1.3 Pool discovery ampliado
No solo los 30 pools hardcodeados. Escanear:
- TODAS las pairs de CamelotV2 factory (500+)
- TODAS las pairs de SushiV2 factory (500+)
- TODAS las pairs de Ramses factory
- Stablecoin bridges/wrappers
- Nuevos pools creados (sniper)

## Fase 2: Feature Engineering + Training

### Features del modelo

**Precio:**
- spread_pct, net_spread_pct (después de fees)
- spread_persistence: ¿cuántos bloques dura el spread?
- price_impact_ratio: spread vs liquidity depth

**Pools:**
- buy_dex_type, sell_dex_type (V2, V3, Curve, Bridge)
- competition_score: V3↔V3 = 10 (max), V2↔V2 = 3, Mixed = 5
- pool_age: bloques desde creación
- pool_volume_24h (proxy: conteo de swaps recientes)

**Liquidity:**
- buy_pool_liquidity, sell_pool_liquidity
- min_liquidity (cuello de botella)
- amount_vs_liquidity_ratio

**Mercado:**
- hour_of_day, day_of_week
- recent_volatility (swaps/min en el par)
- gas_price_gwei actual
- blocks_since_last_arb_on_pair

**Token:**
- is_stablecoin_pair
- is_weth_pair
- token_decimal_diff (18 vs 6)
- token_has_transfer_fee

**Competencia (NUEVO):**
- n_bots_watching_pair (estimado por frecuencia de arb txs)
- time_since_last_successful_arb
- is_popular_pair (WETH/USDC = sí)

### Target variable
- `label`: 1 si sequencer sim pasa Y profit > gas_cost
- Validado con doble-sim (drpc + sequencer concordando)

### Modelo
- LightGBM con class_weight para imbalance
- Optimizar PRECISION > 90% (no enviar arbs falsos)
- Recall secundario (OK perder algunos arbs, NO OK perder gas)
- Threshold dinámico basado en balance disponible

## Fase 3: Deploy + Ejecución inteligente

### Gas optimizado
- Reducir de 0.5 gwei a 0.01 gwei (como bots exitosos)
- Reducir gas_limit de 350k a 250k para 2-hop
- Cada revert cuesta ~0.000003 ETH en vez de 0.000175 ETH

### Budget controller
- Max gas diario configurable
- Pausa automática si pierde N consecutivos
- Escala up si gana M consecutivos

### Continuous learning
- Cada tx (éxito o fallo) se agrega al training set
- Retrain cada 1000 nuevos samples
- Threshold se ajusta automáticamente por precision observada

## Fase 4: Expansión a multi-path

### Router inteligente (futuro)
Implementar lo que hacen los bots exitosos:
- Split un arb en 3-5 pools
- Incluir stablecoin bridges en la ruta
- Optimizar la distribución del amount entre pools
- Contrato nuevo: MultiPathExecutor

---

## Orden de ejecución inmediato

```
1. Implementar onchain_observer.py — monitorea arbs exitosos de otros bots
2. Implementar pool_discovery.py — escanea ALL factories
3. Implementar feature_extractor.py — genera features completos
4. Reducir gas a 0.01 gwei en executor
5. Recolectar datos 2-4 horas
6. Entrenar modelo
7. Deploy scorer + activar live mode
```
