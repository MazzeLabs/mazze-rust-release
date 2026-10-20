// Copyright 2024 Mazze Foundation. All rights reserved.
// Mazze is free software and distributed under GNU General Public License.
// See http://www.gnu.org/licenses/

//! Raw TCP JSON-RPC endpoint (`jsonrpc_tcp_port` / `jsonrpc_local_tcp_port`).
//!
//! `jsonrpsee` has no raw socket transport, so this module serves the
//! jsonrpsee [`Methods`] over the wire protocol of the previous
//! `jsonrpc-tcp-server`:
//!
//! * requests are newline-delimited JSON (single calls or batches);
//! * every request produces exactly one line: the response, or an empty line
//!   for a notification (or a batch of notifications);
//! * requests of a connection are processed in order, one at a time;
//! * subscription notifications are pushed on the same connection, one JSON
//!   object per line, interleaved with responses;
//! * once the client closes its sending side, notifications that are already
//!   queued are flushed and the connection is closed.

use futures::StreamExt;
use jsonrpsee::{
    core::traits::IdProvider,
    prepare_error,
    types::{
        error::reject_too_many_subscriptions, ErrorCode, ErrorObject, Id,
        Notification, Params, Request,
    },
    BoundedSubscriptions, ConnectionId, MethodCallback, MethodResponse,
    MethodSink, Methods, SubscriptionState,
};
use runtime::Handle;
use serde_json::value::RawValue;
use std::{
    io,
    net::SocketAddr,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
};
use tokio::{
    io::AsyncWriteExt,
    net::{TcpListener, TcpStream},
    sync::{mpsc, watch, Semaphore},
};
use tokio_util::codec::{FramedRead, LinesCodec, LinesCodecError};

/// Limits of the TCP endpoint. The previous `jsonrpc-tcp-server` had no
/// explicit limits; the defaults below are at least as permissive as its
/// effective behaviour.
#[derive(Debug, Clone)]
pub struct TcpLimits {
    /// Longest accepted request line, in bytes. The old codec buffered lines
    /// without any bound; `u32::MAX` (4 GiB) is the same upper bound the
    /// HTTP and WebSocket servers can express.
    pub max_request_bytes: usize,
    /// Connections served concurrently. The old server drove at most 1024
    /// connections at once (`buffer_unordered(1024)`); further connections
    /// were accepted and waited, which is also what happens here.
    pub max_connections: usize,
    /// Subscriptions per connection. Unbounded before; `u32::MAX` here.
    pub max_subscriptions_per_connection: u32,
    /// Outgoing messages (responses and notifications) buffered per
    /// connection before a sender waits; the old push channel held 65536.
    pub message_buffer_capacity: usize,
}

impl Default for TcpLimits {
    fn default() -> Self {
        TcpLimits {
            max_request_bytes: u32::MAX as usize,
            max_connections: 1024,
            max_subscriptions_per_connection: u32::MAX,
            message_buffer_capacity: 65536,
        }
    }
}

/// Responses are not size limited on this endpoint (as before).
const MAX_RESPONSE_SIZE: usize = usize::MAX;

/// Connection ids of TCP connections; they only need to be unique among
/// the connections of one `Methods` instance.
static NEXT_CONN_ID: AtomicUsize = AtomicUsize::new(1 << 40);

/// Handle of a running TCP server; dropping it stops the server.
pub struct TcpServer {
    local_addr: SocketAddr,
    stop: watch::Sender<bool>,
}

impl TcpServer {
    /// The address the server listens on.
    pub fn local_addr(&self) -> SocketAddr { self.local_addr }
}

impl Drop for TcpServer {
    fn drop(&mut self) { let _ = self.stop.send(true); }
}

/// Starts serving `methods` on `addr` with the tasks running on `handle`.
pub fn start(
    addr: &SocketAddr, methods: Methods, id_provider: Arc<dyn IdProvider>,
    limits: TcpLimits, handle: &Handle,
) -> io::Result<TcpServer> {
    let std_listener = std::net::TcpListener::bind(addr)?;
    std_listener.set_nonblocking(true)?;
    let local_addr = std_listener.local_addr()?;
    let listener = {
        let _guard = handle.enter();
        TcpListener::from_std(std_listener)?
    };
    let (stop, stop_rx) = watch::channel(false);
    handle.spawn(accept_loop(listener, methods, id_provider, limits, stop_rx));
    Ok(TcpServer { local_addr, stop })
}

