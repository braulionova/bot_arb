#!/usr/bin/env python3
"""
Arb Opportunity Collector — Observes market and labels opportunities.
Runs alongside the Rust bot. Enriches raw opportunities with:
- Fresh on-chain simulation result (sequencer eth_call)
- Pool liquidity depth at time of detection
- Market volatility (price change over last N blocks)
- Time features (hour, minute)
- Latency measurement (how fast we could have executed)

Writes enriched data to ml/training_data.jsonl for model training.
"""

import json
import time
import subprocess
import os
import sys
from datetime import datetime
from pathlib import Path

RAW_LOG = "/home/ubuntu/arbitrum_bot/logs/arb_opportunities.jsonl"
TRAINING_LOG = "/home/ubuntu/arbitrum_bot/ml/training_data.jsonl"
CONTRACT = "0xaF96FA723D8C9823669F1329EaA795FF0fF530Eb"
BOT = "0xd69F9856A569B1655B43B0395b7c2923a217Cfe0"
SEQUENCER_RPC = "https://arb1.arbitrum.io/rpc"

# Known token decimals
DECIMALS = {
    "0x82af49447d8a07e3bd95bd0d56f35241523fbab1": 18,  # WETH
    "0xaf88d065e77c8cc2239327c5edb3a432268e5831": 6,   # USDC
    "0xff970a61a04b1ca14834a43f5de4533ebddb5cc8": 6,   # USDC.e
    "0xfd086bc7cd5c481dcc9c85ebe478a1c0b69fcbb9": 6,   # USDT
    "0x912ce59144191c1204e64559fe8253a0e49e6548": 18,  # ARB
    "0x2f2a2543b76a4166549f7aab2e75bef0aefc5b0f": 8,   # WBTC
    "0xfc5a1a6eb076a2c7ad06ed22c90d7e710e35ad0a": 18,  # GMX
    "0xf97f4df75117a78c1a5a0dbb814af92458539fb4": 18,  # LINK
    "0x539bde0d7dbd336b79148aa742883198bbf60342": 18,  # MAGIC
}

# DEX type encoding
DEX_ENCODE = {
    "UniswapV3": 0, "UniswapV2": 1, "PancakeSwapV3": 2,
    "CamelotV2": 3, "CamelotV3": 4, "SushiSwapV3": 5,
    "SushiSwapV2": 6, "RamsesV2": 7, "CurveStable": 8,
    "BalancerStable": 9, "Unknown": 10,
}


def rpc_call(method, params, rpc=SEQUENCER_RPC):
    body = json.dumps({"jsonrpc": "2.0", "method": method, "params": params, "id": 1})
    try:
        r = subprocess.run(
            ["curl", "-s", "-m", "3", "-X", "POST", rpc,
             "-H", "Content-Type: application/json", "-d", body],
            capture_output=True, text=True, timeout=5
        )
        if r.stdout.strip():
            return json.loads(r.stdout)
    except:
        pass
    return {"error": {"message": "timeout"}}


def simulate_arb(buy_pool, sell_pool, token_in, token_out, amount, buy_is_v3, sell_is_v3):
    """Simulate the flash loan arb on the sequencer — returns (passed, latency_ms)"""
    try:
        r = subprocess.run([
            "cast", "calldata",
            "executeArbFlashLoan(address,uint256,address,address,address,bool,bool,uint256)",
            token_in, str(amount), buy_pool, sell_pool, token_out,
            str(buy_is_v3).lower(), str(sell_is_v3).lower(), "0"
        ], capture_output=True, text=True, timeout=5)
        calldata = r.stdout.strip()
        if not calldata:
            return False, 0

        t0 = time.time()
        result = rpc_call("eth_call", [{
            "from": BOT, "to": CONTRACT, "data": calldata
        }, "latest"])
        latency_ms = (time.time() - t0) * 1000

        passed = "error" not in result
        return passed, latency_ms
    except:
        return False, 0


