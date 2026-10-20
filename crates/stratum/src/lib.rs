// Copyright 2024 Mazze Foundation. All rights reserved.
// Mazze is free software and distributed under GNU General Public License.
// See http://www.gnu.org/licenses/

// Copyright 2015-2019 Parity Technologies (UK) Ltd.
// This file is part of Parity Ethereum.

// Parity Ethereum is free software: you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.

// Parity Ethereum is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
// GNU General Public License for more details.

// You should have received a copy of the GNU General Public License
// along with Parity Ethereum.  If not, see <http://www.gnu.org/licenses/>.

//! Stratum protocol implementation for Mazze clients
//!
//! The wire protocol is the one of the previous `jsonrpc-tcp-server` based
//! implementation, served by a small tokio TCP server:
//!
//! * newline-delimited JSON-RPC requests, JSON-RPC 1.0 (no `"jsonrpc"`
//!   member, as sent by `mazze-miner`) or 2.0, single or batched;
//! * every request line is answered by one line (an empty line for
//!   notifications); responses mirror the request version;
//! * `mining.subscribe [worker_id, secret]` registers the connection as a
//!   worker (`true`), or answers `false` when a secret is configured and
//!   `keccak(secret)` does not match it;
//! * `mining.submit [worker, job_id, nonce, pow_hash, ..]` hands the string
//!   parameters to the job dispatcher and answers `[true]`,
//!   `[false, "reason"]` or `[false]`;
//! * jobs are pushed to every registered worker as
//!   `{ "id": N, "method": "mining.notify", "params": <payload> }` lines.

extern crate keccak_hash as hash;
extern crate mazze_types;
extern crate parking_lot;

#[macro_use]
extern crate log;

mod jsonrpc;
mod traits;

pub use traits::{
    Error, JobDispatcher, PushMessageError, PushWorkHandler,
    ServiceConfiguration,
};

use futures::StreamExt;
use jsonrpc::{Handler, Params};
use lazy_static::lazy_static;
use mazze_rpc_error::{to_value, Error as RpcError, Value};
use metrics::{Counter, CounterUsize};
use std::sync::Arc;

use crate::traits::Error::InvalidSolution;
use hash::keccak;
use mazze_types::H256;
use parking_lot::{Mutex, RwLock};
use std::{
    collections::{HashMap, HashSet},
    io,
    net::SocketAddr,
};
use tokio::{
    io::AsyncWriteExt,
    net::{TcpListener, TcpStream},
    sync::{mpsc, watch},
};
use tokio_util::codec::{FramedRead, LinesCodec};

type RpcResult = Result<Value, RpcError>;

const NOTIFY_COUNTER_INITIAL: u32 = 16;

/// Longest accepted request line. The previous server buffered lines
/// without a bound; `u32::MAX` keeps it at least as permissive.
const MAX_REQUEST_BYTES: usize = u32::MAX as usize;

/// Pushed messages buffered per worker (same as the previous push channel).
const PUSH_BUFFER_CAPACITY: usize = 65536;

lazy_static! {
    /// G-MN-1 — Per-outcome counters for `mining.submit` from stratum
    /// workers. Distinguish good shares from rejections so pool
    /// operators can dashboard share-validity rate and alert on
    /// miner misconfiguration (e.g. a spike in `invalid_solution` from
    /// a single worker is usually a wrong seed_hash). See
    /// docs/flow-audit.md G-MN-1.
    static ref STRATUM_SHARES_ACCEPTED: Arc<dyn Counter<usize>> =
        CounterUsize::register_with_group("stratum", "shares_accepted");
    static ref STRATUM_SHARES_REJECTED_INVALID: Arc<dyn Counter<usize>> =
        CounterUsize::register_with_group(
            "stratum",
            "shares_rejected.invalid_solution",
        );
    static ref STRATUM_SHARES_REJECTED_OTHER: Arc<dyn Counter<usize>> =
        CounterUsize::register_with_group(
            "stratum",
            "shares_rejected.other_error",
        );
    static ref STRATUM_SHARES_MALFORMED: Arc<dyn Counter<usize>> =
        CounterUsize::register_with_group("stratum", "shares_malformed");
}

