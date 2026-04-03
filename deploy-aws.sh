#!/bin/bash
set -e

# ═══════════════════════════════════════════════════════════════
# Deploy Arbitrum MEV Bot to AWS us-east-1
# Instance: t3.medium (2 vCPU, 4GB RAM, $30/month)
# Why: 20ms to Arbitrum sequencer vs 400ms from Contabo
# ═══════════════════════════════════════════════════════════════

echo "=== Arbitrum Bot AWS Deploy ==="
echo ""

# ─── CONFIG ───
AWS_REGION="us-east-1"
INSTANCE_TYPE="t3.medium"
AMI="ami-0866a3c8686eaeeba"  # Ubuntu 24.04 LTS us-east-1
KEY_NAME="arb-bot-key"
SECURITY_GROUP="arb-bot-sg"

echo "Region: $AWS_REGION"
echo "Instance: $INSTANCE_TYPE"
echo ""

# ─── STEP 1: Create key pair ───
echo "Step 1: Creating SSH key pair..."
aws ec2 create-key-pair \
    --region $AWS_REGION \
    --key-name $KEY_NAME \
    --query 'KeyMaterial' \
    --output text > ~/.ssh/${KEY_NAME}.pem 2>/dev/null || echo "Key exists"
chmod 400 ~/.ssh/${KEY_NAME}.pem

# ─── STEP 2: Create security group ───
echo "Step 2: Creating security group..."
SG_ID=$(aws ec2 create-security-group \
    --region $AWS_REGION \
    --group-name $SECURITY_GROUP \
    --description "Arbitrum MEV Bot" \
    --query 'GroupId' \
    --output text 2>/dev/null || \
    aws ec2 describe-security-groups \
        --region $AWS_REGION \
        --group-names $SECURITY_GROUP \
        --query 'SecurityGroups[0].GroupId' \
        --output text)

# Allow SSH
aws ec2 authorize-security-group-ingress \
    --region $AWS_REGION \
    --group-id $SG_ID \
    --protocol tcp --port 22 --cidr 0.0.0.0/0 2>/dev/null || true

echo "Security group: $SG_ID"

# ─── STEP 3: Launch instance ───
echo "Step 3: Launching instance..."
INSTANCE_ID=$(aws ec2 run-instances \
    --region $AWS_REGION \
    --image-id $AMI \
    --instance-type $INSTANCE_TYPE \
    --key-name $KEY_NAME \
    --security-group-ids $SG_ID \
    --block-device-mappings '[{"DeviceName":"/dev/sda1","Ebs":{"VolumeSize":30,"VolumeType":"gp3"}}]' \
    --tag-specifications "ResourceType=instance,Tags=[{Key=Name,Value=arb-bot}]" \
    --query 'Instances[0].InstanceId' \
    --output text)

echo "Instance: $INSTANCE_ID"
echo "Waiting for instance to start..."
aws ec2 wait instance-running --region $AWS_REGION --instance-ids $INSTANCE_ID

PUBLIC_IP=$(aws ec2 describe-instances \
    --region $AWS_REGION \
    --instance-ids $INSTANCE_ID \
    --query 'Reservations[0].Instances[0].PublicIpAddress' \
    --output text)

echo "Public IP: $PUBLIC_IP"
echo ""

# ─── STEP 4: Wait for SSH ───
echo "Step 4: Waiting for SSH..."
for i in $(seq 1 30); do
    if ssh -o StrictHostKeyChecking=no -o ConnectTimeout=5 -i ~/.ssh/${KEY_NAME}.pem ubuntu@$PUBLIC_IP "echo ok" 2>/dev/null; then
        break
    fi
    sleep 5
done

# ─── STEP 5: Setup server ───
echo "Step 5: Setting up server..."
ssh -i ~/.ssh/${KEY_NAME}.pem ubuntu@$PUBLIC_IP << 'SETUP'
sudo apt-get update -qq
sudo apt-get install -y -qq build-essential pkg-config libssl-dev redis-server curl
# Install Rust
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y
source ~/.cargo/env
# Install Foundry
curl -L https://foundry.paradigm.xyz | bash
source ~/.bashrc
~/.foundry/bin/foundryup
SETUP

# ─── STEP 6: Deploy bot code ───
echo "Step 6: Deploying bot..."
rsync -avz --exclude target --exclude '.git' --exclude 'logs/*.jsonl' \
    -e "ssh -i ~/.ssh/${KEY_NAME}.pem" \
    /root/arbitrum_bot/ ubuntu@$PUBLIC_IP:~/arbitrum_bot/

# ─── STEP 7: Build on AWS ───
echo "Step 7: Building on AWS..."
ssh -i ~/.ssh/${KEY_NAME}.pem ubuntu@$PUBLIC_IP << 'BUILD'
source ~/.cargo/env
cd ~/arbitrum_bot
cargo build --release -p arbitrum_bot -p rpc-cache 2>&1 | tail -3
BUILD

# ─── STEP 8: Start bot ───
echo "Step 8: Starting bot..."
ssh -i ~/.ssh/${KEY_NAME}.pem ubuntu@$PUBLIC_IP << 'START'
cd ~/arbitrum_bot
# Start Redis
sudo systemctl start redis-server

# Start rpc-cache
source .env 2>/dev/null || true
set -a && source .env && set +a
nohup ./target/release/rpc-cache > /tmp/rpc_cache.log 2>&1 &
sleep 3

# Start bot
nohup env RUST_LOG=arbitrum_bot=info,arbitrum_bot::executor=debug \
    ./target/release/arbitrum_bot > /tmp/arb_bot.log 2>&1 &
echo "Bot started!"
START

# ─── STEP 9: Verify latency ───
echo ""
echo "Step 9: Verifying latency..."
ssh -i ~/.ssh/${KEY_NAME}.pem ubuntu@$PUBLIC_IP << 'LATENCY'
curl -o /dev/null -s -w "Sequencer RTT: %{time_total}s\n" \
    -X POST https://arb1-sequencer.arbitrum.io/rpc \
    -H "Content-Type: application/json" \
    -d '{"jsonrpc":"2.0","method":"eth_sendRawTransaction","params":["0x"],"id":1}'
LATENCY

echo ""
echo "═══════════════════════════════════════════"
echo "  DEPLOY COMPLETE"
echo "  IP: $PUBLIC_IP"
echo "  SSH: ssh -i ~/.ssh/${KEY_NAME}.pem ubuntu@$PUBLIC_IP"
echo "  Logs: ssh ... 'tail -f /tmp/arb_bot.log'"
echo "═══════════════════════════════════════════"