def get_pool_liquidity(pool_addr, is_v3):
    """Get current liquidity for a pool"""
    if is_v3:
        r = rpc_call("eth_call", [{"to": pool_addr, "data": "0x1a686502"}, "latest"])
        res = r.get("result", "0x0")
        if len(res) >= 66:
            return int(res[2:66], 16)
    else:
        r = rpc_call("eth_call", [{"to": pool_addr, "data": "0x0902f1ac"}, "latest"])
        res = r.get("result", "0x")
        if len(res) >= 130:
            r0 = int(res[2:66], 16)
            r1 = int(res[66:130], 16)
            return r0 + r1  # total reserve as proxy for liquidity
    return 0


def extract_features(opp):
    """Extract ML features from a raw opportunity"""
    dt = datetime.fromisoformat(opp["timestamp"].replace("+00:00", ""))

    buy_dex = opp.get("buy_dex", "Unknown")
    sell_dex = opp.get("sell_dex", "Unknown")

    token_in = opp.get("trigger_token_in", "").lower()
    token_out = opp.get("trigger_token_out", "").lower()

    # Is it a V3 pool?
    buy_is_v3 = buy_dex in ["UniswapV3", "PancakeSwapV3", "SushiSwapV3", "CamelotV3"]
    sell_is_v3 = sell_dex in ["UniswapV3", "PancakeSwapV3", "SushiSwapV3", "CamelotV3"]

    # Pool competition level
    both_v3 = buy_is_v3 and sell_is_v3
    both_v2 = not buy_is_v3 and not sell_is_v3
    mixed = not both_v3 and not both_v2

    # Token classification
    stablecoin_in = token_in in [
        "0xaf88d065e77c8cc2239327c5edb3a432268e5831",
        "0xff970a61a04b1ca14834a43f5de4533ebddb5cc8",
        "0xfd086bc7cd5c481dcc9c85ebe478a1c0b69fcbb9",
    ]
    weth_involved = "82af49447d8a07e3bd95bd0d56f35241523fbab1" in token_in or \
                    "82af49447d8a07e3bd95bd0d56f35241523fbab1" in token_out

    features = {
        # Price features
        "spread_pct": opp.get("spread_pct", 0),
        "net_spread_pct": opp.get("net_spread_pct", 0),
        "abs_spread": abs(opp.get("spread_pct", 0)),

        # Fee features
        "buy_fee_bps": opp.get("buy_fee_bps", 0),
        "sell_fee_bps": opp.get("sell_fee_bps", 0),
        "total_fee_bps": opp.get("buy_fee_bps", 0) + opp.get("sell_fee_bps", 0),

        # DEX features
        "buy_dex_enc": DEX_ENCODE.get(buy_dex, 10),
        "sell_dex_enc": DEX_ENCODE.get(sell_dex, 10),
        "buy_is_v3": int(buy_is_v3),
        "sell_is_v3": int(sell_is_v3),
        "both_v3": int(both_v3),
        "both_v2": int(both_v2),
        "mixed_v2_v3": int(mixed),

        # Token features
        "stablecoin_in": int(stablecoin_in),
        "weth_involved": int(weth_involved),

        # Simulation features
        "sim_bought": int(opp.get("sim_bought", "0")),
        "sim_sold": int(opp.get("sim_sold", "0")),
        "sim_has_data": int(opp.get("sim_bought", "0") != "0"),
        "profit_gross_eth": opp.get("profit_gross_eth", 0),
        "profit_net_eth": opp.get("profit_net_eth", 0),

        # Amount features
        "optimal_input": int(opp.get("optimal_input", "0")),
        "input_nonzero": int(opp.get("optimal_input", "0") != "0"),

        # Time features
        "hour": dt.hour,
        "minute": dt.minute,
        "weekday": dt.weekday(),

        # Original label
        "profitable_local": int(opp.get("profitable", False)),
    }

    return features, buy_is_v3, sell_is_v3


