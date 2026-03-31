# Plan On-Chain: Flash Loan Arb Exitoso en Arbitrum

## Objetivo

Llevar el bot de estado actual (compila, detecta spreads, loguea oportunidades) a **ejecucion real on-chain con flash loans de Balancer V2 que generen profit neto**.

---

## Estado Actual

| Componente | Estado | Ubicacion |
|------------|--------|-----------|
| Contrato ArbExecutor.sol | Escrito, no deployado | `contracts/ArbExecutor.sol` |
| Feed del sequencer | Funcional | `src/feed/mod.rs` |
| Decodificacion de swaps | 11 DEXes cubiertos | `src/decoder/mod.rs` |
| Pool indexer + tracker | Funcional | `src/pools/` |
| Deteccion de arb | Funcional (spread-based) | `src/arb/mod.rs` |
| Executor (calldata + envio) | Escrito, no probado on-chain | `src/executor/mod.rs` |
| Deploy script | **Desincronizado con contrato** | `foundry/script/Deploy.s.sol` |
| Tests Foundry | **Vacio** | `foundry/test/` |

---

## Bugs y Desincronizaciones Criticas

### 1. Deploy.s.sol no coincide con ArbExecutor.sol

El script llama funciones que **no existen** en el contrato actual:
- `arb.aavePool()` — el contrato usa Balancer V2, no Aave
- `arb.setApproval()` — no existe; el contrato hace `approve()` inline en `_doArb()`

**Fix**: Reescribir `Deploy.s.sol` para que coincida con la interfaz real del contrato.

### 2. Slippage = 0 en el contrato

```solidity
// ArbExecutor.sol lineas 180, 209
amountOutMinimum: 0    // Acepta CUALQUIER resultado
```

El contrato no protege contra slippage. Si el precio se mueve entre simulacion y ejecucion, puede comprar/vender a precio peor y aun asi no revertir (el `minProfit` check solo valida el neto final, no cada swap individual).

**Riesgo**: En Arbitrum no hay sandwich clasico, pero si competidores que ejecutan el mismo arb antes → precio se mueve → tu tx recibe menos tokens → profit desaparece → tx revierte por `minProfit` (gasta gas sin ganar nada).

**Fix**: Calcular `amountOutMinimum` en el Rust executor y pasarlo como parametro adicional, o aceptar el riesgo dado que el revert por `minProfit` limita la perdida a gas (~$0.01).

### 3. Fee conversion inconsistente entre arb y executor

```rust
// src/arb/mod.rs linea 190-191
let buy_fee = if is_v3(buy_pool.dex) { buy_pool.fee_bps * 10 } else { 0 };
// src/executor/mod.rs linea 153-154
let buy_fee: u32 = if is_v3(opp.buy_pool.dex) { opp.buy_pool.fee_bps * 100 } else { 0 };
```

`arb/mod.rs` multiplica por 10, `executor/mod.rs` multiplica por 100. Solo uno puede ser correcto.
- Uniswap V3 fee tiers: 100, 500, 3000, 10000 (en unidades de 1/1000000)
- Si `fee_bps = 30` (0.3%), correcto es `30 * 100 = 3000`
- El valor en `arb/mod.rs` (`* 10 = 300`) es **incorrecto** pero solo se usa para logging/metadata
- El valor en `executor/mod.rs` (`* 100 = 3000`) es **correcto** para el calldata

**Fix**: Corregir `arb/mod.rs` para consistencia, aunque el executor ya tiene el valor correcto.

---

## Fases de Implementacion

### Fase 0: Seguridad (antes de todo)

- [ ] **0.1 Rotar private key** — La key actual esta en plaintext en `.env` y posiblemente en git history
  ```bash
  cast wallet new
  # Actualizar PRIVATE_KEY y BOT_ADDRESS en .env
  # Fondear nueva wallet con 0.05+ ETH para gas
  ```
- [ ] **0.2 Verificar .gitignore** — `.env` y `node.env` no deben commitearse
- [ ] **0.3 Limpiar git history** si la key fue commiteada (o asumir que esta quemada)

---

### Fase 1: Arreglar contrato + deploy script (1 dia)

#### 1.1 Agregar `setApproval` al contrato (o quitarlo del deploy)

