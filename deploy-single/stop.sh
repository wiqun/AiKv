#!/bin/bash
# Stop AiKv single node

echo "Stopping AiKv..."
docker compose down -v

echo "✅ AiKv stopped"
