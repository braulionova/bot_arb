#!/bin/bash
set -e

cd /root/arbitrum_bot

echo "=== Arbitrum MEV Bot Startup ==="

# Build if needed
if [ ! -f target/release/arbitrum_bot ] || [ src/main.rs -nt target/release/arbitrum_bot ]; then
    echo "Building bot..."
    cargo build --release -p arbitrum_bot 2>&1 | tail -1
fi

if [ ! -f target/release/rpc-cache ] || [ rpc-cache/src/main.rs -nt target/release/rpc-cache ]; then
    echo "Building rpc-cache..."
    cargo build --release -p rpc-cache 2>&1 | tail -1
fi

# Load env
set -a; source .env; set +a

# Kill old instances
pkill -f "target/release/rpc-cache" 2>/dev/null && sleep 1 || true
pkill -f "target/release/arbitrum_bot" 2>/dev/null && sleep 1 || true

# Start rpc-cache proxy
echo "Starting rpc-cache proxy..."
nohup ./target/release/rpc-cache > /tmp/rpc_cache.log 2>&1 &
echo $! > /tmp/rpc_cache_pid.txt

# Wait for proxy to be ready
for i in $(seq 1 10); do
    if curl -s -X POST http://127.0.0.1:8547 -H "Content-Type: application/json" \
       -d '{"jsonrpc":"2.0","method":"eth_blockNumber","params":[],"id":1}' \
       --connect-timeout 2 2>/dev/null | grep -q result; then
        echo "rpc-cache: READY (PID: $(cat /tmp/rpc_cache_pid.txt))"
        break
    fi
    if [ "$i" -eq 10 ]; then
        echo "rpc-cache: FAILED TO START"
        cat /tmp/rpc_cache.log
        exit 1
    fi
    sleep 1
done

# Start bot
echo "Starting bot..."
nohup env RUST_LOG=arbitrum_bot=info ./target/release/arbitrum_bot > /tmp/arb_bot.log 2>&1 &
echo $! > /tmp/bot_pid.txt
echo "PID: $(cat /tmp/bot_pid.txt)"
echo ""
echo "Logs:"
echo "  tail -f /tmp/arb_bot.log          # bot"
echo "  tail -f /tmp/rpc_cache.log        # rpc-cache"
echo ""
echo "Stop:"
echo "  kill \$(cat /tmp/bot_pid.txt) \$(cat /tmp/rpc_cache_pid.txt)"
