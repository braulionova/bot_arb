#!/usr/bin/env python3
"""
Feature Extractor — Generates rich ML features from opportunity + on-chain data.
Combines: raw opportunity data + pool discovery + on-chain arb patterns.
"""

import json
import os
from datetime import datetime
from collections import defaultdict

ONCHAIN_ARBS = "/home/ubuntu/arbitrum_bot/ml/onchain_arbs.jsonl"
ARB_PAIRS = "/home/ubuntu/arbitrum_bot/ml/arb_pairs.json"
ALL_POOLS = "/home/ubuntu/arbitrum_bot/ml/all_pools.json"

# Competition score: how many other bots are arbing this pool?
# Computed from on-chain observations
_pool_competition = {}
_pair_success_rate = {}


def load_onchain_stats():
    """Compute competition and success stats from on-chain observations"""
    global _pool_competition, _pair_success_rate

    if not os.path.exists(ONCHAIN_ARBS):
        return

    pool_counts = defaultdict(int)
    bot_counts = defaultdict(int)
    pair_arbs = defaultdict(int)

    with open(ONCHAIN_ARBS) as f:
        for line in f:
            try:
                arb = json.loads(line)
                for pool in arb.get("pools", []):
                    pool_counts[pool] += 1
                bot_counts[arb.get("bot_contract", "")] += 1
                tokens = sorted(arb.get("tokens_involved", []))
                if len(tokens) >= 2:
                    pair_key = f"{tokens[0]}_{tokens[1]}"
                    pair_arbs[pair_key] += 1
            except:
                continue

    _pool_competition = dict(pool_counts)
    _pair_success_rate = dict(pair_arbs)

    print(f"Loaded on-chain stats: {len(pool_counts)} pools, {len(bot_counts)} bots, {len(pair_arbs)} pairs")


def get_competition_score(pool_addr):
    """How many arbs have been done on this pool by other bots?"""
    return _pool_competition.get(pool_addr.lower(), 0)


def get_pair_arb_frequency(token0, token1):
    """How often is this pair arbed by other bots?"""
    key = "_".join(sorted([token0.lower(), token1.lower()]))
    return _pair_success_rate.get(key, 0)


DEX_ENCODE = {
    "UniswapV3": 0, "UniswapV2": 1, "PancakeSwapV3": 2,
    "CamelotV2": 3, "CamelotV3": 4, "SushiSwapV3": 5,
    "SushiSwapV2": 6, "RamsesV2": 7, "CurveStable": 8,
    "BalancerStable": 9, "TraderJoeV1": 10, "ZyberswapV2": 11,
    "Unknown": 99,
}

STABLECOINS = {
    "0xaf88d065e77c8cc2239327c5edb3a432268e5831",  # USDC
    "0xff970a61a04b1ca14834a43f5de4533ebddb5cc8",  # USDC.e
    "0xfd086bc7cd5c481dcc9c85ebe478a1c0b69fcbb9",  # USDT
    "0xda10009cbd5d07dd0cecc66161fc93d7c9000da1",  # DAI
}

WETH = "0x82af49447d8a07e3bd95bd0d56f35241523fbab1"