/// Container which owns the tcp server and stratum implementation
pub struct Stratum {
    /// stratum protocol implementation
    ///
    /// It is owned by a container and rpc server
    implementation: Arc<StratumImpl>,
    /// Message dispatcher (tcp/ip service)
    ///
    /// Used to push messages to peers
    tcp_dispatcher: Dispatcher,
    /// Address the server listens on.
    local_addr: SocketAddr,
    /// Stops the accept loop and the connections.
    stop: watch::Sender<bool>,
    /// Runtime of the server tasks. It is an `Option` so it can be shut
    /// down during `drop`.
    runtime: Option<tokio::runtime::Runtime>,
}

impl Stratum {
    pub fn start(
        addr: &SocketAddr, dispatcher: Arc<dyn JobDispatcher>,
        secret: Option<H256>,
    ) -> Result<Arc<Stratum>, Error> {
        let implementation = Arc::new(StratumImpl {
            dispatcher,
            workers: Arc::new(RwLock::default()),
            secret,
            notify_counter: RwLock::new(NOTIFY_COUNTER_INITIAL),
        });

        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .thread_name("stratum")
            .enable_all()
            .build()?;

        let listener = std::net::TcpListener::bind(addr)?;
        listener.set_nonblocking(true)?;
        let local_addr = listener.local_addr()?;
        let listener = {
            let _guard = runtime.enter();
            TcpListener::from_std(listener)?
        };

        let tcp_dispatcher = Dispatcher::default();
        let (stop, stop_rx) = watch::channel(false);
        runtime.spawn(accept_loop(
            listener,
            implementation.clone(),
            tcp_dispatcher.clone(),
            stop_rx,
        ));

        Ok(Arc::new(Stratum {
            implementation,
            tcp_dispatcher,
            local_addr,
            stop,
            runtime: Some(runtime),
        }))
    }

    /// The address the stratum server listens on.
    pub fn local_addr(&self) -> SocketAddr { self.local_addr }
}

impl PushWorkHandler for Stratum {
    fn push_work_all(&self, payload: String) -> Result<(), Error> {
        debug!("Pushing job {} to miners", payload);

        self.implementation
            .push_work_all(payload, &self.tcp_dispatcher)
    }
}

impl Drop for Stratum {
    fn drop(&mut self) {
        // shut down the tcp server
        let _ = self.stop.send(true);
        if let Some(runtime) = self.runtime.take() {
            runtime.shutdown_background();
        }
    }
}

/// Peer-messages dispatcher: the push channel of every connected peer.
#[derive(Clone, Default)]
pub struct Dispatcher {
    channels: Arc<Mutex<HashMap<SocketAddr, mpsc::Sender<String>>>>,
}

impl Dispatcher {
    /// Pushes a message to the given peer.
    pub fn push_message(
        &self, peer_addr: &SocketAddr, msg: String,
    ) -> Result<(), PushMessageError> {
        let channels = self.channels.lock();
        match channels.get(peer_addr) {
            Some(channel) => channel.try_send(msg).map_err(|e| match e {
                mpsc::error::TrySendError::Closed(_) => {
                    PushMessageError::NoSuchPeer
                }
                mpsc::error::TrySendError::Full(_) => PushMessageError::Send(
                    "the peer's message queue is full".into(),
                ),
            }),
            None => Err(PushMessageError::NoSuchPeer),
        }
    }

    /// Returns `true` if the peer is still connected.
    pub fn is_connected(&self, socket_addr: &SocketAddr) -> bool {
        self.channels.lock().contains_key(socket_addr)
    }

    /// Returns current peer count.
    pub fn peer_count(&self) -> usize { self.channels.lock().len() }
}

async fn accept_loop(
    listener: TcpListener, implementation: Arc<StratumImpl>,
    dispatcher: Dispatcher, mut stop: watch::Receiver<bool>,
) {
    loop {
        let (socket, peer_addr) = tokio::select! {
            _ = stop.changed() => return,
            accepted = listener.accept() => match accepted {
                Ok(accepted) => accepted,
                Err(e) => {
                    warn!(target: "tcp", "Error accepting connection: {}", e);
                    continue;
                }
            },
        };
        trace!(target: "tcp", "Accepted incoming connection from {}", &peer_addr);
        tokio::spawn(serve_connection(
            socket,
            peer_addr,
            implementation.clone(),
            dispatcher.clone(),
            stop.clone(),
        ));
    }
}

