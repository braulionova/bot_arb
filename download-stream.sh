#!/bin/bash
set -eo pipefail

BASE_URL="https://snapshot.arbitrum.foundation/arb1/2026-03-28-ab33b7bf/pruned.tar"
DATA_DIR="/data/arbitrum/arb1/nitro"
PARTS=(part0000 part0001 part0002 part0003 part0004)

echo "=== Streaming multi-part snapshot (HTTP/1.1) ==="
echo "Parts: ${#PARTS[@]} | Target: $DATA_DIR"
df -h / | tail -1
echo ""

mkdir -p "$DATA_DIR"
rm -rf "$DATA_DIR/l2chaindata" "$DATA_DIR/arbitrumdata" "$DATA_DIR/classic-msg" "$DATA_DIR/triecache" "$DATA_DIR/tmp" "$DATA_DIR/LOCK" 2>/dev/null

# Stream all parts concatenated directly into tar
# curl --http1.1 avoids the HTTP/2 INTERNAL_ERROR
# --retry 999 with --retry-max-time 0 = retry forever on failure
# Each part retries independently

(
for PART in "${PARTS[@]}"; do
    echo "[$(date '+%H:%M:%S')] Streaming $PART ..." >&2
    curl -L \
        --http1.1 \
        --retry 999 \
        --retry-delay 10 \
        --retry-max-time 0 \
        --connect-timeout 60 \
        --keepalive-time 30 \
        --no-progress-meter \
        "${BASE_URL}.${PART}"
done
) | tar -xf - -C "$DATA_DIR" 2>&1

echo ""
echo "=== Extraction complete ==="
ls "$DATA_DIR"
du -sh "$DATA_DIR"
df -h / | tail -1

echo ""
echo "=== Starting node ==="
cd /home/ubuntu/arbitrum_bot
sudo docker compose up -d arb-node
sleep 15
sudo docker logs --tail 10 arb-node 2>&1

echo ""
echo "=== DONE ==="
