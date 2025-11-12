#!/bin/bash
# End-to-end test script for AiKv
# This script starts the AiKv server and runs basic commands against it

set -e

# Colors for output
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
NC='\033[0m' # No Color

# Configuration
AIKV_PORT=6380
AIKV_HOST=127.0.0.1
AIKV_BINARY="./target/debug/aikv"
TEST_TIMEOUT=30

echo -e "${YELLOW}AiKv End-to-End Test Script${NC}"
echo "=========================================="

# Function to print status
print_status() {
    if [ $1 -eq 0 ]; then
        echo -e "${GREEN}✓${NC} $2"
    else
        echo -e "${RED}✗${NC} $2"
        exit 1
    fi
}

# Build the project
echo -e "\n${YELLOW}Step 1: Building AiKv...${NC}"
cargo build
print_status $? "Build completed"

# Start the server in background
echo -e "\n${YELLOW}Step 2: Starting AiKv server...${NC}"
$AIKV_BINARY &
AIKV_PID=$!
echo "Server PID: $AIKV_PID"

# Wait for server to start
sleep 2

# Check if server is running
if ! kill -0 $AIKV_PID 2>/dev/null; then
    echo -e "${RED}✗${NC} Server failed to start"
    exit 1
fi
print_status 0 "Server started successfully"

# Cleanup function
cleanup() {
    echo -e "\n${YELLOW}Cleaning up...${NC}"
    if [ ! -z "$AIKV_PID" ]; then
        kill $AIKV_PID 2>/dev/null || true
        wait $AIKV_PID 2>/dev/null || true
        echo -e "${GREEN}✓${NC} Server stopped"
    fi
}

# Register cleanup on exit
trap cleanup EXIT INT TERM

# Run tests using redis-cli if available
echo -e "\n${YELLOW}Step 3: Running tests...${NC}"

if command -v redis-cli &> /dev/null; then
    echo "Using redis-cli for testing"
    
    # Test PING
    echo -e "\n${YELLOW}Testing PING command...${NC}"
    result=$(redis-cli -p $AIKV_PORT PING 2>/dev/null || echo "ERROR")
    if [ "$result" = "PONG" ]; then
        print_status 0 "PING command"
    else
        print_status 1 "PING command (got: $result)"
    fi
    
    # Test ECHO
    echo -e "\n${YELLOW}Testing ECHO command...${NC}"
    result=$(redis-cli -p $AIKV_PORT ECHO "Hello AiKv" 2>/dev/null || echo "ERROR")
    if [ "$result" = "Hello AiKv" ]; then
        print_status 0 "ECHO command"
    else
        print_status 1 "ECHO command (got: $result)"
    fi
    
    # Test SET/GET
    echo -e "\n${YELLOW}Testing SET/GET commands...${NC}"
    redis-cli -p $AIKV_PORT SET testkey "testvalue" >/dev/null 2>&1
    result=$(redis-cli -p $AIKV_PORT GET testkey 2>/dev/null || echo "ERROR")
    if [ "$result" = "testvalue" ]; then
        print_status 0 "SET/GET commands"
    else
        print_status 1 "SET/GET commands (got: $result)"
    fi
    
    # Test DEL
    echo -e "\n${YELLOW}Testing DEL command...${NC}"
    result=$(redis-cli -p $AIKV_PORT DEL testkey 2>/dev/null || echo "ERROR")
    if [ "$result" = "1" ] || [ "$result" = "(integer) 1" ]; then
        print_status 0 "DEL command"
    else
        print_status 1 "DEL command (got: $result)"
    fi
    
    # Test EXISTS
    echo -e "\n${YELLOW}Testing EXISTS command...${NC}"
    redis-cli -p $AIKV_PORT SET existskey "value" >/dev/null 2>&1
    result=$(redis-cli -p $AIKV_PORT EXISTS existskey 2>/dev/null || echo "ERROR")
    if [ "$result" = "1" ] || [ "$result" = "(integer) 1" ]; then
        print_status 0 "EXISTS command"
    else
        print_status 1 "EXISTS command (got: $result)"
    fi
    
else
    echo -e "${YELLOW}redis-cli not found, skipping protocol tests${NC}"
    echo "Install redis-cli to run full end-to-end tests"
    print_status 0 "Server startup test"
fi

echo -e "\n${GREEN}=========================================="
echo -e "All tests passed!${NC}"
echo -e "=========================================="
