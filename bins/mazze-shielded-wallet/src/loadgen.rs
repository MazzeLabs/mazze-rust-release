// Copyright 2024 Mazze Foundation. All rights reserved.
// Mazze is free software and distributed under GNU General Public License.
// See http://www.gnu.org/licenses/

//! A load generator for native and eSpace transfers (fleet capacity tests).
//!
//! Accounts are derived from a seed, `keccak(seed ‖ space ‖ i)`, so every
//! run of `fund`, `run` and `keys` with the same seed sees the same accounts.
//!
//! ```text
//! mazze-loadgen fund --rpc URL --funder-key HEX --space native|espace --seed S
//!                    --accounts N --amount A          (A in mazzy, per account)
//! mazze-loadgen run  --rpcs URL,URL,.. [--espace-rpcs URL,..] --space native|espace
//!                    --seed S --accounts N --tps T --duration SECS
//! mazze-loadgen keys --space S --seed S --accounts N   (secret per line)
//! ```
//! `run` sends transfers between the accounts at `--tps`, in JSON-RPC batches
//! spread over the nodes, and every 10 s reports what was accepted and what
//! actually executed: the executed nonces of a sample of accounts, scaled up.

#[path = "rpc.rs"]
#[allow(dead_code)]
mod rpc;

use std::{
    collections::HashMap,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, Mutex,
    },
    time::{Duration, Instant},
};

use jsonrpsee::{
    core::{client::ClientT, params::BatchRequestBuilder},
    http_client::{HttpClient, HttpClientBuilder},
    rpc_params,
};
use mazze_types::{Address, U256};
use primitives::{
    transaction::{eth_transaction::Eip155Transaction, native_transaction::NativeTransaction},
    Action, Transaction,
};
use rpc::{as_u64, hex0x, Node};
use serde_json::{json, Value};

/// The cross-space bridge, `0x0888…06`.
const CROSS_SPACE_ADDRESS: [u8; 20] = [
    0x08, 0x88, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x06,
];
/// Transactions per JSON-RPC batch.
const BATCH: usize = 200;
/// Accounts whose executed nonce is sampled for the executed rate.
const SAMPLE: usize = 64;

struct Args(HashMap<String, String>);

