# Viewing Mazze Logs

## Zurich development phase
While the Mazze network is in the Zurich development phase, logs are stored in the `run/logs` directory by default with a higher verbosity level (DEBUG).
This is to ensure that the Mazze team can monitor the network and identify any issues that may arise.
This verbosity level will be reduced to INFO after the mainnet launch.

In the meantime, **you must be careful with the log size**, as it will grow quickly.

## Docker Installation
View logs in real-time using:

```bash
# Node logs
docker logs -f mazze-node
# Miner logs
docker logs -f mazze-miner
```

## Source Build Installation
Logs are stored in the `run/logs` directory by default.

```bash
# Node logs
tail -f logs/mazze-node.log
# Miner logs
tail -f logs/mazze-miner.log

# Remove `-f` flag if you don't want to follow the logs in real-time.
```