async fn write_line(
    write: &mut tokio::net::tcp::OwnedWriteHalf, line: &str,
) -> io::Result<()> {
    let mut buf = Vec::with_capacity(line.len() + 1);
    buf.extend_from_slice(line.as_bytes());
    buf.push(b'\n');
    write.write_all(&buf).await
}

async fn serve_connection(
    socket: TcpStream, peer_addr: SocketAddr, implementation: Arc<StratumImpl>,
    dispatcher: Dispatcher, mut stop: watch::Receiver<bool>,
) {
    let (push_tx, mut push_rx) = mpsc::channel(PUSH_BUFFER_CAPACITY);
    dispatcher.channels.lock().insert(peer_addr, push_tx);

    let (read, mut write) = socket.into_split();
    let mut lines = FramedRead::new(
        read,
        LinesCodec::new_with_max_length(MAX_REQUEST_BYTES),
    );
    let session = Session {
        implementation: &implementation,
        addr: peer_addr,
    };

    let mut write_ok = true;
    loop {
        tokio::select! {
            // Responses first, then pushed messages (as before).
            biased;
            line = lines.next() => match line {
                Some(Ok(line)) => {
                    let response = jsonrpc::handle_request(&session, &line);
                    match &response {
                        Some(r) => trace!(target: "tcp", "Sent response: {}", r),
                        None => trace!(target: "tcp", "JSON RPC request produced no response"),
                    }
                    if write_line(&mut write, &response.unwrap_or_default())
                        .await
                        .is_err()
                    {
                        write_ok = false;
                        break;
                    }
                }
                Some(Err(e)) => {
                    warn!(target: "tcp", "Error while processing request: {:?}", e);
                    break;
                }
                None => break,
            },
            Some(msg) = push_rx.recv() => {
                if write_line(&mut write, &msg).await.is_err() {
                    write_ok = false;
                    break;
                }
            }
            _ = stop.changed() => break,
        }
    }

    // The peer stopped sending: deliver the messages already queued.
    if write_ok {
        while let Ok(msg) = push_rx.try_recv() {
            if write_line(&mut write, &msg).await.is_err() {
                break;
            }
        }
        let _ = write.shutdown().await;
    }
    dispatcher.channels.lock().remove(&peer_addr);
    trace!(target: "tcp", "Peer {}: service finished", peer_addr);
}

/// The stratum methods as seen from one connection.
struct Session<'a> {
    implementation: &'a StratumImpl,
    addr: SocketAddr,
}

impl Handler for Session<'_> {
    fn call(&self, method: &str, params: Params) -> Option<RpcResult> {
        match method {
            "mining.subscribe" => {
                Some(self.implementation.subscribe(params, &self.addr))
            }
            "mining.submit" => Some(self.implementation.submit(params)),
            _ => None,
        }
    }
}

struct StratumImpl {
    /// Payload manager
    dispatcher: Arc<dyn JobDispatcher>,
    /// Authorized workers (socket - worker_id)
    workers: Arc<RwLock<HashMap<SocketAddr, String>>>,
    /// Secret if any
    secret: Option<H256>,
    /// Dispatch notify couinter
    notify_counter: RwLock<u32>,
}

impl StratumImpl {
    /// rpc method `mining.subscribe`
    fn subscribe(&self, params: Params, addr: &SocketAddr) -> RpcResult {
        params.parse::<(String, String)>().map(|(worker_id, secret)|{
            if let Some(valid_secret) = self.secret {
                let hash = keccak(secret);
                if hash != valid_secret {
                    return to_value(&false);
                }
            }
            debug!(target: "stratum", "New worker #{} registered", worker_id);
            self.workers.write().insert(addr.clone(), worker_id);
            to_value(true)
        }).map(|v| v.expect("Only true/false is returned and it's always serializable; qed"))
    }

