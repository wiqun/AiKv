#!/bin/bash
# Start AiKv cluster (6 nodes: 3 masters, 3 replicas)

echo "Starting AiKv cluster..."
docker-compose up -d

echo "Waiting for all nodes to be ready..."
sleep 10

# Check if all nodes are up
RUNNING_COUNT=$(docker-compose ps | grep -c "Up" || true)
if [ "$RUNNING_COUNT" -eq 6 ]; then
    echo "✅ All 6 nodes are running!"
else
    echo "⚠️  Some nodes may not be ready yet. Status:"
    docker-compose ps
fi

echo ""
echo "================================"
echo "Next Steps:"
echo "================================"
echo "1. Initialize the cluster with dynamic MetaRaft membership:"
echo "   ./init-cluster.sh"
echo ""
echo "2. After initialization, connect with:"
echo "   redis-cli -c -h 127.0.0.1 -p 6379"
echo ""
echo "3. Check cluster status:"
echo "   redis-cli -p 6379 CLUSTER INFO"
echo "   redis-cli -p 6379 CLUSTER NODES"
echo "   redis-cli -p 6379 CLUSTER METARAFT MEMBERS"