**Opcion A** — Agregar funcion al contrato (recomendado para gas savings):
```solidity
function setApproval(address token, address spender, uint256 amount) external onlyOwner {
    IERC20(token).approve(spender, amount);
}
```
Pre-aprobar tokens para routers en deploy ahorra ~46k gas por swap (no hace approve en cada `_doArb`).

**Opcion B** — Quitar del deploy script y dejar approve inline (mas simple, funciona ya).

#### 1.2 Reescribir Deploy.s.sol

```solidity
contract DeployScript is Script {
    function run() external {
        uint256 pk = vm.envUint("PRIVATE_KEY");
        vm.startBroadcast(pk);

        ArbExecutor arb = new ArbExecutor();
        console.log("ArbExecutor:", address(arb));
        console.log("Owner:", arb.owner());
        console.log("Balancer Vault:", address(arb.vault()));

        // Si se agrego setApproval:
        // Pre-approve tokens x routers
        // ...

        vm.stopBroadcast();
    }
}
```

#### 1.3 Considerar slippage en swaps individuales (opcional)

Cambiar firma de `_doArb` para aceptar `minAmountOut` por cada leg:
```solidity
function _doArb(
    address tokenIn, uint256 amountIn,
    address buyRouter, address sellRouter,
    address tokenOut,
    uint24 buyFee, uint24 sellFee,
    uint256 minBuyOut,   // nuevo
    uint256 minSellOut   // nuevo
) internal { ... }
```
**Decision**: Dado que `minProfit` ya protege el resultado neto y gas en Arbitrum es ~$0.01, esto es **nice-to-have**, no bloqueante.

---

### Fase 2: Tests con fork de mainnet (1-2 dias)

#### 2.1 Test de flash loan completo

```bash
# foundry/test/ArbExecutor.t.sol
forge test --fork-url https://arbitrum.drpc.org -vvv
```

Tests necesarios:

| Test | Que valida |
|------|-----------|
| `testFlashLoan_V3_V3_WETH_USDC` | Flash loan Balancer → swap en UniV3 → swap en SushiV3 → repay |
| `testFlashLoan_V2_V2_WETH_USDC` | Flash loan → swap en CamelotV2 → swap en SushiV2 → repay |
| `testFlashLoan_V3_V2_mixed` | Flash loan → compra en V3 → venta en V2 → repay |
| `testDirectArb_withBalance` | Arb sin flash loan usando balance del contrato |
| `testRevert_insufficientProfit` | Revierte si profit < minProfit |
| `testRevert_notOwner` | Solo owner puede ejecutar |
| `testRevert_notVault` | Solo Balancer vault puede llamar callback |
| `testWithdraw` | Owner puede retirar tokens y ETH |

#### 2.2 Verificar que Balancer V2 Vault funciona en Arbitrum

- Vault address: `0xBA12222222228d8Ba445958a75a0704d566BF2C8`
- Confirmar que el vault tiene liquidez de WETH, USDC, USDT, ARB
- Confirmar fee = 0 (Balancer V2 flash loans son gratis)
- El callback debe ser `receiveFlashLoan` (no `executeOperation` como Aave)

#### 2.3 Verificar routers en fork

- UniV3 SwapRouter (`0xE592427A0AEce92De3Edee1F18E0157C05861564`) — funciona con pools de UniV3, Sushi V3
- CamelotV2 router (`0xc873fEcbd354f5A56E00E710B90EF4201db2448d`) — tiene `swapExactTokensForTokens`?
- SushiSwapV2 router (`0x1b02dA8Cb0d097eB8D57A175b88c7D8b47997506`) — confirmar interfaz

**ATENCION**: CamelotV2 usa una interfaz diferente a Uniswap V2 standard:
```solidity
// Camelot V2 Router tiene un parametro extra: `referrer`
function swapExactTokensForTokensSupportingFeeOnTransferTokens(
    uint amountIn, uint amountOutMin,
    address[] calldata path, address to,
    address referrer,     // <-- EXTRA
    uint deadline
) external;
```
Si `ArbExecutor.sol` llama `swapExactTokensForTokens` en el router de Camelot, **puede fallar**. Verificar en fork test.

---

### Fase 3: Deploy a mainnet (30 min)