def enrich_and_label(opp):
    """Enrich opportunity with on-chain sim and label it"""
    features, buy_is_v3, sell_is_v3 = extract_features(opp)

    # Only simulate opportunities that look promising locally
    if features["spread_pct"] > 0 and features["net_spread_pct"] > 0:
        optimal = opp.get("optimal_input", "0")
        if optimal != "0" and int(optimal) > 0:
            passed, latency = simulate_arb(
                opp["buy_pool"], opp["sell_pool"],
                opp["trigger_token_in"], opp["trigger_token_out"],
                optimal, buy_is_v3, sell_is_v3
            )
            features["sequencer_sim_passed"] = int(passed)
            features["sim_latency_ms"] = latency

            # Get liquidity at time of detection
            buy_liq = get_pool_liquidity(opp["buy_pool"], buy_is_v3)
            sell_liq = get_pool_liquidity(opp["sell_pool"], sell_is_v3)
            features["buy_pool_liquidity"] = buy_liq
            features["sell_pool_liquidity"] = sell_liq
            features["min_liquidity"] = min(buy_liq, sell_liq)

            # Label: 1 = would have succeeded on-chain
            features["label"] = int(passed)
        else:
            features["sequencer_sim_passed"] = 0
            features["sim_latency_ms"] = 0
            features["buy_pool_liquidity"] = 0
            features["sell_pool_liquidity"] = 0
            features["min_liquidity"] = 0
            features["label"] = 0
    else:
        features["sequencer_sim_passed"] = 0
        features["sim_latency_ms"] = 0
        features["buy_pool_liquidity"] = 0
        features["sell_pool_liquidity"] = 0
        features["min_liquidity"] = 0
        features["label"] = 0

    return features


def process_historical():
    """Process existing historical data"""
    print(f"Processing historical data from {RAW_LOG}")
    count = 0
    positive = 0
    simmed = 0

    with open(RAW_LOG) as f, open(TRAINING_LOG, "a") as out:
        for line in f:
            try:
                opp = json.loads(line.strip())
            except:
                continue

            features = None
            # Only enrich promising ones (positive spread) to save RPC calls
            if opp.get("spread_pct", 0) > 0.05 and opp.get("net_spread_pct", 0) > 0:
                features = enrich_and_label(opp)
                simmed += 1
                if features.get("label"):
                    positive += 1
                time.sleep(0.02)  # rate limit
            else:
                features, _, _ = extract_features(opp)
                features["sequencer_sim_passed"] = 0
                features["sim_latency_ms"] = 0
                features["buy_pool_liquidity"] = 0
                features["sell_pool_liquidity"] = 0
                features["min_liquidity"] = 0
                features["label"] = 0

            out.write(json.dumps(features) + "\n")
            count += 1

            if count % 1000 == 0:
                print(f"  Processed {count} | simmed={simmed} | positive={positive}")

    print(f"Done: {count} total | {simmed} simulated | {positive} positive labels")
    return count


def watch_mode():
    """Continuously watch for new opportunities and enrich them"""
    print("Entering watch mode — monitoring for new opportunities...")
    last_size = os.path.getsize(RAW_LOG) if os.path.exists(RAW_LOG) else 0

    while True:
        try:
            current_size = os.path.getsize(RAW_LOG)
            if current_size > last_size:
                with open(RAW_LOG) as f:
                    f.seek(last_size)
                    new_lines = f.readlines()
                last_size = current_size

                with open(TRAINING_LOG, "a") as out:
                    for line in new_lines:
                        try:
                            opp = json.loads(line.strip())
                            if opp.get("spread_pct", 0) > 0.05:
                                features = enrich_and_label(opp)
                                out.write(json.dumps(features) + "\n")
                                if features.get("label"):
                                    print(f"  POSITIVE: spread={features['spread_pct']:.3f}% "
                                          f"sim_passed={features['sequencer_sim_passed']}")
                        except:
                            continue
        except Exception as e:
            print(f"Watch error: {e}")

        time.sleep(2)


if __name__ == "__main__":
    mode = sys.argv[1] if len(sys.argv) > 1 else "historical"

    if mode == "historical":
        process_historical()
    elif mode == "watch":
        watch_mode()
    else:
        print(f"Usage: {sys.argv[0]} [historical|watch]")