async fn accept_loop(
    listener: TcpListener, methods: Methods, id_provider: Arc<dyn IdProvider>,
    limits: TcpLimits, mut stop: watch::Receiver<bool>,
) {
    let connections = Arc::new(Semaphore::new(limits.max_connections));
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
        trace!(target: "tcp", "Accepted incoming connection from {}", peer_addr);
        let methods = methods.clone();
        let id_provider = id_provider.clone();
        let limits = limits.clone();
        let connections = connections.clone();
        let stop = stop.clone();
        tokio::spawn(async move {
            let _permit = match connections.acquire_owned().await {
                Ok(permit) => permit,
                Err(_) => return,
            };
            serve_connection(socket, methods, id_provider, limits, stop).await;
            trace!(target: "tcp", "Peer {}: service finished", peer_addr);
        });
    }
}

/// Per-connection state needed to dispatch calls.
struct Connection {
    methods: Methods,
    conn_id: ConnectionId,
    sink: MethodSink,
    subscriptions: BoundedSubscriptions,
    max_subscriptions: u32,
    id_provider: Arc<dyn IdProvider>,
}

async fn serve_connection(
    socket: TcpStream, methods: Methods, id_provider: Arc<dyn IdProvider>,
    limits: TcpLimits, mut stop: watch::Receiver<bool>,
) {
    let _ = socket.set_nodelay(true);
    let (read, mut write) = socket.into_split();

    // Subscription responses and notifications go through `sink`, plain
    // responses through `responses`; one writer task owns the socket.
    let (sink_tx, mut sink_rx) =
        mpsc::channel::<Box<RawValue>>(limits.message_buffer_capacity);
    let (resp_tx, mut resp_rx) =
        mpsc::channel::<String>(limits.message_buffer_capacity);
    let (reader_done_tx, mut reader_done) = watch::channel(false);

    let writer = tokio::spawn(async move {
        loop {
            let line = tokio::select! {
                biased;
                Some(msg) = sink_rx.recv() => String::from(Box::<str>::from(msg)),
                Some(resp) = resp_rx.recv() => resp,
                _ = reader_done.changed() => break,
            };
            if write_line(&mut write, &line).await.is_err() {
                return;
            }
        }
        // The client stopped sending: flush what is already queued.
        while let Ok(resp) = resp_rx.try_recv() {
            if write_line(&mut write, &resp).await.is_err() {
                return;
            }
        }
        while let Ok(msg) = sink_rx.try_recv() {
            let msg = String::from(Box::<str>::from(msg));
            if write_line(&mut write, &msg).await.is_err() {
                return;
            }
        }
        let _ = write.shutdown().await;
    });

    let conn = Connection {
        methods,
        conn_id: ConnectionId(NEXT_CONN_ID.fetch_add(1, Ordering::Relaxed)),
        sink: MethodSink::new_with_limit(sink_tx, u32::MAX),
        subscriptions: BoundedSubscriptions::new(
            limits.max_subscriptions_per_connection,
        ),
        max_subscriptions: limits.max_subscriptions_per_connection,
        id_provider,
    };

    let mut lines = FramedRead::new(
        read,
        LinesCodec::new_with_max_length(limits.max_request_bytes),
    );
    loop {
        let line = tokio::select! {
            _ = stop.changed() => break,
            line = lines.next() => line,
        };
        let line = match line {
            Some(Ok(line)) => line,
            Some(Err(LinesCodecError::MaxLineLengthExceeded)) => {
                debug!(target: "tcp", "Request exceeds the size limit");
                break;
            }
            Some(Err(LinesCodecError::Io(e))) => {
                debug!(target: "tcp", "Error reading request: {}", e);
                break;
            }
            None => break,
        };
        let line = match handle_request(&conn, &line).await {
            Outcome::Response(response) => response,
            Outcome::NoResponse => String::new(),
            // Already written through the subscription sink.
            Outcome::SentBySubscription => continue,
        };
        if resp_tx.send(line).await.is_err() {
            break;
        }
    }

    let _ = reader_done_tx.send(true);
    let _ = writer.await;
}

async fn write_line(
    write: &mut tokio::net::tcp::OwnedWriteHalf, line: &str,
) -> io::Result<()> {
    let mut buf = Vec::with_capacity(line.len() + 1);
    buf.extend_from_slice(line.as_bytes());
    buf.push(b'\n');
    write.write_all(&buf).await
}

fn error_json(id: Id<'_>, code: ErrorCode) -> String {
    String::from(Box::<str>::from(
        MethodResponse::error(id, ErrorObject::from(code)).into_json(),
    ))
}

/// What a request line produced.
enum Outcome {
    /// A response to write.
    Response(String),
    /// Nothing to answer (notifications): an empty line is written.
    NoResponse,
    /// The (un)subscription response was already sent through the sink.
    SentBySubscription,
}

