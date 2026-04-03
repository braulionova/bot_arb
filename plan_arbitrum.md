# Plan Arbitrum MEV Bot

## Contexto

- Arbitrum **no tiene mempool**. El Sequencer centralizado procesa txs en orden FCFS.
- No hay sandwich attacks posibles — solo **backrun arbitrage** y **liquidaciones**.
- Block time: **250ms**. Ventana de reacción ultra-corta.
- TVL DeFi: ~$2.8B concentrado en Uniswap V3, GMX, Aave, Radiant, Camelot.
- Timeboost: subasta de **200ms express lane** para ventaja temporal.

---

## Arquitectura del Pipeline

```
Nodo Propio (local, mínima latencia)
        │
        ▼
Sequencer Feed (WSS port 9642)
        │
        ▼
Decoder (identificar swaps por selector + router)
        │
        ▼
Arb Detector (comparar precios cross-DEX)
        │
        ▼
Executor (firmar + enviar al Sequencer Endpoint directo)
```

---

## Estado del Proyecto

| Módulo | Archivo | Estado | Notas |
|--------|---------|--------|-------|
| Config | `src/config.rs` | ✅ Completo | Wallet, contract, thresholds, Timeboost |
| Feed | `src/feed/mod.rs` | ✅ Completo | WS al sequencer feed, reconnect automático |
| Decoder | `src/decoder/mod.rs` | ✅ Completo | Uniswap V2/V3, Camelot V2/V3, SushiSwap V3 |
| Pool Indexer | `src/pools/indexer.rs` | ✅ Completo | Factory queries V2+V3, top 12 tokens, all fee tiers |
| Pool Tracker | `src/pools/tracker.rs` | ✅ Completo | Real-time Sync/Swap events + periodic refresh |
| Pool State | `src/pools/mod.rs` | ✅ Completo | Shared state con RwLock, pair-indexed lookups |
| Arb Detector | `src/arb/mod.rs` | ✅ Completo | Cross-DEX V2/V3, optimal input, profit sim |
| Executor | `src/executor/mod.rs` | ✅ Completo | Wallet signing, nonce mgmt, flash loan + direct |
| Wallet | `src/wallet/mod.rs` | ✅ Completo | PrivateKeySigner, atomic nonce, sync from chain |
| GMX | `src/gmx/mod.rs` | ✅ Completo | Oracle price feed, GMX↔AMM arb detection |
| Timeboost | `src/timeboost/mod.rs` | ✅ Completo | Auction loop, express lane, bid strategy |
| Contract | `contracts/ArbExecutor.sol` | ✅ Completo | Flash loan (Aave V3), V2+V3 swaps, atomic arb |
| Main | `src/main.rs` | ✅ Compila | Full pipeline integrado |

**Total: ~3000+ líneas, compila limpio con cargo check.**

---

## Roadmap de Implementación

### Fase 1: Infraestructura Base
- [ ] **1.1 Nodo Arbitrum** — Levantar nodo Nitro en Contabo VPS
  - Docker: `offchainlabs/nitro-node:v3.9.7-75e084e`
  - Requiere endpoint L1 (Infura/Alchemy para empezar)
  - Storage: 1TB+ NVMe, ~560GB pruned inicial, crece ~200GB/mes
  - Ports: 8547 (HTTP), 8548 (WS)
- [ ] **1.2 Wallet Setup** — Crear wallet dedicada, fondear con ETH para gas
- [ ] **1.3 Verificar Feed** — Conectar al sequencer feed y logear txs raw para validar parsing

### Fase 2: Pool State & Pricing
- [ ] **2.1 Pool Indexer** — Consultar factory contracts para descubrir pools activos
  - Uniswap V3 Factory: `0x1F98431c8aD98523631AE4a59f267346ea31F984`
  - Camelot V3 Factory (Algebra): `0x1a3c9B1d2F0529e84FcE159b82A4E4C9Db632399`
  - SushiSwap V3 Factory: `0x1af415a1EbA07a4986a52B6f2e7dE7003D82231e`
  - Camelot V2 Factory: `0x6EcCab422D763aC031210895C81787E87B43A652`
- [ ] **2.2 Reserve Tracker** — Suscribirse a eventos Sync/Swap vía WS para mantener reserves actualizadas en memoria
- [ ] **2.3 Price Oracle** — Calcular precios spot por pool y detectar divergencias cross-DEX en tiempo real

### Fase 3: Smart Contract de Ejecución
- [ ] **3.1 Contrato ArbExecutor** — Solidity contract para flash swap atómico
  - Buy en DEX A → Sell en DEX B → Revert si profit < minProfit
  - Flash loans (Aave V3 en Arbitrum) para no necesitar capital upfront
  - `onlyOwner` para proteger la función de ejecución
