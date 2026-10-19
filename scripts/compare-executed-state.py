#!/usr/bin/env python3
"""Compare the executed state of main-chain epochs across nodes.

    scripts/compare-executed-state.py <ref_url> <url>... [--step N] [--from E]

Each URL is a node's *local* JSON-RPC endpoint (jsonrpc_local_http_port,
e.g. http://127.0.0.1:12539 through an ssh tunnel), which serves the test
RPC `getExecutedInfo`. For every N-th epoch from E to the reference node's
latest executed epoch, it takes the reference node's main block of that epoch
and compares (state_root, receipts_root) as executed by every other node.

A node whose state once diverged from the network's never recovers by
itself, blames the blocks of every other miner, and has its own blocks
blamed by everyone ("state/blame is invalid" on every node while it mines).
This finds such a node and the first epoch where it differs; it must then be
resynced. Use a healthy node as the reference (the one the other miners agree
with; `mazze_consensusDebug` reports `local_state_diverged_since`).
"""
import json
import sys
import urllib.request


def rpc(url, method, params):
    req = urllib.request.Request(
        url,
        data=json.dumps({'jsonrpc': '2.0', 'id': 1, 'method': method,
                         'params': params}).encode(),
        headers={'Content-Type': 'application/json'})
    with urllib.request.urlopen(req, timeout=10) as r:
        return json.loads(r.read())


def main(argv):
    step, start = 100, 1
    if '--step' in argv:
        i = argv.index('--step'); step = int(argv[i + 1]); del argv[i:i + 2]
    if '--from' in argv:
        i = argv.index('--from'); start = int(argv[i + 1]); del argv[i:i + 2]
    if len(argv) < 2:
        print(__doc__); return 1
    ref, others = argv[0], argv[1:]
    tip = int(rpc(ref, 'mazze_epochNumber', ['latest_state'])['result'], 16)
    stats = {u: {'same': 0, 'different': 0, 'unavailable': 0, 'first': None}
             for u in others}
    for e in range(start, tip + 1, step):
        blocks = rpc(ref, 'mazze_getBlocksByEpoch', [hex(e)]).get('result')
        if not blocks:
            continue
        mine = rpc(ref, 'getExecutedInfo', [blocks[-1]]).get('result')
        if mine is None:
            continue
        for u in others:
            theirs = rpc(u, 'getExecutedInfo', [blocks[-1]]).get('result')
            s = stats[u]
            if theirs is None:
                s['unavailable'] += 1
            elif theirs == mine:
                s['same'] += 1
            else:
                s['different'] += 1
                if s['first'] is None:
                    s['first'] = e
    print(f'reference {ref}: epochs {start}..{tip} step {step}')
    for u, s in stats.items():
        print(f"  {u}: same={s['same']} different={s['different']} "
              f"unavailable={s['unavailable']} "
              f"first_different_epoch={s['first']}")
    return 0


if __name__ == '__main__':
    sys.exit(main(sys.argv[1:]))