impl Args {
    fn parse(args: &[String]) -> Self {
        let mut map = HashMap::new();
        let mut it = args.iter().peekable();
        while let Some(a) = it.next() {
            if let Some(k) = a.strip_prefix("--") {
                let v = match it.peek() {
                    Some(v) if !v.starts_with("--") => it.next().unwrap().clone(),
                    _ => String::new(),
                };
                map.insert(k.to_string(), v);
            }
        }
        Args(map)
    }
    fn get(&self, k: &str) -> Result<&str, String> {
        self.0.get(k).map(|s| s.as_str()).ok_or_else(|| format!("missing --{k}"))
    }
    fn num(&self, k: &str) -> Result<u64, String> {
        self.get(k)?.parse().map_err(|e| format!("--{k}: {e}"))
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Space {
    Native,
    Espace,
}

struct Account {
    secret: mazzekey::Secret,
    /// The address transfers are sent from: native (type bit) or eSpace.
    address: Address,
}

fn account(seed: &str, space: Space, i: u64) -> Account {
    let tag = match space {
        Space::Native => "native",
        Space::Espace => "espace",
    };
    let mut ctr = 0u32;
    loop {
        let h = keccak_hash::keccak(format!("mazze-loadgen/{seed}/{tag}/{i}/{ctr}").as_bytes());
        if let Some(secret) = mazzekey::Secret::from_slice(h.as_bytes()) {
            if let Ok(pair) = mazzekey::KeyPair::from_secret(secret.clone()) {
                let address = match space {
                    Space::Native => pair.address(),
                    Space::Espace => pair.evm_address(),
                };
                return Account { secret, address };
            }
        }
        ctr += 1;
    }
}

fn space_of(a: &Args) -> Result<Space, String> {
    match a.get("space")? {
        "native" => Ok(Space::Native),
        "espace" => Ok(Space::Espace),
        s => Err(format!("--space {s}: native or espace")),
    }
}

fn base32(node: &Node, address: &Address) -> Result<String, String> {
    // The network never changes during a run: ask once.
    static NETWORK: std::sync::OnceLock<mazze_addr::Network> = std::sync::OnceLock::new();
    let network = match NETWORK.get() {
        Some(n) => *n,
        None => {
            let n = match as_u64(&node.call("mazze_getStatus", vec![])?["networkId"])? {
                1 => mazze_addr::Network::Test,
                1990 => mazze_addr::Network::Main,
                n => mazze_addr::Network::Id(n),
            };
            *NETWORK.get_or_init(|| n)
        }
    };
    mazze_addr::mazze_addr_encode(address.as_bytes(), network, mazze_addr::EncodingOptions::Simple)
        .map_err(|e| format!("{e:?}"))
}

/// What a node needs to sign against right now.
#[derive(Clone)]
struct Chain {
    chain_id: u32,
    gas_price: U256,
    epoch: u64,
}

/// The price paid is the latest block's base price plus a quarter, never
/// the node's gasPrice quote: that quote follows recent transactions'
/// prices, so paying a multiple of it under our own load fed back into
/// itself (2× every refresh, 1e9 → 2e12 in minutes, draining the accounts).
fn chain(node: &Node, space: Space) -> Result<Chain, String> {
    let base = |block: Value| -> Result<U256, String> {
        Ok(U256::from(as_u64(&block["baseFeePerGas"])?) * 5 / 4)
    };
    Ok(match space {
        Space::Native => {
            let status = node.call("mazze_getStatus", vec![])?;
            Chain {
                chain_id: as_u64(&status["chainId"])? as u32,
                gas_price: base(node.call("mazze_getBlockByEpochNumber", vec![json!("latest_state"), json!(false)])?)?,
                epoch: as_u64(&node.call("mazze_epochNumber", vec![json!("latest_state")])?)?,
            }
        }
        Space::Espace => Chain {
            chain_id: as_u64(&node.call("eth_chainId", vec![])?)? as u32,
            gas_price: base(node.call("eth_getBlockByNumber", vec![json!("latest"), json!(false)])?)?,
            epoch: 0,
        },
    })
}

fn sign(
    space: Space, c: &Chain, from: &Account, nonce: u64, to: Address, value: U256, gas: u64,
    data: Vec<u8>,
) -> Value {
    let tx: Transaction = match space {
        Space::Native => NativeTransaction {
            nonce: nonce.into(),
            gas_price: c.gas_price,
            gas: gas.into(),
            action: Action::Call(to),
            value,
            storage_limit: 0,
            epoch_height: c.epoch,
            chain_id: c.chain_id,
            data,
        }
        .into(),
        Space::Espace => Eip155Transaction {
            nonce: nonce.into(),
            gas_price: c.gas_price,
            gas: gas.into(),
            action: Action::Call(to),
            value,
            chain_id: Some(c.chain_id),
            data,
        }
        .into(),
    };
    hex0x(&rlp::encode(&tx.sign(&from.secret).transaction))
}

fn send_method(space: Space) -> &'static str {
    match space {
        Space::Native => "mazze_sendRawTransaction",
        Space::Espace => "eth_sendRawTransaction",
    }
}

/// The nonce an account has executed (`executed`) or will use next.
fn nonce_of(node: &Node, space: Space, address: &Address, executed: bool) -> Result<u64, String> {
    match space {
        // Executed: the state's nonce. Next: the pool's, counting the
        // transactions it already holds from this sender.
        Space::Native if executed => as_u64(
            &node.call("mazze_getNextNonce", vec![json!(base32(node, address)?), json!("latest_state")])?,
        ),
        Space::Native => as_u64(&node.call("txpool_nextNonce", vec![json!(base32(node, address)?)])?),
        Space::Espace => {
            let tag = if executed { "latest" } else { "pending" };
            as_u64(&node.call("eth_getTransactionCount", vec![json!(format!("{address:?}")), json!(tag)])?)
        }
    }
}

struct Batcher {
    client: HttpClient,
    runtime: tokio::runtime::Runtime,
}

impl Batcher {
    fn new(url: &str) -> Result<Self, String> {
        Ok(Batcher {
            client: HttpClientBuilder::default()
                .max_request_size(32 * 1024 * 1024)
                .max_response_size(32 * 1024 * 1024)
                .request_timeout(Duration::from_secs(60))
                .build(url)
                .map_err(|e| e.to_string())?,
            runtime: tokio::runtime::Builder::new_current_thread().enable_all().build().map_err(|e| e.to_string())?,
        })
    }

