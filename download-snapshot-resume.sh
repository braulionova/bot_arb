#!/bin/bash
set -eo pipefail

SNAPSHOT_URL="https://snapshot.arbitrum.foundation/arb1/2026-03-28-ab33b7bf/pruned.tar.part0000"
DOWNLOAD_PATH="/data/snapshot-pruned.tar"
DATA_DIR="/data/arbitrum/arb1/nitro"

echo "=== Snapshot Download with Resume Support ==="
df -h / | tail -1
echo ""

# Step 1: Download with wget -c (resume capable)
echo "Downloading snapshot to $DOWNLOAD_PATH ..."
echo "If interrupted, re-run this script to resume."
echo ""

wget -c --progress=dot:giga \
    --timeout=60 \
    --waitretry=30 \
    --tries=0 \
    -O "$DOWNLOAD_PATH" \
    "$SNAPSHOT_URL"

echo ""
echo "Download complete. Size:"
ls -lh "$DOWNLOAD_PATH"
echo ""

# Step 2: Extract
echo "Extracting to $DATA_DIR ..."
rm -rf "$DATA_DIR/l2chaindata" "$DATA_DIR/arbitrumdata" "$DATA_DIR/classic-msg" "$DATA_DIR/triecache" 2>/dev/null
mkdir -p "$DATA_DIR"

tar -xf "$DOWNLOAD_PATH" -C "$DATA_DIR"

echo ""
echo "Extraction complete."
du -sh "$DATA_DIR"
echo ""

# Step 3: Cleanup
echo "Removing downloaded tar..."
rm -f "$DOWNLOAD_PATH"
df -h / | tail -1

echo ""
echo "=== DONE ==="
echo "Start the node with:"
echo "  cd /home/ubuntu/arbitrum_bot && docker compose up -d arb-node"
