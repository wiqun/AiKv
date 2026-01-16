# AiKv Single Node Deployment

This directory contains the deployment files for a single-node AiKv instance.

## Prerequisites

- Docker and Docker Compose installed
- AiKv Docker image built (docker build -t aikv:latest . from AiKv project root)

## Files

| File | Description |
|------|-------------|
| docker-compose.yml | Docker Compose configuration |
| aikv.toml | AiKv configuration file |
| start.sh | Start script |
| stop.sh | Stop script |

## Quick Start

Start AiKv: ./start.sh
Or manually: docker-compose up -d

## Connecting

Using redis-cli:
  redis-cli -h 127.0.0.1 -p 6379

Test connection:
  redis-cli PING

## Configuration

Edit aikv.toml to customize:

- Storage Engine: memory (fast) or aidb (persistent)
- Port: Default 6379
- Log Level: trace, debug, info, warn, error

## Monitoring

View logs: docker-compose logs -f
Check status: docker-compose ps

## Stopping

Stop: ./stop.sh
Or manually: docker-compose down
Remove data volumes: docker-compose down -v
