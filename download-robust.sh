#!/bin/bash
set -eo pipefail

BASE_URL="https://snapshot.arbitrum.foundation/arb1/2026-03-28-ab33b7bf/pruned.tar"
DATA_DIR="/data/arbitrum/arb1/nitro"
PARTS="part0000 part0001 part0002 part0003 part0004"

# Robust curl that retries from the exact byte on failure
# Keeps stdout open and continuous for tar
robust_curl() {
    local url="$1"
    local total_size
    total_size=$(curl -sI --http1.1 "$url" | grep -i content-length | awk '{print $2}' | tr -d '\r')
    local offset=0

    while [ "$offset" -lt "$total_size" ]; do
        echo "[$(date '+%H:%M:%S')] Downloading $(basename $url) from byte $offset / $total_size ($(python3 -c "print(f'{$offset/$total_size*100:.1f}%')"))" >&2

        # Stream from offset, output to stdout
        curl -L --http1.1 \
            --range "${offset}-" \
            --connect-timeout 60 \
            --keepalive-time 30 \
            --no-progress-meter \
            --fail \
            "$url" 2>/dev/null && break

        # curl failed — figure out how many bytes were actually written
        # We can't know exactly, so we use a marker file approach
        echo "[$(date '+%H:%M:%S')] curl failed at offset ~$offset, retrying in 10s..." >&2
        sleep 10
        # The problem: we can't resume mid-pipe because tar already consumed bytes
        # This approach won't work for pipe. We need a different strategy.
        break
    done
}

echo "[$(date '+%H:%M:%S')] === Streaming multi-part snapshot ==="
echo "Target: $DATA_DIR"
df -h / | tail -1
echo ""

mkdir -p "$DATA_DIR"
rm -rf "$DATA_DIR"/* 2>/dev/null

# Use a named pipe (FIFO) approach with a download manager
# that writes to the FIFO and can restart on failure
FIFO="/tmp/snapshot-fifo"
rm -f "$FIFO"
mkfifo "$FIFO"

# Start tar reading from the FIFO in background
tar -xf "$FIFO" -C "$DATA_DIR" &
TAR_PID=$!

# Feed the FIFO with all parts sequentially
(
    for PART in $PARTS; do
        URL="${BASE_URL}.${PART}"
        echo "[$(date '+%H:%M:%S')] Streaming $PART ..." >&2

        MAX_RETRIES=50
        ATTEMPT=1
        while [ $ATTEMPT -le $MAX_RETRIES ]; do
            if curl -L --http1.1 \
                --connect-timeout 60 \
                --keepalive-time 15 \
                --speed-limit 1000000 \
                --speed-time 120 \
                --no-progress-meter \
                --fail \
                "$URL" 2>/dev/null; then
                echo "[$(date '+%H:%M:%S')] $PART complete!" >&2
                break
            fi

            echo "[$(date '+%H:%M:%S')] $PART attempt $ATTEMPT failed. Restarting part from scratch in 15s..." >&2
            ATTEMPT=$((ATTEMPT + 1))
            sleep 15

            # Kill tar and restart everything
            kill $TAR_PID 2>/dev/null || true
            wait $TAR_PID 2>/dev/null || true
            rm -rf "$DATA_DIR"/* 2>/dev/null
            rm -f "$FIFO"
            mkfifo "$FIFO"
            tar -xf "$FIFO" -C "$DATA_DIR" &
            TAR_PID=$!

            # Restart from part0000
            echo "[$(date '+%H:%M:%S')] Restarting from part0000..." >&2
            for PREV_PART in $PARTS; do
                [ "$PREV_PART" = "$PART" ] && break
                PREV_URL="${BASE_URL}.${PREV_PART}"
                echo "[$(date '+%H:%M:%S')] Re-streaming $PREV_PART ..." >&2
                curl -L --http1.1 \
                    --connect-timeout 60 \
                    --keepalive-time 15 \
                    --speed-limit 1000000 \
                    --speed-time 120 \
                    --no-progress-meter \
                    --fail \
                    "$PREV_URL" 2>/dev/null || {
                    echo "[$(date '+%H:%M:%S')] FATAL: failed re-streaming $PREV_PART" >&2
                    exit 1
                }
            done
        done
    done
) > "$FIFO"

# Wait for tar to finish
wait $TAR_PID
rm -f "$FIFO"

echo ""
echo "[$(date '+%H:%M:%S')] === Extraction complete ==="
ls "$DATA_DIR"
du -sh "$DATA_DIR"
df -h / | tail -1

echo ""
echo "[$(date '+%H:%M:%S')] === Starting node ==="
cd /home/ubuntu/arbitrum_bot
sudo docker compose up -d arb-node
sleep 15
sudo docker logs --tail 10 arb-node 2>&1
echo "=== DONE ==="