    /// rpc method `mining.submit`
    fn submit(&self, params: Params) -> RpcResult {
        Ok(Value::Array(match params {
            Params::Array(vals) => {
                // first two elements are service messages (worker_id & job_id)
                match self.dispatcher.submit(
                    vals.iter()
                        .filter_map(|val| match *val {
                            Value::String(ref s) => Some(s.to_owned()),
                            _ => None,
                        })
                        .collect::<Vec<String>>(),
                ) {
                    Ok(()) => {
                        STRATUM_SHARES_ACCEPTED.inc(1);
                        vec![to_value(true).expect("serializable")]
                    }
                    Err(InvalidSolution(msg)) => {
                        // When we have invalid solution, we propagate the
                        // reason to the client
                        STRATUM_SHARES_REJECTED_INVALID.inc(1);
                        warn!("Error because of invalid solution: {:?}", msg);
                        vec![
                            to_value(false).expect("serializable"),
                            to_value(msg).expect("serializable"),
                        ]
                    }
                    Err(submit_err) => {
                        STRATUM_SHARES_REJECTED_OTHER.inc(1);
                        warn!("Error while submitting share: {:?}", submit_err);
                        vec![to_value(false).expect("serializable")]
                    }
                }
            }
            _ => {
                STRATUM_SHARES_MALFORMED.inc(1);
                trace!(target: "stratum", "Invalid submit work format {:?}", params);
                vec![to_value(false).expect("serializable")]
            }
        }))
    }