    /// Sends raw transactions in one batch; the error message of each one
    /// that was refused.
    fn send(&self, method: &str, raws: Vec<Value>) -> Result<Vec<Option<String>>, String> {
        let mut batch = BatchRequestBuilder::new();
        for raw in raws {
            batch.insert(method, rpc_params![raw]).map_err(|e| e.to_string())?;
        }
        let response = self
            .runtime
            .block_on(self.client.batch_request::<Value>(batch))
            .map_err(|e| e.to_string())?;
        Ok(response
            .into_iter()
            .map(|r| {
                r.err().map(|e| match e.data() {
                    Some(data) => format!("{}: {}", e.message(), data.get()),
                    None => e.message().to_string(),
                })
            })
            .collect())
    }
}

/// An error with its hashes replaced, so equal reasons count together.
fn reason(e: &str) -> String {
    let mut out = String::new();
    let mut chars = e.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '0' && chars.peek() == Some(&'x') {
            chars.next();
            while chars.peek().map_or(false, |c| c.is_ascii_hexdigit()) {
                chars.next();
            }
            out.push('H');
        } else {
            out.push(c);
        }
    }
    out.chars().take(160).collect()
}

fn fund(a: &Args) -> Result<(), String> {
    let space = space_of(a)?;
    let node = Node::connect(a.get("rpc")?)?;
    let funder_secret = mazzekey::Secret::from_slice(
        &hex::decode(a.get("funder-key")?.trim_start_matches("0x")).map_err(|e| e.to_string())?,
    )
    .ok_or("--funder-key must be a 32-byte secret")?;
    let funder_pair = mazzekey::KeyPair::from_secret(funder_secret.clone()).map_err(|e| format!("{e:?}"))?;
    let funder = Account { secret: funder_secret, address: funder_pair.address() };
    let (seed, n, amount) = (a.get("seed")?, a.num("accounts")?, U256::from(a.num("amount")?));
    let c = chain(&node, Space::Native)?;
    let mut nonce = nonce_of(&node, Space::Native, &funder.address, false)?;
    let batcher = Batcher::new(a.get("rpc")?)?;
    let mut raws = Vec::new();
    for i in 0..n {
        let to = account(seed, space, i).address;
        // eSpace accounts are funded through the cross-space bridge.
        let (dest, data, gas) = match space {
            Space::Native => (to, Vec::new(), 21_000),
            Space::Espace => {
                let mut d = keccak_hash::keccak(b"transferEVM(bytes20)").0[..4].to_vec();
                d.extend_from_slice(to.as_bytes());
                d.extend_from_slice(&[0u8; 12]);
                (Address::from(CROSS_SPACE_ADDRESS), d, 100_000)
            }
        };
        raws.push(sign(Space::Native, &c, &funder, nonce, dest, amount, gas, data));
        nonce += 1;
    }
    let mut refused = 0;
    for chunk in raws.chunks(BATCH) {
        for err in batcher.send("mazze_sendRawTransaction", chunk.to_vec())?.into_iter().flatten() {
            refused += 1;
            if refused <= 3 {
                eprintln!("refused: {err}");
            }
        }
    }
    println!("sent {} funding transactions ({refused} refused); waiting for execution", n);
    let target = nonce;
    for _ in 0..600 {
        if nonce_of(&node, Space::Native, &funder.address, true)? >= target {
            println!("funded {n} {} accounts", a.get("space")?);
            return Ok(());
        }
        std::thread::sleep(Duration::from_secs(1));
    }
    Err("funding was not executed within 10 minutes".into())
}

