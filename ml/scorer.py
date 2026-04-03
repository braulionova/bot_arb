#!/usr/bin/env python3
"""
ML Scoring Service — HTTP server that the Rust bot calls to score opportunities.
Loads trained LightGBM model and returns prediction + confidence.

Endpoint: POST http://localhost:8090/score
Body: JSON with feature dict
Response: {"execute": true/false, "confidence": 0.95, "reason": "..."}
"""

import json
import sys
import lightgbm as lgb
from http.server import HTTPServer, BaseHTTPRequestHandler
from pathlib import Path

MODEL_PATH = "/home/ubuntu/arbitrum_bot/ml/model_v2.txt"
THRESHOLD_PATH = "/home/ubuntu/arbitrum_bot/ml/threshold.json"
PORT = 8090

# Load model
model = None
threshold = 0.8

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


def load_model():
    global model, threshold

    if not Path(MODEL_PATH).exists():
        print(f"No model at {MODEL_PATH}. Run train.py first.")
        return False

    model = lgb.Booster(model_file=MODEL_PATH)
    print(f"Model loaded: {MODEL_PATH}")

    if Path(THRESHOLD_PATH).exists():
        with open(THRESHOLD_PATH) as f:
            data = json.load(f)
            threshold = data.get("threshold", 0.8)
            print(f"Threshold: {threshold} (precision={data.get('precision', '?')})")

    return True


def score(features):
    """Score a single opportunity"""
    if model is None:
        return {"execute": False, "confidence": 0, "reason": "model not loaded"}

    # Build feature vector — pad missing features with 0
    # Read actual feature names from model
    expected = model.feature_name() if model else FEATURE_COLS
    vec = []
    for col in expected:
        vec.append(float(features.get(col, 0)))

    # Predict
    proba = model.predict([vec])[0]

    execute = bool(proba >= threshold)
    reason = "ML confidence sufficient" if execute else f"confidence {proba:.3f} < threshold {threshold:.3f}"

    return {
        "execute": execute,
        "confidence": float(proba),
        "threshold": float(threshold),
        "reason": reason,
    }


class Handler(BaseHTTPRequestHandler):
    def do_POST(self):
        if self.path == "/score":
            content_len = int(self.headers.get("Content-Length", 0))
            body = self.rfile.read(content_len)
            try:
                features = json.loads(body)
                result = score(features)
                self.send_response(200)
                self.send_header("Content-Type", "application/json")
                self.end_headers()
                self.wfile.write(json.dumps(result).encode())
            except Exception as e:
                self.send_response(400)
                self.end_headers()
                self.wfile.write(json.dumps({"error": str(e)}).encode())
        elif self.path == "/health":
            self.send_response(200)
            self.end_headers()
            self.wfile.write(b'{"status":"ok"}')
        else:
            self.send_response(404)
            self.end_headers()

    def log_message(self, format, *args):
        pass  # silent


def main():
    if not load_model():
        print("Starting without model — all predictions will be False")
        print("Train a model first: python3 ml/train.py")

    server = HTTPServer(("127.0.0.1", PORT), Handler)
    print(f"Scorer listening on http://127.0.0.1:{PORT}/score")
    print("Ctrl+C to stop")
    try:
        server.serve_forever()
    except KeyboardInterrupt:
        print("\nStopped")


if __name__ == "__main__":
    main()
