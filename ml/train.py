#!/usr/bin/env python3
"""
Train the arb prediction model.
Uses LightGBM to predict: will this arb succeed on-chain?

Input: ml/training_data.jsonl (from collector.py)
Output: ml/model.txt (LightGBM model)
        ml/model_stats.json (performance metrics)
"""

import json
import numpy as np
import pandas as pd
import lightgbm as lgb
from sklearn.model_selection import train_test_split
from sklearn.metrics import (
    classification_report, precision_score, recall_score,
    f1_score, roc_auc_score, confusion_matrix
)
from pathlib import Path

TRAINING_DATA = "/home/ubuntu/arbitrum_bot/ml/training_data.jsonl"
MODEL_PATH = "/home/ubuntu/arbitrum_bot/ml/model.txt"
STATS_PATH = "/home/ubuntu/arbitrum_bot/ml/model_stats.json"
THRESHOLD_PATH = "/home/ubuntu/arbitrum_bot/ml/threshold.json"

FEATURE_COLS = [
    "spread_pct", "net_spread_pct", "abs_spread",
    "buy_fee_bps", "sell_fee_bps", "total_fee_bps",
    "buy_dex_enc", "sell_dex_enc",
    "buy_is_v3", "sell_is_v3", "both_v3", "both_v2", "mixed_v2_v3",
    "stablecoin_in", "weth_involved",
    "sim_has_data", "profit_gross_eth", "profit_net_eth",
    "input_nonzero",
    "hour", "minute", "weekday",
    "profitable_local",
    "buy_pool_liquidity", "sell_pool_liquidity", "min_liquidity",
    "sim_latency_ms",
]


def load_data():
    """Load training data"""
    records = []
    with open(TRAINING_DATA) as f:
        for line in f:
            try:
                records.append(json.loads(line.strip()))
            except:
                continue

    df = pd.DataFrame(records)
    print(f"Loaded {len(df)} records")
    print(f"Label distribution:\n{df['label'].value_counts()}")
    print(f"Positive rate: {df['label'].mean():.4%}")
    return df


def train_model(df):
    """Train LightGBM classifier"""
    # Filter to valid features
    available = [c for c in FEATURE_COLS if c in df.columns]
    print(f"\nUsing {len(available)} features: {available}")

    X = df[available].fillna(0).apply(pd.to_numeric, errors='coerce').fillna(0)
    y = df["label"]

    # Handle extreme class imbalance
    n_pos = y.sum()
    n_neg = len(y) - n_pos

    if n_pos == 0:
        print("\nWARNING: No positive labels. Need more data with sequencer sim passes.")
        print("Run collector in watch mode with the bot running to get positive labels.")

        # Train anyway on proxy label: profitable_local
        if "profitable_local" in df.columns and df["profitable_local"].sum() > 0:
            print(f"\nUsing profitable_local as proxy label ({df['profitable_local'].sum()} positives)")
            y = df["profitable_local"]
            n_pos = y.sum()
            n_neg = len(y) - n_pos
        else:
            print("No proxy labels either. Cannot train.")
            return None

    print(f"\nClass balance: {n_neg} negative / {n_pos} positive (ratio {n_neg/max(n_pos,1):.0f}:1)")

    X_train, X_test, y_train, y_test = train_test_split(
        X, y, test_size=0.2, random_state=42, stratify=y
    )

    # LightGBM with tuning for imbalanced data
    params = {
        "objective": "binary",
        "metric": "auc",
        "boosting_type": "gbdt",
        "num_leaves": 31,
        "learning_rate": 0.05,
        "feature_fraction": 0.8,
        "bagging_fraction": 0.8,
        "bagging_freq": 5,
        "scale_pos_weight": n_neg / max(n_pos, 1),
        "min_child_samples": 10,
        "verbose": -1,
    }

    train_data = lgb.Dataset(X_train, label=y_train)
    valid_data = lgb.Dataset(X_test, label=y_test, reference=train_data)

    model = lgb.train(
        params,
        train_data,
        num_boost_round=500,
        valid_sets=[valid_data],
        callbacks=[lgb.early_stopping(50), lgb.log_evaluation(100)],
    )

    # Evaluate
    y_pred_proba = model.predict(X_test)

    # Find optimal threshold — maximize precision (we want NO false positives)
    best_threshold = 0.5
    best_precision = 0
    for t in np.arange(0.3, 0.99, 0.01):
        y_pred = (y_pred_proba >= t).astype(int)
        if y_pred.sum() > 0:
            prec = precision_score(y_test, y_pred, zero_division=0)
            rec = recall_score(y_test, y_pred, zero_division=0)
            # We want HIGH precision (>90%) — better to miss arbs than to revert
            if prec >= 0.9 and rec > 0:
                if prec > best_precision or (prec == best_precision and rec > recall_score(y_test, (y_pred_proba >= best_threshold).astype(int), zero_division=0)):
                    best_precision = prec
                    best_threshold = t

    # If no threshold gives 90% precision, use highest precision available
    if best_precision == 0:
        for t in np.arange(0.5, 0.99, 0.01):
            y_pred = (y_pred_proba >= t).astype(int)
            if y_pred.sum() > 0:
                prec = precision_score(y_test, y_pred, zero_division=0)
                if prec > best_precision:
                    best_precision = prec
                    best_threshold = t

    y_pred = (y_pred_proba >= best_threshold).astype(int)

    print(f"\n{'='*60}")
    print(f"OPTIMAL THRESHOLD: {best_threshold:.2f}")
    print(f"{'='*60}")
    print(f"\nClassification Report (threshold={best_threshold:.2f}):")
    print(classification_report(y_test, y_pred, zero_division=0))
    print(f"Confusion Matrix:")
    print(confusion_matrix(y_test, y_pred))

    try:
        auc = roc_auc_score(y_test, y_pred_proba)
        print(f"\nAUC: {auc:.4f}")
    except:
        auc = 0

    # Feature importance
    importance = model.feature_importance(importance_type='gain')
    feat_imp = sorted(zip(available, importance), key=lambda x: -x[1])
    print(f"\nTop features:")
    for feat, imp in feat_imp[:10]:
        print(f"  {feat:30s} {imp:.1f}")

    # Save model
    model.save_model(MODEL_PATH)
    print(f"\nModel saved to {MODEL_PATH}")

    # Save threshold
    with open(THRESHOLD_PATH, "w") as f:
        json.dump({"threshold": best_threshold, "precision": best_precision}, f)
    print(f"Threshold saved to {THRESHOLD_PATH}")

    # Save stats
    stats = {
        "n_train": len(X_train),
        "n_test": len(X_test),
        "n_positive": int(y.sum()),
        "n_negative": int(len(y) - y.sum()),
        "threshold": best_threshold,
        "precision": best_precision,
        "recall": recall_score(y_test, y_pred, zero_division=0),
        "f1": f1_score(y_test, y_pred, zero_division=0),
        "auc": auc,
        "top_features": [{"name": n, "importance": float(i)} for n, i in feat_imp[:15]],
    }
    with open(STATS_PATH, "w") as f:
        json.dump(stats, f, indent=2)

    return model


if __name__ == "__main__":
    df = load_data()
    model = train_model(df)
    if model:
        print("\nModel ready. Run scorer.py to start the prediction service.")
    else:
        print("\nNeed more labeled data. Run: python3 ml/collector.py watch")
