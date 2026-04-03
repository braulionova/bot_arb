#!/usr/bin/env python3
"""
On-chain Observer — Watches successful MEV arb txs from other bots.
Learns: what pools, routes, tokens, gas, and timing actually work.
Writes to ml/onchain_arbs.jsonl for model training.
"""

import json
import subprocess
import time
import sys
from datetime import datetime, timezone

OUTPUT = "/home/ubuntu/arbitrum_bot/ml/onchain_arbs.jsonl"

V3_SWAP = "0xc42079f94a6350d7e6235f29174924f928cc2ac818eb64fed8004e115fbcca67"
V2_SWAP = "0xd78ad95fa46c994b6551d0da85fc275fe613ce37657fb8d5e3d130840159d822"
TRANSFER = "0xddf252ad1be2c89b69c2b068fc378daa952ba7f163c4a11628f55a4df523b3ef"

KNOWN_TOKENS = {
    "0x82af49447d8a07e3bd95bd0d56f35241523fbab1": ("WETH", 18),
    "0xaf88d065e77c8cc2239327c5edb3a432268e5831": ("USDC", 6),
    "0xff970a61a04b1ca14834a43f5de4533ebddb5cc8": ("USDC.e", 6),
    "0xfd086bc7cd5c481dcc9c85ebe478a1c0b69fcbb9": ("USDT", 6),
    "0x912ce59144191c1204e64559fe8253a0e49e6548": ("ARB", 18),
    "0x2f2a2543b76a4166549f7aab2e75bef0aefc5b0f": ("WBTC", 8),
    "0xfc5a1a6eb076a2c7ad06ed22c90d7e710e35ad0a": ("GMX", 18),
    "0xf97f4df75117a78c1a5a0dbb814af92458539fb4": ("LINK", 18),
    "0x539bde0d7dbd336b79148aa742883198bbf60342": ("MAGIC", 18),
    "0xda10009cbd5d07dd0cecc66161fc93d7c9000da1": ("DAI", 18),
    "0x5979d7b546e38e414f7e9822514be443a4800529": ("wstETH", 18),
    "0x0c880f6761f1af8d9aa9c466984b80dab9a8c9e8": ("PENDLE", 18),
}

OUR_BOT = "0xd69f9856a569b1655b43b0395b7c2923a217cfe0"
OUR_CONTRACT = "0xaf96fa723d8c9823669f1329eaa795ff0ff530eb"


def rpc(method, params, url="https://arb1.arbitrum.io/rpc"):
    body = json.dumps({"jsonrpc": "2.0", "method": method, "params": params, "id": 1})
    try:
        r = subprocess.run(
            ["curl", "-s", "-m", "8", "-X", "POST", url,
             "-H", "Content-Type: application/json", "-d", body],
            capture_output=True, text=True, timeout=10
        )
        if r.stdout.strip():
            return json.loads(r.stdout)
    except:
        pass
    return {}


