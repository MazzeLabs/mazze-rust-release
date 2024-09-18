#!/bin/bash

./stop.sh

cd ..

git fetch
git pull

cargo build --release

cd run

./start-node.sh
./start-miner.sh