- [ ] **3.1 Compilar**
  ```bash
  cd foundry && forge build
  ```

- [ ] **3.2 Deploy**
  ```bash
  forge script script/Deploy.s.sol \
    --rpc-url https://arbitrum.drpc.org \
    --broadcast --slow -vvv
  ```

- [ ] **3.3 Verificar en Arbiscan**
  ```bash
  forge verify-contract <ADDRESS> ArbExecutor \
    --chain arbitrum \
    --etherscan-api-key $ARBISCAN_API_KEY
  ```

- [ ] **3.4 Actualizar .env**
  ```
  ARB_CONTRACT=0x<nueva_address>
  ```

- [ ] **3.5 Pre-aprobar tokens** (si se agrego `setApproval`)
  ```bash
  cast send $ARB_CONTRACT "setApproval(address,address,uint256)" \
    $WETH $UNI_V3_ROUTER $(cast max-uint) \
    --rpc-url https://arbitrum.drpc.org \
    --private-key $PRIVATE_KEY
  # Repetir para cada token x router
  ```

---

### Fase 4: Dry run con logs (3-5 dias)

- [ ] **4.1 Correr bot con contrato deployado pero MIN_PROFIT_ETH alto**
  ```bash
  MIN_PROFIT_ETH=1.0 cargo run  # Solo loguea, no ejecuta nada real
  ```

- [ ] **4.2 Validar oportunidades detectadas**
  - Revisar `/tmp/arb_opportunities.jsonl`
  - Para cada oportunidad logueada:
    - Verificar que los precios de los pools son correctos (query on-chain manual)
    - Verificar que el spread reportado es real (no stale reserves)
    - Simular el calldata con `cast call` para ver si pasaria

- [ ] **4.3 Analizar frecuencia y tamano de oportunidades**
  ```bash
  # Cuantas oportunidades por hora?
  cat /tmp/arb_opportunities.jsonl | wc -l
  # Profit promedio?
  cat /tmp/arb_opportunities.jsonl | jq -r '.profit_eth' | awk '{s+=$1}END{print s/NR}'
  ```

- [ ] **4.4 Ajustar thresholds basado en data real**
  - Si hay muchos false positives → subir MIN_PROFIT_ETH
  - Si hay pocas oportunidades → agregar mas pools al indexer
  - Si los spreads son reales pero chicos → calcular si flash loan fee + gas los hace no rentables

---

### Fase 5: Ejecucion real con capital minimo (1-2 dias)

- [ ] **5.1 Bajar MIN_PROFIT_ETH a nivel real**
  ```bash
  MIN_PROFIT_ETH=0.001  # ~$2-3 minimo profit
  ```

- [ ] **5.2 Monitorear primeras 10-20 ejecuciones**
  - Cada tx enviada: verificar en Arbiscan
  - Clasificar: exito (profit), revert (por que?), nunca incluida (gas muy bajo?)
  - Ajustar gas settings si necesario:
    ```rust
    // executor/mod.rs lineas 225-226 — actualmente:
    let max_fee = 100_000_000u128;      // 0.1 gwei
    let priority_fee = 1_000_000u128;   // 0.001 gwei
    // Arbitrum base fee tipico: 0.01 gwei — estos valores deberian funcionar
    // Si txs no se incluyen, subir priority_fee
    ```

- [ ] **5.3 Verificar profit real**
  ```bash
  # Balance antes vs despues
  cast balance $BOT_ADDRESS --rpc-url https://arbitrum.drpc.org
  # Tokens en el contrato
  cast call $ARB_CONTRACT "balanceOf(address)(uint256)" $WETH --rpc-url ...
  ```

- [ ] **5.4 Fix nonce sync post-receipt**
  Agregar resync de nonce cuando un tx es confirmado:
  ```rust
  // executor/mod.rs — dentro del spawn de receipt monitoring
  if receipt.status() {
      // Re-sync nonce from chain
      info!(?tx_hash, "ARB SUCCESS");
  } else {
      error!(?tx_hash, "ARB REVERTED");
      // wallet.sync_nonce() tambien aqui
  }
  ```

---

### Fase 6: Optimizacion post-lanzamiento (ongoing)