def analyze_block(block_num):
    """Analyze a block for successful arb txs"""
    block = rpc("eth_getBlockByNumber", [hex(block_num), True])
    result = block.get("result", {})
    if not result:
        return []

    timestamp = int(result.get("timestamp", "0x0"), 16)
    txs = result.get("transactions", [])
    arbs = []

    for tx in txs:
        tx_from = tx.get("from", "").lower()
        tx_to = (tx.get("to") or "").lower()
        tx_hash = tx.get("hash", "")

        # Skip our own txs
        if tx_from == OUR_BOT or tx_to == OUR_CONTRACT:
            continue

        # Skip simple transfers (no input data = not a contract call)
        if len(tx.get("input", "0x")) <= 10:
            continue

        # Get receipt
        receipt = rpc("eth_getTransactionReceipt", [tx_hash])
        r = receipt.get("result", {})
        if not r or r.get("status") != "0x1":
            continue

        logs = r.get("logs", [])
        swap_pools = set()
        swap_events = []
        transfer_events = []

        for log in logs:
            addr = log["address"].lower()
            topics = log.get("topics", [])
            data = log.get("data", "0x")

            if not topics:
                continue

            if topics[0] == V3_SWAP:
                swap_pools.add(addr)
                amount0 = int(data[2:66], 16)
                if amount0 > 2**255:
                    amount0 -= 2**256
                amount1 = int(data[66:130], 16)
                if amount1 > 2**255:
                    amount1 -= 2**256
                swap_events.append({
                    "pool": addr, "type": "v3",
                    "amount0": amount0, "amount1": amount1,
                })

            elif topics[0] == V2_SWAP:
                swap_pools.add(addr)
                if len(data) >= 258:
                    a0in = int(data[2:66], 16)
                    a1in = int(data[66:130], 16)
                    a0out = int(data[130:194], 16)
                    a1out = int(data[194:258], 16)
                    swap_events.append({
                        "pool": addr, "type": "v2",
                        "amount0In": a0in, "amount1In": a1in,
                        "amount0Out": a0out, "amount1Out": a1out,
                    })

            elif topics[0] == TRANSFER and len(topics) >= 3:
                token_name, dec = KNOWN_TOKENS.get(addr, (addr[:10], 18))
                amount = int(data, 16) if len(data) >= 66 else 0
                transfer_events.append({
                    "token": addr,
                    "token_name": token_name,
                    "from": "0x" + topics[1][26:],
                    "to": "0x" + topics[2][26:],
                    "amount": amount,
                    "decimals": dec,
                })

        # Only interested in multi-pool txs (arbs)
        if len(swap_pools) < 2:
            continue

        gas_used = int(r.get("gasUsed", "0x0"), 16)
        gas_price = int(tx.get("gasPrice", tx.get("maxFeePerGas", "0x0")), 16)

        arb_record = {
            "timestamp": datetime.fromtimestamp(timestamp, tz=timezone.utc).isoformat() if timestamp > 0 else "",
            "block": block_num,
            "tx_hash": tx_hash,
            "bot_contract": tx_to,
            "bot_wallet": tx_from,
            "n_pools": len(swap_pools),
            "pools": list(swap_pools),
            "n_swaps": len(swap_events),
            "gas_used": gas_used,
            "gas_price_gwei": gas_price / 1e9,
            "gas_cost_eth": gas_used * gas_price / 1e18,
            "input_length": len(tx.get("input", "")),
            "swap_events": swap_events,
            "n_transfers": len(transfer_events),
            "tokens_involved": list(set(t["token"] for t in transfer_events)),
        }

        arbs.append(arb_record)

    return arbs


def scan_range(start_block, end_block, output_file):
    """Scan a range of blocks for arbs"""
    total_arbs = 0

    with open(output_file, "a") as f:
        for block_num in range(start_block, end_block + 1):
            arbs = analyze_block(block_num)
            for arb in arbs:
                f.write(json.dumps(arb) + "\n")
                total_arbs += 1
                print(f"  ARB: block={arb['block']} pools={arb['n_pools']} "
                      f"swaps={arb['n_swaps']} gas={arb['gas_price_gwei']:.4f}gwei "
                      f"bot={arb['bot_contract'][:12]}")

            if (block_num - start_block) % 100 == 0:
                sys.stdout.write(f"\r  Block {block_num} ({block_num - start_block}/{end_block - start_block}) arbs={total_arbs}")
                sys.stdout.flush()

            time.sleep(0.02)

    print(f"\nTotal arbs found: {total_arbs}")
    return total_arbs


def watch():
    """Continuously watch for new arbs"""
    print("Watching for arbs in real-time...")
    last_block = int(rpc("eth_blockNumber", []).get("result", "0x0"), 16)

    while True:
        current = int(rpc("eth_blockNumber", []).get("result", "0x0"), 16)
        if current > last_block:
            for b in range(last_block + 1, current + 1):
                arbs = analyze_block(b)
                with open(OUTPUT, "a") as f:
                    for arb in arbs:
                        f.write(json.dumps(arb) + "\n")
                        print(f"  ARB: block={b} pools={arb['n_pools']} "
                              f"gas={arb['gas_price_gwei']:.4f}gwei "
                              f"tokens={len(arb['tokens_involved'])}")
            last_block = current
        time.sleep(1)


if __name__ == "__main__":
    mode = sys.argv[1] if len(sys.argv) > 1 else "scan"

    if mode == "scan":
        block = int(rpc("eth_blockNumber", []).get("result", "0x0"), 16)
        # Scan last 2000 blocks (~8 min)
        print(f"Scanning blocks {block-2000} to {block}")
        scan_range(block - 2000, block, OUTPUT)
    elif mode == "watch":
        watch()
    elif mode == "deep":
        block = int(rpc("eth_blockNumber", []).get("result", "0x0"), 16)
        # Deep scan: last 50000 blocks (~3.5 hours)
        print(f"Deep scanning {block-50000} to {block}")
        scan_range(block - 50000, block, OUTPUT)
