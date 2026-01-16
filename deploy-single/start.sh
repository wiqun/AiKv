#!/bin/bash
# Start AiKv single node

echo "Starting AiKv..."
docker compose up -d

echo "Waiting for service to be ready..."
sleep 3

# Health check
if docker compose ps | grep -q "Up"; then
    echo "✅ AiKv is running!"
    echo "   Connect with: redis-cli -h 127.0.0.1 -p 6379"
else
    echo "❌ Failed to start AiKv"
    docker-compose logs
    exit 1
fi
