#!/bin/bash

# Define constants
TAG=x86-64-v3
PUBLIC_IP_ADDRESS=37.27.134.141
MINING_AUTHOR=mazze:aakefvpch7kjbw9brxkby4apyzj7a1rv72a1k0dv2m
WORKER_ID=1
NUM_THREADS=4

# Create network if it doesn't exist
docker network create mazze-network || true

# Create logs directory if it doesn't exist
mkdir -p ./logs

## Start node
docker run -d \
    --name mazze-node \
    --network mazze-network \
    -p 32525:32525 \
    -p 55555:55555 \
    -p 52535:52535 \
    -p 52536:52536 \
    -p 52537:52537 \
    -p 58545:58545 \
    -p 58546:58546 \
    -v "$(pwd)/logs:/app/logs" \
    -e PUBLIC_ADDRESS="$PUBLIC_IP_ADDRESS" \
    -e MINING_AUTHOR="$MINING_AUTHOR" \
    0xnotadev/mazze-node:node-${TAG}

## Start miner
docker run -d \
    --name mazze-miner \
    --network mazze-network \
    -v "$(pwd)/logs:/app/logs" \
    -e MINING_AUTHOR="$MINING_AUTHOR" \
    -e WORKER_ID="$WORKER_ID" \
    -e NUM_THREADS="$NUM_THREADS" \
    0xnotadev/mazze-node:miner-${TAG}