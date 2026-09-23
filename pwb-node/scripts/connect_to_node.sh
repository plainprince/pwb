#!/usr/bin/env bash
# Simple script to connect to a PlainWeb node via its connection link.
# Usage: ./connect_to_node.sh <node_ip> <node_port> <node_public_key_hex>
#
# This script generates a connection link for the given node and then connects to it.
#
# Example:
#   ./connect_to_node.sh 192.168.1.100 8080 abc123def4567890...

set -euo pipefail

if [ $# -ne 3 ]; then
    echo "Usage: $0 <node_ip> <node_port> <node_public_key_hex>"
    exit 1
fi

NODE_IP="$1"
NODE_PORT="$2"
NODE_PUBKEY_HEX="$3"

# Generate the connection link URI
LINK="plain://nodes.plain?connect=${NODE_IP}:${NODE_PORT}/${NODE_PUBKEY_HEX}"

echo "Connecting to node at ${NODE_IP}:${NODE_PORT}..."
echo "Connection link: ${LINK}"

# Try to establish a TCP connection to the node
if timeout 10 bash -c "echo > /dev/tcp/${NODE_IP}/${NODE_PORT}" 2>/dev/null; then
    echo "✓ Successfully connected to ${NODE_IP}:${NODE_PORT}"
else
    echo "✗ Failed to connect to ${NODE_IP}:${NODE_PORT}"
    exit 1
fi

echo "Connected successfully!"
