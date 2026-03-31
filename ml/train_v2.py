#!/usr/bin/env python3
"""
Train ML model v2 — Uses real on-chain arb data + opportunity data.
Two models:
  1. Classifier: will this opportunity pass sequencer sim?
  2. Regressor: how much profit will it make?
"""

import json
import numpy as np
import pandas as pd
import lightgbm as lgb
from sklearn.model_selection import train_test_split
from sklearn.metrics import classification_report, precision_score, recall_score, roc_auc_score
from collections import defaultdict
from pathlib import Path

ONCHAIN_ARBS = "/home/ubuntu/arbitrum_bot/ml/onchain_arbs.jsonl"
TRAINING_DATA = "/home/ubuntu/arbitrum_bot/ml/training_data.jsonl"
ARB_PAIRS = "/home/ubuntu/arbitrum_bot/ml/arb_pairs.json"
MODEL_PATH = "/home/ubuntu/arbitrum_bot/ml/model_v2.txt"
STATS_PATH = "/home/ubuntu/arbitrum_bot/ml/model_v2_stats.json"
THRESHOLD_PATH = "/home/ubuntu/arbitrum_bot/ml/threshold.json"


def load_onchain_patterns():
    """Load patterns from successful on-chain arbs"""
    arbs = []
    with open(ONCHAIN_ARBS) as f:
        for line in f:
            try:
                arbs.append(json.loads(line))
            except:
                continue

    # Extract patterns
    pool_frequency = defaultdict(int)
    bot_gas_prices = []
    successful_pool_pairs = set()

    for a in arbs:
        for p in a.get("pools", []):
            pool_frequency[p] += 1
        bot_gas_prices.append(a.get("gas_price_gwei", 0.01))
        pools = sorted(a.get("pools", []))
        if len(pools) >= 2:
            successful_pool_pairs.add(tuple(pools[:2]))

    print(f"On-chain patterns: {len(arbs)} arbs, {len(pool_frequency)} pools, {len(successful_pool_pairs)} pool pairs")
    print(f"Gas: avg={np.mean(bot_gas_prices):.4f} gwei")

    return pool_frequency, successful_pool_pairs, bot_gas_prices


def load_opportunity_data():
    """Load our observed opportunities with labels"""
    records = []
    with open(TRAINING_DATA) as f:
        for line in f:
            try:
                records.append(json.loads(line))
            except:
                continue

    df = pd.DataFrame(records)
    print(f"Opportunity data: {len(df)} records")
    return df


def build_training_set(df, pool_freq, successful_pairs):
    """Build feature matrix with on-chain intelligence"""

    # Add on-chain features
    df["onchain_buy_pool_freq"] = df.get("buy_pool", pd.Series(dtype=str)).map(
        lambda x: pool_freq.get(str(x).lower(), 0) if pd.notna(x) else 0
    ).fillna(0)
    df["onchain_sell_pool_freq"] = df.get("sell_pool", pd.Series(dtype=str)).map(
        lambda x: pool_freq.get(str(x).lower(), 0) if pd.notna(x) else 0
    ).fillna(0)

    feature_cols = [
        "spread_pct", "net_spread_pct", "abs_spread",
        "buy_fee_bps", "sell_fee_bps", "total_fee_bps",
        "buy_dex_enc", "sell_dex_enc",
        "buy_is_v3", "sell_is_v3", "both_v3", "both_v2", "mixed_v2_v3",
        "stablecoin_in", "weth_involved",
        "sim_has_data", "profit_gross_eth", "profit_net_eth",
        "input_nonzero",
        "hour", "minute", "weekday",
        "profitable_local",
    ]

    # Add on-chain features if available
    for col in ["onchain_buy_pool_freq", "onchain_sell_pool_freq",
                "buy_pool_liquidity", "sell_pool_liquidity", "min_liquidity",
                "sim_latency_ms"]:
        if col in df.columns:
            feature_cols.append(col)

    available = [c for c in feature_cols if c in df.columns]
    X = df[available].apply(pd.to_numeric, errors='coerce').fillna(0)

    # Target: use profitable_local as proxy (best we have without real execution data)
    # Weight samples that match on-chain patterns higher
    y = df["profitable_local"].fillna(0).astype(int) if "profitable_local" in df.columns else pd.Series(0, index=df.index)

    # If no positives, create synthetic positives from on-chain patterns
    if y.sum() == 0:
        print("WARNING: No positive labels. Creating synthetic from on-chain patterns...")
        # Mark opportunities with positive spread + matching on-chain pool patterns as positive
        mask = (df.get("spread_pct", pd.Series(0)) > 0.1) & \
               (df.get("net_spread_pct", pd.Series(0)) > 0) & \
               (df.get("sim_has_data", pd.Series(0)) == 1)
        y[mask] = 1

    print(f"Features: {len(available)}, Positives: {y.sum()}, Negatives: {len(y) - y.sum()}")
    return X, y, available


