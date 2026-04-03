#!/bin/bash
set -eo pipefail

SNAPSHOT_URL="https://snapshot.arbitrum.foundation/arb1/2026-03-28-ab33b7bf/pruned.tar.part0000"
DATA_DIR="/root/arb-node-data/arb1/nitro"

echo "=== Streaming Snapshot Download + Extract ==="
echo "Using curl with aggressive retry and keep-alive"
echo ""
df -h / | tail -1
echo ""

mkdir -p "$DATA_DIR"
rm -rf "$DATA_DIR/l2chaindata" "$DATA_DIR/arbitrumdata" "$DATA_DIR/classic-msg" "$DATA_DIR/triecache" 2>/dev/null

# Use wget instead of curl - more robust for large files
# --tries=0 = infinite retries
# -c = continue/resume (but won't work with pipe)
#
# Since streaming (pipe) doesn't support resume, we use a retry wrapper
# that restarts from scratch on failure. But we add TCP keepalive
# and larger buffers to prevent disconnects.

MAX_ATTEMPTS=10
ATTEMPT=1

while [ $ATTEMPT -le $MAX_ATTEMPTS ]; do
    echo "Attempt $ATTEMPT/$MAX_ATTEMPTS — starting download+extract..."
    echo ""

    # Clean previous partial extraction
    rm -rf "$DATA_DIR/l2chaindata" "$DATA_DIR/arbitrumdata" "$DATA_DIR/classic-msg" "$DATA_DIR/triecache" 2>/dev/null

    if curl -L \
        --http1.1 \
        --keepalive-time 30 \
        --connect-timeout 60 \
        --max-time 36000 \
        --no-progress-meter \
        "$SNAPSHOT_URL" 2>/tmp/curl_error.log | tar -xf - -C "$DATA_DIR" 2>/tmp/tar_error.log; then

        echo ""
        echo "=== SUCCESS ==="
        du -sh "$DATA_DIR"
        df -h / | tail -1
        echo ""
        echo "Arranca el nodo:"
        echo "  docker compose up -d arb-node"
        exit 0
    fi

    echo "Attempt $ATTEMPT failed:"
    cat /tmp/curl_error.log 2>/dev/null
    cat /tmp/tar_error.log 2>/dev/null
    echo ""
    echo "Waiting 30s before retry..."
    sleep 30
    ATTEMPT=$((ATTEMPT + 1))
done

echo "FAILED after $MAX_ATTEMPTS attempts"
exit 1