fn run(a: &Args) -> Result<(), String> {
    let space = space_of(a)?;
    let urls: Vec<String> = match space {
        Space::Native => a.get("rpcs")?,
        Space::Espace => a.get("espace-rpcs")?,
    }
    .split(',')
    .map(String::from)
    .collect();
    let (seed, n, tps, duration) = (a.get("seed")?, a.num("accounts")?, a.num("tps")?, a.num("duration")?);
    let accounts: Arc<Vec<Account>> = Arc::new((0..n).map(|i| account(seed, space, i)).collect());
    let probe = Node::connect(&urls[0])?;
    let state = Arc::new(Mutex::new(chain(&probe, space)?));
    // The starting nonces, fetched by 32 threads.
    let mut nonces = vec![0u64; n as usize];
    std::thread::scope(|scope| -> Result<(), String> {
        let chunks: Vec<_> = nonces.chunks_mut((n as usize).div_ceil(32).max(1)).enumerate().collect();
        let width = (n as usize).div_ceil(32).max(1);
        let jobs: Vec<_> = chunks
            .into_iter()
            .map(|(k, chunk)| {
                let (accounts, url) = (&accounts, &urls[k % urls.len()]);
                scope.spawn(move || -> Result<(), String> {
                    let node = Node::connect(url)?;
                    for (j, slot) in chunk.iter_mut().enumerate() {
                        // From the executed nonce: a gap an earlier run left is
                        // filled first.
                        *slot = nonce_of(&node, space, &accounts[k * width + j].address, true)?;
                    }
                    Ok(())
                })
            })
            .collect();
        for job in jobs {
            job.join().map_err(|_| "nonce thread panicked")??;
        }
        Ok(())
    })?;
    let nonces = Arc::new(Mutex::new(nonces));
    let (accepted, refused) = (Arc::new(AtomicU64::new(0)), Arc::new(AtomicU64::new(0)));
    let errors: Arc<Mutex<HashMap<String, u64>>> = Arc::new(Mutex::new(HashMap::new()));
    let resync: Arc<Mutex<Vec<usize>>> = Arc::new(Mutex::new(Vec::new()));
    let start = Instant::now();
    let deadline = start + Duration::from_secs(duration);

    // One sender thread per node; each sends its share of the rate.
    let mut handles = Vec::new();
    let per_node = (tps as f64 / urls.len() as f64).max(1.0);
    for (k, url) in urls.iter().enumerate() {
        let (accounts, state, nonces) = (accounts.clone(), state.clone(), nonces.clone());
        let (accepted, refused, errors, resync) = (accepted.clone(), refused.clone(), errors.clone(), resync.clone());
        let url = url.clone();
        let nodes = urls.len();
        handles.push(std::thread::spawn(move || {
            let batcher = match Batcher::new(&url) {
                Ok(b) => b,
                Err(e) => return eprintln!("{url}: {e}"),
            };
            let mut next = k; // this thread's accounts: k, k + nodes, ...
            let mut sent = 0f64;
            let t0 = Instant::now();
            while Instant::now() < deadline {
                let due = (t0.elapsed().as_secs_f64() * per_node - sent).floor() as usize;
                if due == 0 {
                    std::thread::sleep(Duration::from_millis(20));
                    continue;
                }
                let c = state.lock().unwrap().clone();
                let mut raws = Vec::new();
                let mut senders = Vec::new();
                {
                    let mut nonces = nonces.lock().unwrap();
                    for _ in 0..due.min(BATCH) {
                        let from = next % accounts.len();
                        let to = accounts[(from + 1) % accounts.len()].address;
                        raws.push(sign(space, &c, &accounts[from], nonces[from], to, U256::from(1u64), 21_000, Vec::new()));
                        nonces[from] += 1;
                        senders.push(from);
                        next += nodes;
                    }
                }
                sent += raws.len() as f64;
                match batcher.send(send_method(space), raws) {
                    Ok(results) => {
                        for (r, from) in results.into_iter().zip(senders) {
                            match r {
                                None => {
                                    accepted.fetch_add(1, Ordering::Relaxed);
                                }
                                // The pool already holds it: the gap-filling resend.
                                Some(e) if e.contains("already exist") => {
                                    accepted.fetch_add(1, Ordering::Relaxed);
                                }
                                Some(e) => {
                                    refused.fetch_add(1, Ordering::Relaxed);
                                    let key = reason(&e);
                                    *errors.lock().unwrap().entry(key).or_default() += 1;
                                    resync.lock().unwrap().push(from);
                                }
                            }
                        }
                    }
                    Err(e) => {
                        refused.fetch_add(senders.len() as u64, Ordering::Relaxed);
                        *errors.lock().unwrap().entry(format!("batch: {}", e.chars().take(70).collect::<String>())).or_default() += 1;
                        resync.lock().unwrap().extend(senders);
                    }
                }
            }
        }));
    }

    // Every 10 s: refresh the signing state, resync refused senders' nonces,
    // and report accepted vs executed.
    let sample: Vec<usize> = (0..n as usize).step_by((n as usize / SAMPLE).max(1)).take(SAMPLE).collect();
    let executed_sum = |node: &Node| -> u64 {
        sample.iter().map(|&i| nonce_of(node, space, &accounts[i].address, true).unwrap_or(0)).sum()
    };
    let mut last = (Instant::now(), executed_sum(&probe), 0u64);
    let base_executed = last.1;
    {
        let (state, url) = (state.clone(), urls[0].clone());
        std::thread::spawn(move || {
            let node = match Node::connect(&url) {
                Ok(n) => n,
                Err(_) => return,
            };
            while Instant::now() < deadline {
                std::thread::sleep(Duration::from_secs(5));
                if let Ok(c) = chain(&node, space) {
                    *state.lock().unwrap() = c;
                }
            }
        });
    }
    while Instant::now() < deadline + Duration::from_secs(5) {
        std::thread::sleep(Duration::from_secs(10));
        // A refused transaction leaves a nonce gap that strands every later
        // one from that sender: restart the sender from its executed nonce,
        // so the gap is sent again (duplicates are refused harmlessly).
        let stale: Vec<usize> = std::mem::take(&mut *resync.lock().unwrap());
        let mut uniq = stale;
        uniq.sort_unstable();
        uniq.dedup();
        for i in uniq {
            if let Ok(nonce) = nonce_of(&probe, space, &accounts[i].address, true) {
                nonces.lock().unwrap()[i] = nonce;
            }
        }
        let now = Instant::now();
        let ex = executed_sum(&probe);
        let acc = accepted.load(Ordering::Relaxed);
        let dt = now.duration_since(last.0).as_secs_f64();
        let scale = n as f64 / sample.len() as f64;
        let mut errs: Vec<(String, u64)> = errors.lock().unwrap().drain().collect();
        errs.sort_by(|x, y| y.1.cmp(&x.1));
        println!(
            "[{:>5}s] accepted {:>7.0}/s  executed ~{:>7.0}/s  (total accepted {acc}, refused {}, executed ~{:.0}){}",
            start.elapsed().as_secs(),
            (acc - last.2) as f64 / dt,
            (ex.saturating_sub(last.1)) as f64 * scale / dt,
            refused.load(Ordering::Relaxed),
            (ex - base_executed) as f64 * scale,
            errs.iter().take(2).map(|(e, k)| format!("  | {k}× {e}")).collect::<String>()
        );
        last = (now, ex, acc);
    }
    for h in handles {
        let _ = h.join();
    }
    Ok(())
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let a = Args::parse(&args[2.min(args.len())..]);
    let result = match args.get(1).map(String::as_str) {
        Some("fund") => fund(&a),
        Some("run") => run(&a),
        Some("keys") => space_of(&a).and_then(|space| {
            for i in 0..a.num("accounts")? {
                println!("{}", hex::encode(account(a.get("seed")?, space, i).secret.as_bytes()));
            }
            Ok(())
        }),
        _ => Err("usage: mazze-loadgen fund|run|keys (see the source header)".into()),
    };
    if let Err(e) = result {
        eprintln!("error: {e}");
        std::process::exit(1);
    }
}
