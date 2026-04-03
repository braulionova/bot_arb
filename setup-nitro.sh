#!/bin/bash
set -e

# ═══════════════════════════════════════════════════════════
#  SETUP: Nitro Node + Bot + RPC Cache on t3.large
#  Run ON the server after upgrade
# ═══════════════════════════════════════════════════════════

echo "=== NITRO NODE + BOT SETUP ==="
echo ""

# Check disk
DISK_GB=$(df -BG / | tail -1 | awk '{print $2}' | tr -d 'G')
echo "Disk: ${DISK_GB}GB"
if [ "$DISK_GB" -lt 200 ]; then
    echo "ERROR: Need at least 200GB. Run upgrade-aws.sh first."
    exit 1
fi

# Check RAM
RAM_GB=$(free -g | awk '/^Mem:/{print $2}')
echo "RAM: ${RAM_GB}GB"

echo ""

# ── Step 1: Install Docker ──
if ! command -v docker &>/dev/null; then
    echo ">>> Installing Docker..."
    curl -fsSL https://get.docker.com | sh
    sudo usermod -aG docker ubuntu
    echo "    Docker installed"
else
    echo ">>> Docker already installed"
fi

# ── Step 2: Install Redis ──
if ! command -v redis-server &>/dev/null; then
    echo ">>> Installing Redis..."
    sudo apt-get install -y redis-server >/dev/null 2>&1
    sudo systemctl enable redis-server
    sudo systemctl start redis-server
    echo "    Redis installed"
else
    echo ">>> Redis already installed"
fi

# ── Step 3: Create data directory ──
sudo mkdir -p /root/arb-node-data
echo ">>> Data dir: /root/arb-node-data"

# ── Step 4: Create docker-compose for Nitro ──
cat > /home/ubuntu/arbitrum_bot/docker-compose.yml << 'COMPOSE'
services:
  nitro:
    image: offchainlabs/nitro-node:v3.9.8-4624977
    restart: unless-stopped
    ports:
      - "127.0.0.1:8547:8547"
      - "127.0.0.1:8548:8548"
    volumes:
      - /root/arb-node-data:/home/user/.arbitrum
    env_file:
      - node.env
    command:
      - --parent-chain.connection.url=${L1_RPC_URL}
      - --parent-chain.blob-client.beacon-url=${L1_BEACON_URL}
      - --chain.id=42161
      - --http.api=net,web3,eth
      - --http.corsdomain=*
      - --http.addr=0.0.0.0
      - --http.port=8547
      - --http.vhosts=*
      - --ws.addr=0.0.0.0
      - --ws.port=8548
      - --ws.origins=*
      - --ws.api=net,web3,eth
      - --node.feed.input.url=wss://arb1.arbitrum.io/feed
      - --execution.caching.archive=false
      - --init.prune=full
      - --init.prune-bloom-size=2048
      - --persistent.chain=/home/user/.arbitrum/arbitrum
      - --persistent.global-config=/home/user/.arbitrum
      - --execution.caching.database-cache=2048
      - --execution.caching.trie-dirty-cache=512
      - --execution.caching.trie-clean-cache=512
      - --execution.caching.snapshot-cache=0
      - --execution.caching.state-scheme=path
      - --execution.rpc.max-recreate-state-depth=0
      - --execution.caching.max-number-of-blocks-to-skip-state-computation=64
    deploy:
      resources:
        limits:
          memory: 6g
    logging:
      options:
        max-size: "10m"
        max-file: "2"
COMPOSE

echo ">>> docker-compose.yml created"

# ── Step 5: Create node.env if not exists ──
if [ ! -f /home/ubuntu/arbitrum_bot/node.env ]; then
    cat > /home/ubuntu/arbitrum_bot/node.env << 'NODEENV'
L1_RPC_URL=https://ethereum-rpc.publicnode.com
L1_BEACON_URL=https://ethereum-beacon-api.publicnode.com
NODEENV
    echo ">>> node.env created"
fi

# ── Step 6: Update .env to use local node ──
echo ">>> Updating .env for local node..."
if grep -q "RPC_UPSTREAMS" /home/ubuntu/arbitrum_bot/.env; then
    # Add local node as first upstream
    sed -i 's|RPC_UPSTREAMS=|RPC_UPSTREAMS=http://127.0.0.1:8547,|' /home/ubuntu/arbitrum_bot/.env
    echo "    Added local node to RPC_UPSTREAMS"
fi

# ── Step 7: Pull Nitro image ──
echo ">>> Pulling Nitro image (this takes a few minutes)..."
sudo docker compose pull 2>/dev/null || sudo docker-compose pull 2>/dev/null
echo "    Image pulled"

# ── Step 8: Start Nitro ──
echo ">>> Starting Nitro node..."
cd /home/ubuntu/arbitrum_bot
sudo docker compose up -d 2>/dev/null || sudo docker-compose up -d 2>/dev/null
echo "    Nitro starting (first sync takes 1-3 hours)"

# ── Step 9: Restart bot services ──
echo ">>> Restarting bot services..."
sudo systemctl restart arbitrum-rpc-cache 2>/dev/null || true
sleep 3
sudo systemctl restart arbitrum-bot 2>/dev/null || true

echo ""
echo "=== SETUP COMPLETE ==="
echo ""
echo "Nitro node syncing. Monitor with:"
echo "  sudo docker compose logs -f --tail 20"
echo ""
echo "Check sync progress:"
echo "  curl -s localhost:8547 -X POST -d '{\"jsonrpc\":\"2.0\",\"method\":\"eth_syncing\",\"params\":[],\"id\":1}' -H 'Content-Type: application/json'"
echo ""
echo "Once synced (false = synced):"
echo "  - RPC cache will use local node (0ms latency)"
echo "  - eth_getLogs will work"
echo "  - No more 429 rate limits"
echo "  - Bot sees state 50ms before public RPCs"