- [ ] **3.2 Deploy & Test** — Deploy en Arbitrum testnet (Sepolia), luego mainnet
- [ ] **3.3 Integrar selector real** — Reemplazar el placeholder selector en executor

### Fase 4: GMX Integration
- [ ] **4.1 GMX Oracle Decoder** — GMX usa oracle pricing, no AMM
  - Arb surge cuando oracle price diverge del spot en AMMs
  - Monitorear `setPrice` events del GMX price feed
- [ ] **4.2 GMX↔AMM Arb Logic** — Cuando GMX price < Uniswap price (o viceversa), ejecutar arb
- [ ] **4.3 Radiant/Aave Liquidations** — Monitorear health factors, ejecutar liquidaciones rentables

### Fase 5: Optimización de Latencia
- [ ] **5.1 Wallet Signing** — Integrar `alloy-signer-local` con pre-signed nonce management
- [ ] **5.2 Sequencer Endpoint Directo** — Enviar txs a `https://arb1-sequencer.arbitrum.io/rpc` sin load balancer
- [ ] **5.3 Nonce Pipeline** — Pre-calcular nonces para enviar txs sin esperar confirmación
- [ ] **5.4 Connection Pooling** — Mantener conexiones HTTP/WS persistentes al nodo y sequencer

### Fase 6: Timeboost
- [ ] **6.1 Investigar Timeboost Auction** — Entender mecánica de la subasta por express lane
- [ ] **6.2 Bidding Strategy** — Calcular cuánto pujar basado en profit esperado
- [ ] **6.3 Express Lane Integration** — Enviar txs por el express lane cuando se gana la subasta

---

## DEXes Target en Arbitrum

| DEX | Tipo | Router Address | Prioridad |
|-----|------|---------------|-----------|
| Uniswap V3 | Concentrated Liquidity | `0x68b3465833fb72A70ecDF485E0e4C7bD8665Fc45` | Alta |
| Uniswap V2 | Constant Product | `0x4752ba5DBc23f44D87826276BF6Fd6b1C372aD24` | Media |
| Camelot V3 | Algebra (concentrated) | `0x1F721E2E82F6676FCE4eA07A5958cF098D339e18` | Alta |
| Camelot V2 | Constant Product | `0xc873fEcbd354f5A56E00E710B90EF4201db2448d` | Media |
| SushiSwap V3 | Concentrated Liquidity | `0x8A21F6768C1f8075791D08546Dadf6daA0bE820c` | Media |
| GMX V2 | Oracle-based | Varios contracts | Alta |

---

## Endpoints Clave

| Recurso | URL | Uso |
|---------|-----|-----|
| Sequencer Feed | `wss://arb1.arbitrum.io/feed` | Stream de txs ordenadas |
| Sequencer RPC | `https://arb1-sequencer.arbitrum.io/rpc` | Envío directo de txs |
| Nodo Local HTTP | `http://127.0.0.1:8547` | Estado local, queries |
| Nodo Local WS | `ws://127.0.0.1:8548` | Suscripciones eventos |
| L1 Ethereum | Infura/Alchemy | Requerido por nodo Nitro |

---

## Métricas de Éxito

- **Latencia feed→ejecución**: < 50ms (target)
- **Tasa de detección**: % de arbs detectados vs arbs ejecutados por competidores
- **Win rate**: % de txs enviadas que resultan en profit
- **Profit neto**: Después de gas + Timeboost bids
- **Uptime**: 99.9%+ del feed listener

---

## Riesgos

| Riesgo | Mitigación |
|--------|-----------|
| Competencia por latencia | Timeboost express lane, nodo local |
| Sequencer downtime | Fallback a RPC público, alertas |
| Smart contract exploit | Auditar contrato, limitar fondos en contrato |
| Gas spikes | Gas price ceiling en config, abort si gas > threshold |
| Pool state stale | Refresh reserves cada bloque (250ms) |
| Clave privada expuesta | .env fuera de git, considerar KMS |

---

## Dependencias del Proyecto

```toml
alloy = "1"              # Ethereum primitives & RPC
tokio = "1"              # Async runtime
tokio-tungstenite = "0.24" # WebSocket al sequencer feed
serde / serde_json       # Serialización
tracing                  # Logging
eyre                     # Error handling
dotenvy                  # Config desde .env
hex                      # Hex encoding
```

---

## Referencia: sequencer-client-rs

Librería Rust alternativa para el feed: `github.com/duoxehyon/sequencer-client-rs`
- Decodificación parcial de txs enfocada en MEV
- Considerar migrar el módulo `feed` a esta librería si el parsing manual da problemas