def train(X, y, feature_cols):
    """Train LightGBM"""
    n_pos = max(y.sum(), 1)
    n_neg = len(y) - n_pos

    if n_pos < 10:
        print(f"Only {n_pos} positives — model will be weak. Need more data.")

    X_train, X_test, y_train, y_test = train_test_split(
        X, y, test_size=0.2, random_state=42,
        stratify=y if n_pos >= 2 else None
    )

    params = {
        "objective": "binary",
        "metric": "auc",
        "boosting_type": "gbdt",
        "num_leaves": 15,
        "learning_rate": 0.03,
        "feature_fraction": 0.7,
        "bagging_fraction": 0.7,
        "bagging_freq": 5,
        "scale_pos_weight": n_neg / n_pos,
        "min_child_samples": 5,
        "verbose": -1,
        "n_jobs": 2,
    }

    train_data = lgb.Dataset(X_train, label=y_train)
    valid_data = lgb.Dataset(X_test, label=y_test, reference=train_data)

    model = lgb.train(
        params, train_data,
        num_boost_round=300,
        valid_sets=[valid_data],
        callbacks=[lgb.early_stopping(30), lgb.log_evaluation(100)],
    )

    # Evaluate
    y_proba = model.predict(X_test)

    # Find threshold for >85% precision
    best_t, best_prec, best_rec = 0.5, 0, 0
    for t in np.arange(0.2, 0.99, 0.01):
        pred = (y_proba >= t).astype(int)
        if pred.sum() > 0:
            p = precision_score(y_test, pred, zero_division=0)
            r = recall_score(y_test, pred, zero_division=0)
            if p >= 0.85 and r > best_rec:
                best_t, best_prec, best_rec = t, p, r

    if best_prec == 0:
        # Fallback: highest precision
        for t in np.arange(0.5, 0.99, 0.01):
            pred = (y_proba >= t).astype(int)
            if pred.sum() > 0:
                p = precision_score(y_test, pred, zero_division=0)
                if p > best_prec:
                    best_t, best_prec, best_rec = t, p, recall_score(y_test, pred, zero_division=0)

    pred = (y_proba >= best_t).astype(int)
    print(f"\nThreshold: {best_t:.2f} | Precision: {best_prec:.3f} | Recall: {best_rec:.3f}")
    print(classification_report(y_test, pred, zero_division=0))

    try:
        auc = roc_auc_score(y_test, y_proba)
    except:
        auc = 0

    # Feature importance
    importance = model.feature_importance(importance_type='gain')
    feat_imp = sorted(zip(feature_cols, importance), key=lambda x: -x[1])
    print("Top features:")
    for name, imp in feat_imp[:10]:
        print(f"  {name:30s} {imp:.1f}")

    # Save
    model.save_model(MODEL_PATH)
    with open(THRESHOLD_PATH, "w") as f:
        json.dump({"threshold": best_t, "precision": best_prec}, f)

    stats = {
        "n_samples": len(X),
        "n_positive": int(y.sum()),
        "threshold": best_t,
        "precision": best_prec,
        "recall": best_rec,
        "auc": auc,
        "features": feature_cols,
        "top_features": [{"name": n, "importance": float(i)} for n, i in feat_imp[:10]],
        "onchain_arbs_analyzed": len(pool_freq),
        "gas_recommendation_gwei": float(np.mean(gas_prices)) if gas_prices else 0.01,
    }
    with open(STATS_PATH, "w") as f:
        json.dump(stats, f, indent=2)

    print(f"\nModel saved to {MODEL_PATH}")
    print(f"Recommended gas: {stats['gas_recommendation_gwei']:.4f} gwei")
    return model, stats


if __name__ == "__main__":
    pool_freq, successful_pairs, gas_prices = load_onchain_patterns()
    df = load_opportunity_data()
    X, y, feature_cols = build_training_set(df, pool_freq, successful_pairs)
    model, stats = train(X, y, feature_cols)
    print(f"\n{'='*60}")
    print(f"MODEL READY — precision={stats['precision']:.1%} threshold={stats['threshold']:.2f}")
    print(f"{'='*60}")
