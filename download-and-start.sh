#!/bin/bash
set -eo pipefail

SNAPSHOT_URL="https://snapshot.arbitrum.foundation/arb1/2026-03-28-ab33b7bf/pruned.tar.part0000"
TAR_PATH="/data/arbitrum/snapshot-pruned.tar"
DATA_DIR="/data/arbitrum/arb1/nitro"

echo "=== Step 1: Download with resume (HTTP/1.1) ==="
df -h / | tail -1

wget -c --progress=dot:giga \
    --timeout=120 \
    --waitretry=30 \
    --tries=0 \
    --no-http-keep-alive \
    -O "$TAR_PATH" \
    "$SNAPSHOT_URL"

echo ""
echo "Download complete:"
ls -lh "$TAR_PATH"

echo ""
echo "=== Step 2: Extract ==="
rm -rf "$DATA_DIR/l2chaindata" "$DATA_DIR/arbitrumdata" "$DATA_DIR/classic-msg" "$DATA_DIR/triecache" "$DATA_DIR/tmp" "$DATA_DIR/LOCK" 2>/dev/null
mkdir -p "$DATA_DIR"

tar -xf "$TAR_PATH" -C "$DATA_DIR"

echo ""
echo "Extraction complete:"
ls "$DATA_DIR"
du -sh "$DATA_DIR"

echo ""
echo "=== Step 3: Cleanup tar ==="
rm -f "$TAR_PATH"
df -h / | tail -1

echo ""
echo "=== Step 4: Start node ==="
cd /home/ubuntu/arbitrum_bot
sudo docker compose up -d arb-node
sleep 15
sudo docker logs --tail 10 arb-node 2>&1

echo ""
echo "=== DONE ==="
