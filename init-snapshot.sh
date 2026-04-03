#!/bin/bash
set -eo pipefail

BASE="https://snapshot.arbitrum.foundation/arb1/2026-03-28-ab33b7bf/pruned.tar"
DATA_DIR="/data/arbitrum/arb1/nitro"
BIN="./target/release/stream-download"

echo "=== Arbitrum Snapshot Download (Rust, 10 threads, HTTP/1.1) ==="
df -h / | tail -1
echo ""

mkdir -p "$DATA_DIR"
rm -rf "$DATA_DIR"/* 2>/dev/null

$BIN \
    "${BASE}.part0000" \
    "${BASE}.part0001" \
    "${BASE}.part0002" \
    "${BASE}.part0003" \
    "${BASE}.part0004" \
    --threads 10 \
    | tar -xf - -C "$DATA_DIR"

echo ""
echo "=== Extraction complete ==="
ls "$DATA_DIR"
du -sh "$DATA_DIR"
df -h / | tail -1

# Remove LOCK file left by tar extraction
rm -f "$DATA_DIR/LOCK"

echo ""
echo "=== Starting node ==="
cd /home/ubuntu/arbitrum_bot
sudo docker compose up -d arb-node
sleep 15
sudo docker logs --tail 10 arb-node 2>&1
echo "=== DONE ==="