/// Handles one request line.
async fn handle_request(conn: &Connection, text: &str) -> Outcome {
    let trimmed = text.trim_start();
    if trimmed.starts_with('[') {
        let batch: Vec<&RawValue> = match serde_json::from_str(trimmed) {
            Ok(batch) => batch,
            Err(_) => {
                return Outcome::Response(error_json(
                    Id::Null,
                    ErrorCode::ParseError,
                ))
            }
        };
        let mut responses = Vec::with_capacity(batch.len());
        let mut subscribed = false;
        for entry in batch {
            match handle_single(conn, entry.get()).await {
                Outcome::Response(rp) => responses.push(rp),
                Outcome::NoResponse => {}
                Outcome::SentBySubscription => subscribed = true,
            }
        }
        if !responses.is_empty() {
            Outcome::Response(format!("[{}]", responses.join(",")))
        } else if subscribed {
            Outcome::SentBySubscription
        } else {
            Outcome::NoResponse
        }
    } else if trimmed.starts_with('{') {
        handle_single(conn, trimmed).await
    } else {
        Outcome::Response(error_json(Id::Null, ErrorCode::ParseError))
    }
}

async fn handle_single(conn: &Connection, text: &str) -> Outcome {
    if let Ok(req) = serde_json::from_str::<Request>(text) {
        let rp = call(conn, req).await;
        // Subscription responses are sent by the subscription itself.
        if rp.is_subscription() {
            return Outcome::SentBySubscription;
        }
        let is_success = rp.is_success();
        let (json, notify, _) = rp.into_parts();
        let json = String::from(Box::<str>::from(json));
        if let Some(n) = notify {
            n.notify(is_success);
        }
        return Outcome::Response(json);
    }
    if serde_json::from_str::<Notification<Option<&RawValue>>>(text).is_ok() {
        return Outcome::NoResponse;
    }
    let (id, code) = prepare_error(text.as_bytes());
    Outcome::Response(error_json(id, code))
}

async fn call(conn: &Connection, req: Request<'_>) -> MethodResponse {
    let Request {
        id,
        method,
        params,
        extensions,
        ..
    } = req;
    let params = Params::new(params.as_ref().map(|p| p.get()));

    match conn.methods.method_with_name(&method) {
        None => {
            MethodResponse::error(id, ErrorObject::from(ErrorCode::MethodNotFound))
        }
        Some((_, MethodCallback::Sync(callback))) => {
            (callback)(id, params, MAX_RESPONSE_SIZE, extensions)
        }
        Some((_, MethodCallback::Async(callback))) => {
            (callback)(
                id.into_owned(),
                params.into_owned(),
                conn.conn_id,
                MAX_RESPONSE_SIZE,
                extensions,
            )
            .await
        }
        Some((_, MethodCallback::Subscription(callback))) => {
            match conn.subscriptions.acquire() {
                Some(permit) => {
                    let state = SubscriptionState {
                        conn_id: conn.conn_id,
                        id_provider: &*conn.id_provider,
                        subscription_permit: permit,
                    };
                    (callback)(id, params, conn.sink.clone(), state, extensions)
                        .await
                }
                None => MethodResponse::error(
                    id,
                    reject_too_many_subscriptions(conn.max_subscriptions),
                ),
            }
        }
        Some((_, MethodCallback::Unsubscription(callback))) => {
            (callback)(id, params, conn.conn_id, MAX_RESPONSE_SIZE, extensions)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rpc::helpers::HexIdProvider;
    use jsonrpsee::RpcModule;
    use std::io::{BufRead, BufReader, Write};

    #[test]
    fn serves_newline_delimited_requests() {
        let runtime = runtime::Runtime::with_thread_count(1);
        let mut module = RpcModule::new(());
        module
            .register_method("say_hello", |_, _, _| "hello")
            .unwrap();
        let server = start(
            &"127.0.0.1:0".parse().unwrap(),
            module.into(),
            Arc::new(HexIdProvider::default()),
            TcpLimits::default(),
            runtime.handle(),
        )
        .unwrap();
        let mut stream =
            std::net::TcpStream::connect(server.local_addr()).unwrap();
        let mut reader = BufReader::new(stream.try_clone().unwrap());
        let mut line = String::new();
        stream
            .write_all(b"{\"jsonrpc\":\"2.0\",\"method\":\"say_hello\",\"id\":1}\n")
            .unwrap();
        reader.read_line(&mut line).unwrap();
        assert_eq!(
            line,
            "{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":\"hello\"}\n"
        );
    }
}
