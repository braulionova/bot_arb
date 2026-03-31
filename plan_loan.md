# Plan: Flash Loan Strategies Beyond DEX Arb

## Estado Actual
- Bot operativo con arb 2-hop, 3-hop, 4-hop cross-DEX
- Flash loans: Balancer V2 (0% fee) + Aave V3
- Sequencer feed backrunning activo
- GMX oracle tracking (parcial en src/gmx/mod.rs)
- Contrato ArbExecutor.sol desplegado en 0x674264aabc78C133e691c25e0892aB25eB170Dcf

## Capital Requerido: $0 (todo via flash loans)

---

## FASE 1: Curve/Balancer Stable Pool Rebalancing (1-2 días)
**ROI/esfuerzo: MÁXIMO — menor esfuerzo, infraestructura 90% lista**

### Concepto
Pools estables (USDC/USDT, wstETH/WETH, USDC/USDC.e) se desbalancean con trades
grandes. El precio se desvía del peg → arb contra Uniswap V3 donde la liquidez
concentrada mantiene el precio real.

### Profit estimado
- $0.50-$100 por evento
- 10-50 oportunidades/día
- wstETH/WETH más rentable (yield accruing crea drift natural)

### Implementación

#### 1.1 Agregar DexType::CurveStable y DexType::BalancerStable
- Archivo: src/decoder/mod.rs
- Agregar variantes al enum DexType
- Agregar decodificación de eventos Curve exchange() y Balancer Swap

#### 1.2 Indexar pools estables en Arbitrum
- Archivo: src/pools/indexer.rs
- Pools Curve target:
  - USDC/USDT (2pool)
  - USDC/USDC.e
  - wstETH/WETH
  - frxETH/WETH
- Pools Balancer target:
  - wstETH/WETH ComposableStable
  - USDC/USDT/DAI stable pool
- Leer precios via get_dy() (Curve) y onSwap() (Balancer)

#### 1.3 Matemática StableSwap
- Archivo: src/sim.rs (extender)
- Implementar Curve StableSwap invariant: A * n^n * sum(x_i) + D = A * D * n^n + D^(n+1) / (n^n * prod(x_i))
- Para Balancer: usar StableMath con amplification factor
- Calcular get_dy() off-chain para simular swaps sin RPC call

#### 1.4 Función _swapCurve() en contrato
- Archivo: contracts/ArbExecutor.sol
- Agregar interface ICurvePool { function exchange(int128 i, int128 j, uint256 dx, uint256 min_dy) }
- Agregar interface IBalancerVault.swap() para stable pools (ya tienes el vault)
- Integrar en _doMultiHop() como nuevo tipo de swap

#### 1.5 Detección de arb stable↔V3
- Archivo: src/arb/mod.rs
- Cuando sequencer feed muestra trade grande en Curve/Balancer stable pool:
  - Calcular nuevo precio post-trade en stable pool
  - Comparar con precio Uniswap V3 del mismo par
  - Si spread > gas cost → ejecutar flash loan arb

### Contratos Curve en Arbitrum
- 2pool (USDC/USDT): consultar registry
- Curve Registry: 0x445FE580eF8d70FF569aB36e80c647af338db351
- wstETH/WETH: consultar factory

### Tests
- foundry/test/ArbExecutor.t.sol: agregar test de swap Curve
- Test con fork de Arbitrum mainnet

---

## FASE 2: GMX V2 Pool Rebalancing Arb (3-5 días)
**Competencia media — nicho, requiere conocimiento GMX-específico**

### Concepto
GMX V2 pools tienen long/short imbalance. Cuando un pool está desbalanceado,
GMX ofrece "positive price impact" en swaps que rebalancean. Si GMX da mejor
precio que Uniswap → flash loan, swap en GMX, vender en Uniswap.

También: backrun keeper executions (executeOrder()) que mueven el pool state.

### Profit estimado
- $1-$50 por evento
- 50-200 keeper executions/día con price impact aprovechable

### Implementación

#### 2.1 Extender src/gmx/mod.rs
- Trackear pool states: open interest long/short, pool token balances
- Monitorear eventos OrderCreated, OrderExecuted del EventEmitter
- Calcular price impact usando fórmula GMX:
  price_impact = initial_diff_usd^2 - next_diff_usd^2 (por impact factor)

#### 2.2 Detector GMX↔DEX arb
- Archivo: src/gmx/arb.rs (nuevo)
- Comparar output de swap GMX vs output swap Uniswap para mismo par
- Considerar: GMX swap fee + price impact vs Uniswap fee + slippage
- Cuando GMX ofrece mejor precio → ejecutar

#### 2.3 Contrato: _swapGMX()
- Archivo: contracts/ArbExecutor.sol
- Interface IExchangeRouter.createOrder() — NOTA: GMX swaps son 2-step
- Alternativa: usar multicall atomico si GMX lo permite
- Investigar si se puede hacer swap atomico (single-tx) en GMX V2