def extract_features(opp, timestamp=None):
    """Extract full ML feature set from an opportunity record"""
    if timestamp is None:
        ts = opp.get("timestamp", "")
        try:
            dt = datetime.fromisoformat(ts.replace("+00:00", "").replace("Z", ""))
        except:
            dt = datetime.utcnow()
    else:
        dt = datetime.utcfromtimestamp(timestamp)

    buy_dex = opp.get("buy_dex", "Unknown")
    sell_dex = opp.get("sell_dex", "Unknown")
    token_in = opp.get("trigger_token_in", "").lower()
    token_out = opp.get("trigger_token_out", "").lower()
    buy_pool = opp.get("buy_pool", "").lower()
    sell_pool = opp.get("sell_pool", "").lower()

    buy_is_v3 = buy_dex in ["UniswapV3", "PancakeSwapV3", "SushiSwapV3", "CamelotV3"]
    sell_is_v3 = sell_dex in ["UniswapV3", "PancakeSwapV3", "SushiSwapV3", "CamelotV3"]
    buy_is_v2 = buy_dex in ["UniswapV2", "CamelotV2", "SushiSwapV2", "RamsesV2", "TraderJoeV1", "ZyberswapV2"]
    sell_is_v2 = sell_dex in ["UniswapV2", "CamelotV2", "SushiSwapV2", "RamsesV2", "TraderJoeV1", "ZyberswapV2"]

    # Competition features
    buy_competition = get_competition_score(buy_pool)
    sell_competition = get_competition_score(sell_pool)
    pair_frequency = get_pair_arb_frequency(token_in, token_out)

    features = {
        # === Price features ===
        "spread_pct": opp.get("spread_pct", 0),
        "net_spread_pct": opp.get("net_spread_pct", 0),
        "abs_spread": abs(opp.get("spread_pct", 0)),
        "spread_positive": int(opp.get("spread_pct", 0) > 0),

        # === Fee features ===
        "buy_fee_bps": opp.get("buy_fee_bps", 0),
        "sell_fee_bps": opp.get("sell_fee_bps", 0),
        "total_fee_bps": opp.get("buy_fee_bps", 0) + opp.get("sell_fee_bps", 0),
        "fee_asymmetry": abs(opp.get("buy_fee_bps", 0) - opp.get("sell_fee_bps", 0)),

        # === DEX type features ===
        "buy_dex_enc": DEX_ENCODE.get(buy_dex, 99),
        "sell_dex_enc": DEX_ENCODE.get(sell_dex, 99),
        "buy_is_v3": int(buy_is_v3),
        "sell_is_v3": int(sell_is_v3),
        "both_v3": int(buy_is_v3 and sell_is_v3),
        "both_v2": int(buy_is_v2 and sell_is_v2),
        "mixed_v2_v3": int(buy_is_v2 != sell_is_v2),
        "same_dex_type": int(buy_dex == sell_dex),

        # === Token features ===
        "is_stablecoin_pair": int(token_in in STABLECOINS and token_out in STABLECOINS),
        "has_stablecoin": int(token_in in STABLECOINS or token_out in STABLECOINS),
        "weth_involved": int(WETH in [token_in, token_out]),
        "both_major": int(token_in in STABLECOINS | {WETH} and token_out in STABLECOINS | {WETH}),

        # === Simulation features ===
        "sim_bought": int(opp.get("sim_bought", "0") or "0"),
        "sim_sold": int(opp.get("sim_sold", "0") or "0"),
        "sim_has_data": int(str(opp.get("sim_bought", "0")) != "0"),
        "profit_gross_eth": float(opp.get("profit_gross_eth", 0) or 0),
        "profit_net_eth": float(opp.get("profit_net_eth", 0) or 0),
        "optimal_input": int(opp.get("optimal_input", "0") or "0"),
        "input_nonzero": int(str(opp.get("optimal_input", "0")) != "0"),

        # === Competition features (from on-chain data) ===
        "buy_pool_competition": buy_competition,
        "sell_pool_competition": sell_competition,
        "max_pool_competition": max(buy_competition, sell_competition),
        "pair_arb_frequency": pair_frequency,
        "is_high_competition": int(pair_frequency > 10),

        # === Time features ===
        "hour": dt.hour,
        "minute": dt.minute,
        "weekday": dt.weekday(),
        "is_weekend": int(dt.weekday() >= 5),
        "is_us_hours": int(13 <= dt.hour <= 21),  # UTC -> US market hours
        "is_asia_hours": int(0 <= dt.hour <= 8),

        # === Liquidity features (filled by collector if available) ===
        "buy_pool_liquidity": float(opp.get("buy_pool_liquidity", 0) or 0),
        "sell_pool_liquidity": float(opp.get("sell_pool_liquidity", 0) or 0),
        "min_liquidity": float(opp.get("min_liquidity", 0) or 0),

        # === Derived features ===
        "profit_per_gas": float(opp.get("profit_net_eth", 0) or 0) / 0.000012 if opp.get("profit_net_eth", 0) else 0,
        "spread_vs_fees": float(opp.get("spread_pct", 0)) / max(opp.get("buy_fee_bps", 1) + opp.get("sell_fee_bps", 1), 1) * 100,
    }

    return features


# Feature columns for model training (must match train.py)
FEATURE_COLS = [
    "spread_pct", "net_spread_pct", "abs_spread", "spread_positive",
    "buy_fee_bps", "sell_fee_bps", "total_fee_bps", "fee_asymmetry",
    "buy_dex_enc", "sell_dex_enc",
    "buy_is_v3", "sell_is_v3", "both_v3", "both_v2", "mixed_v2_v3", "same_dex_type",
    "is_stablecoin_pair", "has_stablecoin", "weth_involved", "both_major",
    "sim_has_data", "profit_gross_eth", "profit_net_eth", "input_nonzero",
    "buy_pool_competition", "sell_pool_competition", "max_pool_competition",
    "pair_arb_frequency", "is_high_competition",
    "hour", "minute", "weekday", "is_weekend", "is_us_hours", "is_asia_hours",
    "buy_pool_liquidity", "sell_pool_liquidity", "min_liquidity",
    "profit_per_gas", "spread_vs_fees",
]

if __name__ == "__main__":
    load_onchain_stats()
    # Test with a sample opportunity
    sample = {
        "timestamp": "2026-03-31T09:00:00",
        "buy_dex": "CamelotV2",
        "sell_dex": "UniswapV3",
        "trigger_token_in": WETH,
        "trigger_token_out": "0xaf88d065e77c8cc2239327c5edb3a432268e5831",
        "buy_pool": "0x54b26faf3671677c19f70c4b879a6f7b898f732c",
        "sell_pool": "0xc6962004f452be9203591991d15f6b388e09e8d0",
        "spread_pct": 0.5,
        "net_spread_pct": 0.15,
        "buy_fee_bps": 30,
        "sell_fee_bps": 5,
    }
    features = extract_features(sample)
    print(json.dumps(features, indent=2))