    fn push_work_all(
        &self, payload: String, tcp_dispatcher: &Dispatcher,
    ) -> Result<(), Error> {
        let hup_peers = {
            let workers = self.workers.read();
            let next_request_id = {
                let mut counter = self.notify_counter.write();
                if *counter == ::std::u32::MAX {
                    *counter = NOTIFY_COUNTER_INITIAL;
                } else {
                    *counter += 1
                }
                *counter
            };

            let mut hup_peers = HashSet::with_capacity(0); // most of the cases won't be needed, hence avoid allocation
            let workers_msg = format!("{{ \"id\": {}, \"method\": \"mining.notify\", \"params\": {} }}", next_request_id, payload);
            trace!(target: "stratum", "Pushing work for {} workers (payload: '{}')", workers.len(), &workers_msg);
            for (ref addr, worker_id) in workers.iter() {
                trace!(target: "stratum", "Pushing work to {} at addr {}", &worker_id, &addr);
                match tcp_dispatcher.push_message(addr, workers_msg.clone()) {
                    Err(PushMessageError::NoSuchPeer) => {
                        debug!(target: "stratum", "Worker no longer connected: {} addr {}", &worker_id, &addr);
                        hup_peers.insert(**addr);
                    }
                    Err(e) => {
                        warn!(target: "stratum", "Unexpected transport error: {:?}", e);
                    }
                    Ok(_) => {}
                }
            }
            hup_peers
        };

        if !hup_peers.is_empty() {
            let mut workers = self.workers.write();
            for hup_peer in hup_peers {
                workers.remove(&hup_peer);
            }
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        io::{BufRead, BufReader, Read, Write},
        net::{Shutdown, SocketAddr, TcpStream},
        sync::Arc,
        thread, time,
    };

    pub struct VoidManager;

    impl JobDispatcher for VoidManager {
        fn submit(&self, _payload: Vec<String>) -> Result<(), Error> {
            Ok(())
        }
    }

    fn dummy_request(addr: &SocketAddr, data: &str) -> Vec<u8> {
        let mut stream =
            TcpStream::connect(addr).expect("connect should succeed");
        let mut data_vec = data.as_bytes().to_vec();
        data_vec.extend(b"\n");
        stream.write_all(&data_vec).unwrap();
        stream.shutdown(Shutdown::Write).unwrap();
        let mut read_buf = Vec::with_capacity(2048);
        stream.read_to_end(&mut read_buf).unwrap();
        read_buf
    }

    #[test]
    fn can_be_started() {
        let stratum = Stratum::start(
            &"127.0.0.1:19980".parse().unwrap(),
            Arc::new(VoidManager),
            None,
        );
        assert!(stratum.is_ok());
    }

    struct DummyManager {
        initial_payload: String,
    }

    impl DummyManager {
        fn build() -> DummyManager {
            DummyManager {
                initial_payload: r#"[ "dummy payload" ]"#.to_owned(),
            }
        }

        fn of_initial(mut self, new_initial: &str) -> DummyManager {
            self.initial_payload = new_initial.to_owned();
            self
        }
    }

    impl JobDispatcher for DummyManager {
        fn submit(&self, _payload: Vec<String>) -> Result<(), Error> {
            Ok(())
        }
    }

    fn terminated_str(origin: &'static str) -> String {
        let mut s = String::new();
        s.push_str(origin);
        s.push_str("\n");
        s
    }

    #[test]
    fn can_subscribe() {
        let addr = "127.0.0.1:19970".parse().unwrap();
        let stratum = Stratum::start(
            &addr,
            Arc::new(
                DummyManager::build()
                    .of_initial(r#"["dummy autorize payload"]"#),
            ),
            None,
        )
        .expect("There should be no error starting stratum");

        let request = r#"{"jsonrpc": "2.0", "method": "mining.subscribe", "params": ["miner1", ""], "id": 1}"#;
        let response =
            String::from_utf8(dummy_request(&addr, request)).unwrap();

        assert_eq!(
            terminated_str(r#"{"jsonrpc":"2.0","result":true,"id":1}"#),
            response
        );
        assert_eq!(1, stratum.implementation.workers.read().len());
    }

    #[test]
    fn can_push_work() {
        let _ = ::env_logger::try_init();

        let addr = "127.0.0.1:19995".parse().unwrap();
        let stratum = Stratum::start(
            &addr,
            Arc::new(
                DummyManager::build()
                    .of_initial(r#"["dummy autorize payload"]"#),
            ),
            None,
        )
        .expect("There should be no error starting stratum");

        let mut auth_request =
            r#"{"jsonrpc": "2.0", "method": "mining.subscribe", "params": ["miner1", ""], "id": 1}"#
            .as_bytes()
            .to_vec();
        auth_request.extend(b"\n");

        let auth_response = "{\"jsonrpc\":\"2.0\",\"result\":true,\"id\":1}\n";

        let mut stream = TcpStream::connect(&addr).unwrap();
        stream.write_all(&auth_request).unwrap();
        let mut read_buf0 = vec![0u8; auth_response.len()];
        stream.read_exact(&mut read_buf0).unwrap();
        assert_eq!(String::from_utf8(read_buf0).unwrap(), auth_response);
        trace!(target: "stratum", "Received authorization confirmation");
        thread::sleep(time::Duration::from_millis(100));

        trace!(target: "stratum", "Pusing work to peers");
        stratum
            .push_work_all(r#"{ "00040008", "100500" }"#.to_owned())
            .expect("Pushing work should produce no errors");
        thread::sleep(time::Duration::from_millis(100));

        trace!(target: "stratum", "Ready to read work from server");
        thread::sleep(time::Duration::from_millis(100));
        stream.shutdown(Shutdown::Write).unwrap();
        let mut read_buf1 = Vec::with_capacity(2048);
        stream.read_to_end(&mut read_buf1).unwrap();
        trace!(target: "stratum", "Received work from server");
        let response =
            String::from_utf8(read_buf1).expect("Response should be utf-8");

        assert_eq!(
            "{ \"id\": 17, \"method\": \"mining.notify\", \"params\": { \"00040008\", \"100500\" } }\n",
            response);
    }

    /// Records submitted shares; rejects the nonce "0xbad".
    #[derive(Default)]
    struct RecordingManager {
        submitted: Mutex<Vec<Vec<String>>>,
    }

    impl JobDispatcher for RecordingManager {
        fn submit(&self, payload: Vec<String>) -> Result<(), Error> {
            let reject = payload.get(2).map(String::as_str) == Some("0xbad");
            self.submitted.lock().push(payload);
            if reject {
                Err(Error::InvalidSolution("bad nonce".into()))
            } else {
                Ok(())
            }
        }
    }

    fn read_line(reader: &mut BufReader<TcpStream>) -> String {
        let mut line = String::new();
        reader.read_line(&mut line).unwrap();
        line
    }

    /// A client speaking the protocol exactly as `mazze-miner` and the
    /// previous `jsonrpc-tcp-server` based server did: JSON-RPC 1.0
    /// requests without a `"jsonrpc"` member, newline-delimited, with jobs
    /// pushed on the same connection.
    #[test]
    fn old_protocol_client_works() {
        let secret = "s3cret";
        let manager = Arc::new(RecordingManager::default());
        let stratum = Stratum::start(
            &"127.0.0.1:0".parse().unwrap(),
            manager.clone(),
            Some(keccak(secret)),
        )
        .unwrap();
        let addr = stratum.local_addr();

        // A wrong secret is answered with `false` and does not register.
        let mut stream = TcpStream::connect(addr).unwrap();
        stream
            .write_all(b"{\"id\":\"w1\",\"method\":\"mining.subscribe\",\"params\":[\"999\",\"wrong\"]}\n")
            .unwrap();
        let mut reader = BufReader::new(stream.try_clone().unwrap());
        assert_eq!(read_line(&mut reader), "{\"result\":false,\"id\":\"w1\"}\n");
        assert_eq!(stratum.implementation.workers.read().len(), 0);

        // The right secret registers the worker.
        stream
            .write_all(format!("{{\"id\":\"w1\",\"method\":\"mining.subscribe\",\"params\":[\"999\",\"{}\"]}}\n", secret).as_bytes())
            .unwrap();
        assert_eq!(read_line(&mut reader), "{\"result\":true,\"id\":\"w1\"}\n");
        assert_eq!(stratum.implementation.workers.read().len(), 1);

        // Jobs are pushed to the worker.
        stratum
            .push_work_all(r#"["0x01", "0x02", "0x03"]"#.to_owned())
            .unwrap();
        assert_eq!(
            read_line(&mut reader),
            "{ \"id\": 17, \"method\": \"mining.notify\", \"params\": [\"0x01\", \"0x02\", \"0x03\"] }\n"
        );

        // Shares are submitted as strings; non-string params are dropped.
        stream
            .write_all(b"{\"id\":\"7-10\",\"method\":\"mining.submit\",\"params\":[\"worker\",\"10\",\"0x7\",\"0xabc\",5]}\n")
            .unwrap();
        assert_eq!(read_line(&mut reader), "{\"result\":[true],\"id\":\"7-10\"}\n");
        stream
            .write_all(b"{\"id\":8,\"method\":\"mining.submit\",\"params\":[\"worker\",\"10\",\"0xbad\",\"0xabc\"]}\n")
            .unwrap();
        assert_eq!(read_line(&mut reader), "{\"result\":[false,\"bad nonce\"],\"id\":8}\n");
        stream
            .write_all(b"{\"jsonrpc\":\"2.0\",\"id\":9,\"method\":\"mining.submit\",\"params\":{\"a\":1}}\n")
            .unwrap();
        assert_eq!(read_line(&mut reader), "{\"jsonrpc\":\"2.0\",\"result\":[false],\"id\":9}\n");
        assert_eq!(
            *manager.submitted.lock(),
            vec![
                vec!["worker".to_string(), "10".into(), "0x7".into(), "0xabc".into()],
                vec!["worker".to_string(), "10".into(), "0xbad".into(), "0xabc".into()],
            ]
        );

        // Errors and notifications keep the old framing.
        stream.write_all(b"{\"id\":1,\"method\":\"mining.unknown\",\"params\":[]}\n").unwrap();
        assert_eq!(
            read_line(&mut reader),
            "{\"error\":{\"code\":-32601,\"message\":\"Method not found\"},\"id\":1}\n"
        );
        stream.write_all(b"{\"id\":2,\"method\":\"mining.subscribe\",\"params\":[]}\n").unwrap();
        assert_eq!(
            read_line(&mut reader),
            "{\"error\":{\"code\":-32602,\"message\":\"Invalid params: invalid length 0, expected a tuple of size 2.\"},\"id\":2}\n"
        );
        stream.write_all(b"not json\n").unwrap();
        assert_eq!(
            read_line(&mut reader),
            "{\"jsonrpc\":\"2.0\",\"error\":{\"code\":-32700,\"message\":\"Parse error\"},\"id\":null}\n"
        );
        stream.write_all(b"{\"method\":\"mining.submit\",\"params\":[]}\n").unwrap();
        assert_eq!(read_line(&mut reader), "\n");

        // A disconnected worker is dropped on the next push.
        stream.shutdown(Shutdown::Both).unwrap();
        drop(reader);
        thread::sleep(time::Duration::from_millis(200));
        stratum.push_work_all("[]".to_owned()).unwrap();
        assert_eq!(stratum.implementation.workers.read().len(), 0);
    }
}