### Contratos GMX V2 clave
- ExchangeRouter: 0x7C68C7866A64FA2160F78EEaE12217FFbf871fa8
- DataStore: 0xFD70de6b91282D8017aA4E741e9Ae325CAb992d8
- EventEmitter: 0xC8ee91A54287DB53897056e12D9819156D3822Fb
- Reader: 0xf60becbba223EEA9495Da3f606753867eC10d139

### Riesgo
- GMX swaps son 2-step (createOrder + executeOrder por keeper)
- Flash loan requiere repago en misma tx → investigar si swap atomico es posible
- Si no es atomico, necesita capital propio para esta estrategia
- Alternativa: solo backrun keeper executions con arb GMX→Uniswap post-execution

---

## FASE 3: Liquidation Backrunning (5-7 días)
**El gordo: $5k-$50k profit durante crashes del mercado**

### Concepto
Monitorear posiciones de lending protocols. Cuando price feed cae y health
factor < 1.0, flash loan la deuda, llamar liquidationCall(), recibir colateral
con 5-10% descuento, vender colateral en DEX, repagar flash loan.

### Profit estimado
- Normal: $5-$500 por liquidación, 5-20/día
- Durante crash (ETH -10%+): $1k-$50k por liquidación, 50-200/día
- La ventaja: sequencer feed predice qué posiciones caen ~200ms antes

### Implementación

#### 3.1 Módulo de monitoreo de posiciones
- Archivo: src/liquidation/mod.rs (nuevo)
- Indexar top 500 posiciones por health factor en Aave V3
- Usar getUserAccountData(address) para health factor
- Priority queue ordenada por health factor ascendente
- Refresh cada ~30s para posiciones con HF < 1.5
- Refresh cada ~5min para posiciones con HF < 2.0

#### 3.2 Predicción de liquidaciones via sequencer feed
- Cuando sequencer feed muestra swap grande que moverá precio de un asset:
  - Recalcular HF de posiciones que tienen ese asset como colateral
  - Si HF caerá bajo 1.0 → preparar tx de liquidación
  - Enviar tx inmediatamente después del swap trigger

#### 3.3 Indexar posiciones iniciales
- Leer eventos Borrow, Supply, Repay, Withdraw de Aave V3
- Construir mapa de posiciones: address → (colateral, deuda, HF)
- Actualizar en tiempo real con eventos del sequencer feed

#### 3.4 Contrato: liquidateAndSell()
- Archivo: contracts/ArbExecutor.sol
- Nueva función:
  ```
  function liquidateFlashLoan(
      address lendingPool,      // Aave V3 pool
      address borrower,         // position to liquidate
      address debtToken,        // token to flash loan
      uint256 debtAmount,       // amount to repay
      address collateralToken,  // collateral to receive
      address sellPool,         // DEX pool to sell collateral
      bool sellIsV3,
      uint256 minProfit
  ) external onlyOwner
  ```
- Flow: flash loan debtToken → approve lending pool → liquidationCall() →
  receive collateral at discount → sell on DEX → repay flash loan → profit

### Protocolos target
| Protocolo | Pool Address | Bonus Liquidación |
|-----------|-------------|-------------------|
| Aave V3 | 0x794a61358D6845594F94dc1DB02A252b5b4814aD | 5-10% |
| Radiant | Fork Aave, misma interface | 7.5% |
| Silo V2 | Factory: 0xf7dc975C96B434D436b9bF45E7a45c95F0521442 | Variable |

### Datos necesarios
- Aave V3 PoolDataProvider: para listar assets y posiciones
- Chainlink price feeds en Arbitrum: para predecir HF changes
- Sequencer feed: para detectar swaps que afectan precio del colateral

---

## Resumen de Prioridades

| Fase | Estrategia | Esfuerzo | Profit Mensual Est. | Capital |
|------|-----------|----------|---------------------|---------|
| 1 | Stable pool rebalancing | 1-2 días | $500-$3,000 | $0 |
| 2 | GMX V2 pool arb | 3-5 días | $300-$2,000 | $0* |
| 3 | Liquidation backrunning | 5-7 días | $100-$1,000 normal / $50k+ crash | $0 |

*GMX swap atomico por confirmar — si es 2-step, backrun-only (sin flash loan directo)

## SKIP (no implementar)
- Lending rate arb: requiere capital parqueado días/semanas
- CDP/Vault liquidation: Vesta muerto, no hay CDPs activos en Arbitrum
- Pendle PT/YT: nicho, AMM custom complejo, bajo reward vs esfuerzo

---

## Siguiente paso
Empezar FASE 1: agregar Curve/Balancer stable pools al bot existente.
Menor cambio de código, máximo incremento de oportunidades detectadas.