- [ ] **6.1 Nodo local Arbitrum** — Reduce latencia de 50-100ms (RPC publico) a <5ms
  ```bash
  docker compose up -d  # usa docker-compose.yml existente
  # Esperar 24-48h para sync completo
  ```

- [ ] **6.2 Rate limiting en RPC** — Agregar semaforo para evitar throttling
  ```rust
  let rpc_semaphore = Arc::new(Semaphore::new(20)); // max 20 concurrent calls
  ```

- [ ] **6.3 Gas price dinamico** — Leer `eth_gasPrice` antes de enviar, abortar si > threshold
  ```rust
  let gas_price = provider.get_gas_price().await?;
  if gas_price > max_gas_price { return Ok(()); }
  ```

- [ ] **6.4 Multi-hop arbs** — Actualmente solo 2-leg (buy+sell). Agregar 3-leg:
  - Token A → Token B (DEX 1) → Token C (DEX 2) → Token A (DEX 3)
  - Requiere cambios en contrato y arb detector

- [ ] **6.5 Camelot V2 router fix** — Verificar si necesita interfaz diferente (parametro `referrer`)

- [ ] **6.6 PancakeSwap V3 router** — Actualmente filtrado en executor por incompatibilidad:
  ```rust
  // executor/mod.rs linea 111-118
  // PCS V3 pools se skipean porque el router es diferente
  ```
  Agregar PCS V3 SwapRouter como alternativa para esos pools.

- [ ] **6.7 Timeboost** — Cuando Arbitrum publique las direcciones de los contratos:
  ```rust
  // timeboost/mod.rs lineas 42-43 — actualmente 0x000...0
  pub const EXPRESS_LANE_AUCTION: Address = address!("...");
  pub const EXPRESS_LANE_ROUTER: Address = address!("...");
  ```

- [ ] **6.8 Retry logic** — Cola de transacciones fallidas para re-intentar
  ```rust
  // Si tx falla por nonce: re-sync y reintentar
  // Si tx falla por gas: subir gas y reintentar
  // Si tx revierte: skip (profit ya no existe)
  ```

- [ ] **6.9 Mover log de dry-run** — De `/tmp/` a directorio del proyecto
  ```rust
  // main.rs linea 135
  let logger = Arc::new(DryRunLogger::new("./logs/arb_opportunities.jsonl"));
  ```

---

## Checklist Resumen Pre-Ejecucion

```
[ ] Key rotada y wallet fondeada con 0.05+ ETH
[ ] Deploy.s.sol sincronizado con ArbExecutor.sol
[ ] Tests de Foundry pasando con fork de mainnet
[ ] Contrato deployado y verificado en Arbiscan
[ ] ARB_CONTRACT actualizado en .env
[ ] Bot corriendo en dry-run 3+ dias sin crashes
[ ] Oportunidades validadas manualmente (al menos 5)
[ ] Primeras 10 ejecuciones reales monitoreadas
```

---

## Riesgos y Mitigaciones

| Riesgo | Probabilidad | Impacto | Mitigacion |
|--------|-------------|---------|-----------|
| Contrato tiene bug en callback | Media | Alto (fondos perdidos) | Tests con fork + dry run |
| Spreads son stale (reserves desactualizadas) | Alta | Medio (gas desperdiciado) | Simulation pre-flight (`eth_call`) |
| Competidor ejecuta arb antes | Alta | Bajo (tx revierte, pierde gas) | Timeboost + latencia minima |
| CamelotV2 router interfaz diferente | Media | Medio (reverts en ese DEX) | Test especifico en fork |
| Gas spike inesperado | Baja | Bajo ($0.01-0.10 por tx) | Gas ceiling check |
| Balancer vault sin liquidez del token | Baja | Alto (flash loan falla) | Verificar liquidez antes de arb |
| Nonce desync despues de crash | Media | Medio (txs fallan) | Sync nonce on startup (ya existe) |

---

## Orden de Prioridad (que hacer primero)

1. **Rotar key** — 10 min, bloqueante para todo lo demas
2. **Fix Deploy.s.sol** — 30 min, necesario para deploy
3. **Tests Foundry** — 1-2 dias, da confianza en el contrato
4. **Deploy** — 30 min
5. **Dry run** — 3-5 dias, valida el pipeline completo
6. **Live con capital minimo** — gradual
7. **Optimizaciones** — ongoing
