#!/bin/bash
set -e

# ═══════════════════════════════════════════════════════════
#  UPGRADE: t3.medium → t3.large + 300GB + Nitro Node
#  Run from LOCAL machine (not the server)
# ═══════════════════════════════════════════════════════════

REGION="us-east-1"
KEY_NAME="arb-bot-key"
NEW_TYPE="t3.large"
NEW_DISK_GB=300

echo "=== ARBITRUM BOT — AWS UPGRADE ==="
echo ""
echo "This script will:"
echo "  1. Stop the current instance"
echo "  2. Change type to $NEW_TYPE (8GB RAM)"
echo "  3. Expand disk to ${NEW_DISK_GB}GB"
echo "  4. Start instance"
echo "  5. Setup Nitro node"
echo ""

# Get current instance ID
INSTANCE_ID=$(aws ec2 describe-instances \
    --region $REGION \
    --filters "Name=tag:Name,Values=arb-bot" "Name=instance-state-name,Values=running" \
    --query 'Reservations[0].Instances[0].InstanceId' \
    --output text 2>/dev/null)

if [ "$INSTANCE_ID" == "None" ] || [ -z "$INSTANCE_ID" ]; then
    echo "No running instance found with tag 'arb-bot'"
    echo "Looking for any instance..."
    INSTANCE_ID=$(aws ec2 describe-instances \
        --region $REGION \
        --filters "Name=instance-state-name,Values=running,stopped" \
        --query 'Reservations[0].Instances[0].InstanceId' \
        --output text 2>/dev/null)
fi

if [ "$INSTANCE_ID" == "None" ] || [ -z "$INSTANCE_ID" ]; then
    echo "ERROR: No instance found. Enter instance ID manually:"
    read -p "Instance ID (i-xxx): " INSTANCE_ID
fi

echo "Instance: $INSTANCE_ID"

# Get current volume
VOLUME_ID=$(aws ec2 describe-instances \
    --region $REGION \
    --instance-ids $INSTANCE_ID \
    --query 'Reservations[0].Instances[0].BlockDeviceMappings[0].Ebs.VolumeId' \
    --output text)

echo "Volume: $VOLUME_ID"

# Get public IP
PUBLIC_IP=$(aws ec2 describe-instances \
    --region $REGION \
    --instance-ids $INSTANCE_ID \
    --query 'Reservations[0].Instances[0].PublicIpAddress' \
    --output text)

echo "IP: $PUBLIC_IP"
echo ""

read -p "Proceed with upgrade? (y/N) " CONFIRM
if [ "$CONFIRM" != "y" ]; then
    echo "Aborted"
    exit 0
fi

# ── Step 1: Stop instance ──
echo ""
echo ">>> Stopping instance..."
aws ec2 stop-instances --region $REGION --instance-ids $INSTANCE_ID
aws ec2 wait instance-stopped --region $REGION --instance-ids $INSTANCE_ID
echo "    Stopped"

# ── Step 2: Change instance type ──
echo ">>> Changing to $NEW_TYPE..."
aws ec2 modify-instance-attribute \
    --region $REGION \
    --instance-id $INSTANCE_ID \
    --instance-type "{\"Value\": \"$NEW_TYPE\"}"
echo "    Type changed to $NEW_TYPE"

# ── Step 3: Expand disk ──
CURRENT_SIZE=$(aws ec2 describe-volumes \
    --region $REGION \
    --volume-ids $VOLUME_ID \
    --query 'Volumes[0].Size' \
    --output text)

echo ">>> Current disk: ${CURRENT_SIZE}GB → ${NEW_DISK_GB}GB"

if [ "$CURRENT_SIZE" -lt "$NEW_DISK_GB" ]; then
    aws ec2 modify-volume \
        --region $REGION \
        --volume-id $VOLUME_ID \
        --size $NEW_DISK_GB \
        --volume-type gp3
    echo "    Disk expansion initiated"

    # Wait for volume modification
    echo "    Waiting for disk resize..."
    sleep 10
    while true; do
        STATE=$(aws ec2 describe-volumes-modifications \
            --region $REGION \
            --volume-ids $VOLUME_ID \
            --query 'VolumesModifications[0].ModificationState' \
            --output text 2>/dev/null)
        if [ "$STATE" == "completed" ] || [ "$STATE" == "optimizing" ]; then
            break
        fi
        echo "    State: $STATE"
        sleep 5
    done
    echo "    Disk resized"
else
    echo "    Disk already >= ${NEW_DISK_GB}GB"
fi

# ── Step 4: Start instance ──
echo ">>> Starting instance..."
aws ec2 start-instances --region $REGION --instance-ids $INSTANCE_ID
aws ec2 wait instance-running --region $REGION --instance-ids $INSTANCE_ID

# Get new public IP (might change)
NEW_IP=$(aws ec2 describe-instances \
    --region $REGION \
    --instance-ids $INSTANCE_ID \
    --query 'Reservations[0].Instances[0].PublicIpAddress' \
    --output text)

echo "    Running at $NEW_IP"
echo ""

# ── Step 5: Wait for SSH ──
echo ">>> Waiting for SSH..."
for i in $(seq 1 30); do
    if ssh -o StrictHostKeyChecking=no -o ConnectTimeout=5 -i ~/.ssh/${KEY_NAME}.pem ubuntu@$NEW_IP "echo ok" 2>/dev/null; then
        break
    fi
    sleep 5
done

# ── Step 6: Expand filesystem ──
echo ">>> Expanding filesystem..."
ssh -i ~/.ssh/${KEY_NAME}.pem ubuntu@$NEW_IP << 'REMOTE'
sudo growpart /dev/nvme0n1 1 2>/dev/null || sudo growpart /dev/xvda 1 2>/dev/null || true
sudo resize2fs /dev/nvme0n1p1 2>/dev/null || sudo resize2fs /dev/xvda1 2>/dev/null || true
df -h / | tail -1
REMOTE

echo ""
echo "=== UPGRADE COMPLETE ==="
echo ""
echo "Instance: $INSTANCE_ID ($NEW_TYPE)"
echo "IP: $NEW_IP"
echo "Disk: ${NEW_DISK_GB}GB"
echo ""
echo "Next: SSH in and run setup-nitro.sh"
echo "  ssh -i ~/.ssh/${KEY_NAME}.pem ubuntu@$NEW_IP"
echo "  cd arbitrum_bot && bash setup-nitro.sh"
