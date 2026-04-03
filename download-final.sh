#!/bin/bash
set -eo pipefail

BASE_URL="https://snapshot.arbitrum.foundation/arb1/2026-03-28-ab33b7bf/pruned.tar"
DATA_DIR="/data/arbitrum/arb1/nitro"
PARTS=(part0000 part0001 part0002 part0003 part0004)
DL_DIR="/data/arbitrum/parts"

mkdir -p "$DATA_DIR" "$DL_DIR"
rm -rf "$DATA_DIR/l2chaindata" "$DATA_DIR/arbitrumdata" "$DATA_DIR/classic-msg" "$DATA_DIR/triecache" "$DATA_DIR/tmp" "$DATA_DIR/LOCK" 2>/dev/null

echo "[$(date '+%H:%M:%S')] === Downloading all parts with resume ==="
df -h / | tail -1

# Step 1: Download all parts (wget -c resumes on failure)
for PART in "${PARTS[@]}"; do
    OUTFILE="$DL_DIR/${PART}"

    echo ""
    echo "[$(date '+%H:%M:%S')] Downloading $PART ..."
    wget -c --progress=dot:giga \
        --timeout=120 \
        --waitretry=30 \
        --tries=0 \
        --no-http-keep-alive \
        -O "$OUTFILE" \
        "${BASE_URL}.${PART}"

    echo "[$(date '+%H:%M:%S')] $PART done: $(ls -lh $OUTFILE | awk '{print $5}')"
done

echo ""
echo "[$(date '+%H:%M:%S')] All parts downloaded."
df -h / | tail -1

# Step 2: Extract by concatenating all parts into tar
echo ""
echo "[$(date '+%H:%M:%S')] === Extracting (streaming concat) ==="
cat "$DL_DIR"/part* | tar -xf - -C "$DATA_DIR"

echo ""
echo "[$(date '+%H:%M:%S')] Extraction complete!"
ls "$DATA_DIR"
du -sh "$DATA_DIR"

# Step 3: Cleanup parts
echo ""
echo "[$(date '+%H:%M:%S')] Removing downloaded parts..."
rm -rf "$DL_DIR"
df -h / | tail -1

# Step 4: Start node
echo ""
echo "[$(date '+%H:%M:%S')] === Starting node ==="
cd /home/ubuntu/arbitrum_bot
sudo docker compose up -d arb-node
sleep 15
sudo docker logs --tail 10 arb-node 2>&1
echo "=== DONE ==="
