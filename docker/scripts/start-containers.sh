#!/bin/bash

# Define constants
PUBLIC_IP_ADDRESS=37.27.134.141
MINING_AUTHOR=mazze:aaksp4xsre8r87vw38762fp41cgy6s723jwcssc8dc
WORKER_ID=1
NUM_THREADS=4

# Create logs directory if it doesn't exist
mkdir -p ./logs

## Start node
docker run -d \
    -p 55555:55555 \
    -p 52535:52535 \
    -p 52536:52536 \
    -p 52537:52537 \
    -p 58545:58545 \
    -p 58546:58546 \
    -v "$(pwd)/logs:/app/logs" \
    -e PUBLIC_ADDRESS="$PUBLIC_IP_ADDRESS" \
    -e MINING_AUTHOR="$MINING_AUTHOR" \
    --name mazze-node \
    mazze-node

## Start miner
docker run -d \
    -p 32525:32525 \
    -v "$(pwd)/logs:/app/logs" \
    -e MINING_AUTHOR="$MINING_AUTHOR" \
    -e WORKER_ID="$WORKER_ID" \
    -e NUM_THREADS="$NUM_THREADS" \
    --name mazze-miner \
    mazze-miner