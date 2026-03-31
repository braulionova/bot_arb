#!/bin/bash
set -e

echo "=== Arbitrum Nitro Node Setup (Pruned Snapshot) ==="

# Config
DATA_DIR="/root/arb-node-data"
COMPOSE_DIR="/root/arbitrum_bot"

# 1. Verificar disco
echo ""
echo "--- Espacio en disco ---"
df -h / | tail -1
echo ""

AVAIL_GB=$(df --output=avail / | tail -1 | awk '{print int($1/1024/1024)}')
if [ "$AVAIL_GB" -lt 150 ]; then
    echo "ADVERTENCIA: Solo ${AVAIL_GB}GB disponibles. Se recomiendan 150GB+ para el nodo pruned."
    echo "Continuar? (y/N)"
    read -r ans
    [ "$ans" != "y" ] && exit 1
fi

# 2. Instalar Docker si no existe
if ! command -v docker &>/dev/null; then
    echo "--- Instalando Docker ---"
    curl -fsSL https://get.docker.com | sh
    systemctl enable docker
    systemctl start docker
fi

# 3. Crear directorio de datos
mkdir -p "$DATA_DIR"
echo "Datos del nodo en: $DATA_DIR"

# 4. Verificar env
if [ ! -f "$COMPOSE_DIR/node.env" ]; then
    echo "ERROR: Falta node.env con L1_RPC_URL y L1_BEACON_URL"
    echo "Crear $COMPOSE_DIR/node.env con:"
    echo "  L1_RPC_URL=https://..."
    echo "  L1_BEACON_URL=https://..."
    exit 1
fi

echo ""
echo "--- L1 endpoints ---"
grep -v '^#' "$COMPOSE_DIR/node.env" | grep -v '^$'
echo ""

# 5. Pull de la imagen
echo "--- Descargando imagen Nitro ---"
docker pull offchainlabs/nitro-node:v3.9.8-4624977

# 6. Arrancar nodo (--init.latest=pruned descarga el snapshot automaticamente)
echo ""
echo "--- Arrancando nodo ---"
echo "El primer arranque descargara el snapshot pruned (~80-120GB)."
echo "Esto puede tardar 1-3 horas dependiendo del ancho de banda."
echo ""
cd "$COMPOSE_DIR"
docker compose up -d arb-node

echo ""
echo "=== Nodo arrancado ==="
echo ""
echo "Monitorear:"
echo "  docker logs -f arb-node"
echo ""
echo "Verificar sync:"
echo "  curl -s http://127.0.0.1:8547 -X POST -H 'Content-Type: application/json' \\"
echo "    -d '{\"jsonrpc\":\"2.0\",\"method\":\"eth_syncing\",\"params\":[],\"id\":1}'"
echo ""
echo "Cuando eth_syncing devuelva 'false', el nodo esta listo."
echo "Despues ejecuta: ./start.sh para arrancar el bot."
