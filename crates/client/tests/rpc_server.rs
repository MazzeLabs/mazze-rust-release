// Copyright 2024 Mazze Foundation. All rights reserved.
// Mazze is free software and distributed under GNU General Public License.
// See http://www.gnu.org/licenses/

//! End-to-end tests of the RPC servers (HTTP, WebSocket and raw TCP) with a
//! minimal API set: the core-space pub-sub and trace APIs plus
//! `rpc_methods` / `rpc_modules`, backed by a real (empty) consensus graph.
//!
//! The `OLD_*` constants are responses captured from the previous
//! `jsonrpc-core` 15 based servers of a dev node; responses are compared as
//! JSON values (member order is not significant on the wire).

use client::rpc::{
    add_meta_rpc_methods, extend_with_interceptor,
    impls::{pubsub::PubSubClient, trace::TraceHandler, RpcImplConfiguration},
    rpc_apis::Api,
    start_http, start_tcp, start_ws,
    traits::{pubsub::PubSubServer, trace::TraceServer},
    HttpConfiguration, HttpServer, RpcLimits, TcpConfiguration, TcpServer,
    WsConfiguration, WsServer,
};
use jsonrpsee::{
    core::client::{ClientT, SubscriptionClientT},
    rpc_params,
    ws_client::WsClientBuilder,
    RpcModule,
};
use mazze_addr::Network;
use mazze_types::H256;
use mazzecore::{
    sync::utils::initialize_synchronization_graph, ConsensusGraph,
    ConsensusGraphTrait,
    Notifications,
};
use runtime::Runtime;
use serde_json::{json, Value};
use std::{
    collections::{BTreeSet, HashMap},
    io::{BufRead, BufReader, Read, Write},
    net::{SocketAddr, TcpStream},
    sync::Arc,
    time::Duration,
};

const OLD_METHOD_NOT_FOUND: &str = r#"{"jsonrpc":"2.0","error":{"code":-32601,"message":"Method not found"},"id":1}"#;
const OLD_PARSE_ERROR: &str = r#"{"jsonrpc":"2.0","error":{"code":-32700,"message":"Parse error"},"id":null}"#;
const OLD_TRACE_BLOCK_UNKNOWN: &str = r#"{"jsonrpc":"2.0","result":null,"id":1}"#;
const OLD_SUBSCRIBE_OVER_HTTP: &str = r#"{"jsonrpc":"2.0","error":{"code":-32090,"message":"Subscriptions are not available on this transport."},"id":1}"#;
const OLD_HEADS_WITH_PARAMS: &str = r#"{"jsonrpc":"2.0","error":{"code":-32602,"message":"Invalid parameters: newHeads","data":"\"Expected no parameters.\""},"id":1}"#;
const OLD_UNSUBSCRIBE_UNKNOWN: &str = r#"{"jsonrpc":"2.0","result":false,"id":1}"#;
const OLD_TOO_LARGE_BODY: &str = "request body size exceeds allowed maximum";
const OLD_METHOD_NOT_ALLOWED: &str =
    "Used HTTP Method is not allowed. POST or OPTIONS is required\n";

const ZERO_HASH: &str =
    "0x0000000000000000000000000000000000000000000000000000000000000000";

struct Node {
    _dir: tempfile::TempDir,
    runtime: Runtime,
    consensus: Arc<ConsensusGraph>,
    notifications: Arc<Notifications>,
    genesis: H256,
    throttling_file: String,
}

impl Node {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let db = format!("{}/db/", dir.path().display());
        let (_sync, consensus, _data_man, genesis) =
            initialize_synchronization_graph(
                &db,
                1,
                1,
                1,
                1,
                50000,
                H256::default(),
            );
        // `trace_epoch` is limited to a single call: the second one is
        // throttled.
        let throttling_file = format!("{}/throttling.toml", dir.path().display());
        std::fs::write(&throttling_file, "[rpc]\ntrace_epoch=\"1,1,1,1,0\"\n")
            .unwrap();
        Node {
            _dir: dir,
            runtime: Runtime::with_thread_count(2),
            consensus,
            notifications: Notifications::init(),
            genesis: genesis.hash(),
            throttling_file,
        }
    }

    /// The pub-sub and trace APIs, both behind the throttling interceptor
    /// like on a full node, plus `rpc_methods` / `rpc_modules`.
    fn module(&self) -> RpcModule<()> {
        let config = RpcImplConfiguration {
            get_logs_filter_max_limit: None,
            dev_pack_tx_immediately: false,
            max_payload_bytes: 1024,
            enable_metrics: false,
            poll_lifetime_in_seconds: None,
        };
        let throttling = Some(self.throttling_file.clone());
        let pubsub = PubSubClient::new(
            self.runtime.executor(),
            self.consensus.clone(),
            self.notifications.clone(),
            Network::Main,
        );
        let trace = TraceHandler::new(
            self.consensus.get_data_manager().clone(),
            Network::Main,
            self.consensus.clone(),
        );
        let mut module = RpcModule::new(());
        extend_with_interceptor(
            &mut module,
            &config,
            PubSubServer::into_rpc(pubsub),
            &throttling,
            "rpc",
        );
        extend_with_interceptor(
            &mut module,
            &config,
            TraceServer::into_rpc(trace),
            &throttling,
            "rpc",
        );
        add_meta_rpc_methods(module, [Api::Pubsub, Api::Trace].into_iter().collect())
    }

    fn http(&self, cors: Option<&str>, keep_alive: bool) -> HttpServer {
        let mut conf = HttpConfiguration::new(
            Some((127, 0, 0, 1)),
            Some(0),
            cors.map(String::from),
            keep_alive,
            None,
        );
        conf.address.set_port(0);
        start_http(conf, self.module(), &self.runtime).unwrap().unwrap()
    }

    fn ws(&self, max_payload_bytes: usize) -> WsServer {
        let conf =
            WsConfiguration::new(Some((127, 0, 0, 1)), Some(0), max_payload_bytes);
        start_ws(conf, self.module(), &self.runtime).unwrap().unwrap()
    }

    fn tcp(&self) -> TcpServer {
        let conf = TcpConfiguration::new(Some((127, 0, 0, 1)), Some(0));
        start_tcp(conf, self.module(), &self.runtime).unwrap().unwrap()
    }

    fn http_with(&self, limits: RpcLimits) -> HttpServer {
        let mut conf = HttpConfiguration::new(
            Some((127, 0, 0, 1)),
            Some(0),
            None,
            false,
            None,
        )
        .with_limits(limits);
        conf.address.set_port(0);
        start_http(conf, self.module(), &self.runtime).unwrap().unwrap()
    }

    fn tcp_with(&self, limits: RpcLimits) -> TcpServer {
        let conf = TcpConfiguration::new(Some((127, 0, 0, 1)), Some(0))
            .with_limits(limits);
        start_tcp(conf, self.module(), &self.runtime).unwrap().unwrap()
    }
}

fn json(s: &str) -> Value { serde_json::from_str(s.trim_end()).unwrap() }

fn req(method: &str, params: Value, id: u64) -> String {
    json!({"jsonrpc": "2.0", "method": method, "params": params, "id": id})
        .to_string()
}

struct HttpResponse {
    status: u16,
    headers: HashMap<String, String>,
    body: String,
}

/// Minimal HTTP/1.1 client, so that status, headers and the exact body can
/// be checked.
fn http(
    addr: SocketAddr, method: &str, body: &[u8], headers: &[(&str, &str)],
) -> HttpResponse {
    let mut stream = TcpStream::connect(addr).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(30)))
        .unwrap();
    let mut request = format!(
        "{} / HTTP/1.1\r\nHost: {}\r\nContent-Length: {}\r\n",
        method,
        addr,
        body.len()
    );
    for (k, v) in headers {
        request.push_str(&format!("{}: {}\r\n", k, v));
    }
    request.push_str("\r\n");
    stream.write_all(request.as_bytes()).unwrap();
    // The server may answer (413) before reading the whole body.
    let _ = stream.write_all(body);

    let mut reader = BufReader::new(stream);
    let mut status_line = String::new();
    reader.read_line(&mut status_line).unwrap();
    let status = status_line.split(' ').nth(1).unwrap().parse().unwrap();
    let mut headers = HashMap::new();
    loop {
        let mut line = String::new();
        reader.read_line(&mut line).unwrap();
        let line = line.trim_end();
        if line.is_empty() {
            break;
        }
        let (k, v) = line.split_once(':').unwrap();
        headers.insert(k.trim().to_lowercase(), v.trim().to_string());
    }
    let mut body = vec![];
    if let Some(len) = headers.get("content-length") {
        body.resize(len.parse().unwrap(), 0);
        reader.read_exact(&mut body).unwrap();
    } else if headers.get("transfer-encoding").map(String::as_str)
        == Some("chunked")
    {
        loop {
            let mut size = String::new();
            reader.read_line(&mut size).unwrap();
            let size = usize::from_str_radix(size.trim(), 16).unwrap();
            let mut chunk = vec![0; size + 2];
            reader.read_exact(&mut chunk).unwrap();
            if size == 0 {
                break;
            }
            body.extend_from_slice(&chunk[..size]);
        }
    }
    HttpResponse {
        status,
        headers,
        body: String::from_utf8(body).unwrap(),
    }
}

fn post(addr: SocketAddr, body: &str) -> HttpResponse {
    http(addr, "POST", body.as_bytes(), &[(
        "Content-Type",
        "application/json",
    )])
}

#[test]
fn http_server() {
    let node = Node::new();
    let server = node.http(None, false);
    let addr = server.local_addr();

    // A known method gives the same response as before.
    let rp = post(addr, &req("trace_block", json!([ZERO_HASH]), 1));
    assert_eq!(rp.status, 200);
    assert_eq!(json(&rp.body), json(OLD_TRACE_BLOCK_UNKNOWN));
    assert!(rp.body.ends_with('\n'));
    assert_eq!(rp.headers["content-type"], "application/json; charset=utf-8");
    // `jsonrpc_http_keep_alive = false` (the default) closes connections.
    assert_eq!(rp.headers["connection"], "close");

    let rp = post(addr, &req("rpc_modules", json!([]), 1));
    let modules: BTreeSet<String> =
        serde_json::from_value(json(&rp.body)["result"].clone()).unwrap();
    assert_eq!(
        modules,
        ["pubsub", "trace"].iter().map(|s| s.to_string()).collect()
    );
    let rp = post(addr, &req("rpc_methods", json!([]), 1));
    let methods: BTreeSet<String> =
        serde_json::from_value(json(&rp.body)["result"].clone()).unwrap();
    for m in [
        "mazze_subscribe",
        "mazze_unsubscribe",
        "trace_block",
        "trace_epoch",
        "trace_filter",
        "trace_transaction",
    ] {
        assert!(methods.contains(m), "{} missing from {:?}", m, methods);
    }

    // Unknown method, parse error.
    let rp = post(addr, &req("mazze_doesNotExist", json!([]), 1));
    assert_eq!(json(&rp.body), json(OLD_METHOD_NOT_FOUND));
    let rp = post(addr, "{bad json");
    assert_eq!(json(&rp.body), json(OLD_PARSE_ERROR));

    // Batches, including a notification (which gets no response).
    let batch = format!(
        "[{},{},{}]",
        req("trace_block", json!([ZERO_HASH]), 1),
        req("nope", json!([]), 2),
        json!({"jsonrpc": "2.0", "method": "rpc_modules", "params": []}),
    );
    let rp = post(addr, &batch);
    assert_eq!(
        json(&rp.body),
        json!([
            json(OLD_TRACE_BLOCK_UNKNOWN),
            {"jsonrpc":"2.0","error":{"code":-32601,"message":"Method not found"},"id":2},
        ])
    );

    // A notification gets an empty 200 response.
    let rp = post(
        addr,
        &json!({"jsonrpc": "2.0", "method": "rpc_modules", "params": []})
            .to_string(),
    );
    assert_eq!((rp.status, rp.body.as_str()), (200, ""));

    // Subscriptions need a session.
    let rp = post(addr, &req("mazze_subscribe", json!(["newHeads"]), 1));
    assert_eq!(json(&rp.body), json(OLD_SUBSCRIBE_OVER_HTTP));

    // Request bodies are limited to 5 MiB.
    let big = format!("0x{}", "ab".repeat(3 * 1024 * 1024));
    let rp = post(addr, &req("trace_block", json!([big]), 1));
    assert_eq!((rp.status, rp.body.as_str()), (413, OLD_TOO_LARGE_BODY));
    // ... and a body just below the limit is processed.
    let almost = format!("0x{}", "ab".repeat(2 * 1024 * 1024));
    let rp = post(addr, &req("trace_block", json!([almost]), 1));
    assert_eq!(rp.status, 200);
    assert_eq!(json(&rp.body)["error"]["code"], json!(-32602));

    // Only POST and OPTIONS are served.
    let rp = http(addr, "GET", b"", &[]);
    assert_eq!((rp.status, rp.body.as_str()), (405, OLD_METHOD_NOT_ALLOWED));

    // Throttling (from the throttling file) applies over HTTP.
    let rp = post(addr, &req("trace_epoch", json!(["earliest"]), 1));
    assert_ne!(json(&rp.body)["error"]["code"], json!(-32072));
    let rp = post(addr, &req("trace_epoch", json!(["earliest"]), 2));
    assert_eq!(json(&rp.body)["error"]["code"], json!(-32072));
    assert_eq!(json(&rp.body)["error"]["message"], json!("Request rejected."));
}

#[test]
fn http_cors_and_keep_alive() {
    let node = Node::new();

    // Default: any origin is echoed back.
    let server = node.http(None, false);
    let addr = server.local_addr();
    let rp = http(addr, "POST", req("rpc_modules", json!([]), 1).as_bytes(), &[
        ("Content-Type", "application/json"),
        ("Origin", "http://example.com"),
    ]);
    assert_eq!(rp.status, 200);
    assert_eq!(rp.headers["access-control-allow-origin"], "http://example.com");
    assert_eq!(rp.headers["access-control-allow-methods"], "OPTIONS, POST");
    assert_eq!(rp.headers["vary"], "origin");
    let rp = http(addr, "OPTIONS", b"", &[
        ("Origin", "http://example.com"),
        ("Access-Control-Request-Method", "POST"),
        ("Access-Control-Request-Headers", "content-type"),
    ]);
    assert_eq!((rp.status, rp.body.as_str()), (200, ""));
    assert_eq!(rp.headers["allow"], "OPTIONS, POST");
    assert_eq!(rp.headers["accept"], "application/json");
    assert_eq!(rp.headers["access-control-allow-headers"], "content-type");
    assert_eq!(rp.headers["access-control-allow-origin"], "http://example.com");

    // An allow list rejects other origins; keep-alive drops `Connection`.
    let server = node.http(Some("http://allowed.com,*.wild.org"), true);
    let addr = server.local_addr();
    for (origin, allowed) in [
        ("http://allowed.com", true),
        ("http://a.wild.org", true),
        ("https://allowed.com", false),
        ("http://example.com", false),
    ] {
        let rp =
            http(addr, "POST", req("rpc_modules", json!([]), 1).as_bytes(), &[
                ("Content-Type", "application/json"),
                ("Origin", origin),
            ]);
        assert!(!rp.headers.contains_key("connection"));
        if allowed {
            assert_eq!(rp.status, 200);
            assert_eq!(rp.headers["access-control-allow-origin"], origin);
        } else {
            assert_eq!(rp.status, 403, "{}", origin);
            assert!(!rp.headers.contains_key("access-control-allow-origin"));
            assert!(rp.body.starts_with("Origin of the request is not whitelisted."));
        }
    }
}

fn read_line(reader: &mut BufReader<TcpStream>) -> String {
    let mut line = String::new();
    reader.read_line(&mut line).unwrap();
    line
}

#[test]
fn tcp_server() {
    let node = Node::new();
    let server = node.tcp();
    let stream = TcpStream::connect(server.local_addr()).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(30)))
        .unwrap();
    let mut writer = stream.try_clone().unwrap();
    let mut reader = BufReader::new(stream);
    let mut send = |line: &str| {
        writer.write_all(line.as_bytes()).unwrap();
        writer.write_all(b"\n").unwrap();
    };

    send(&req("trace_block", json!([ZERO_HASH]), 1));
    assert_eq!(json(&read_line(&mut reader)), json(OLD_TRACE_BLOCK_UNKNOWN));
    send(&req("nope", json!([]), 1));
    assert_eq!(json(&read_line(&mut reader)), json(OLD_METHOD_NOT_FOUND));
    send("{bad");
    assert_eq!(json(&read_line(&mut reader)), json(OLD_PARSE_ERROR));
    // A notification is answered with an empty line.
    send(&json!({"jsonrpc": "2.0", "method": "rpc_modules", "params": []}).to_string());
    assert_eq!(read_line(&mut reader), "\n");
    send(&format!(
        "[{},{}]",
        req("trace_block", json!([ZERO_HASH]), 1),
        req("nope", json!([]), 2)
    ));
    assert_eq!(
        json(&read_line(&mut reader)),
        json!([
            json(OLD_TRACE_BLOCK_UNKNOWN),
            {"jsonrpc":"2.0","error":{"code":-32601,"message":"Method not found"},"id":2},
        ])
    );

    // Errors defined by the node are unchanged.
    send(&req("mazze_subscribe", json!(["newHeads", {}]), 1));
    assert_eq!(json(&read_line(&mut reader)), json(OLD_HEADS_WITH_PARAMS));
    send(&req("mazze_unsubscribe", json!(["0x0000000000000001"]), 1));
    assert_eq!(json(&read_line(&mut reader)), json(OLD_UNSUBSCRIBE_UNKNOWN));
    send(&req("mazze_subscribe", json!(["notAKind"]), 1));
    assert_eq!(json(&read_line(&mut reader))["error"]["code"], json!(-32602));

    // A subscription delivers notifications on the connection.
    send(&req("mazze_subscribe", json!(["newHeads"]), 7));
    let rp = json(&read_line(&mut reader));
    assert_eq!(rp["id"], json!(7));
    let id = rp["result"].as_str().unwrap().to_string();
    assert_eq!(id.len(), 18);
    assert!(id.starts_with("0x"));
    assert!(node.notifications.new_block_hashes.send(node.genesis));
    let notification = json(&read_line(&mut reader));
    assert_eq!(notification["jsonrpc"], json!("2.0"));
    assert_eq!(notification["method"], json!("mazze_subscription"));
    assert_eq!(notification["params"]["subscription"], json!(id));
    assert_eq!(
        notification["params"]["result"]["hash"],
        json!(format!("{:?}", node.genesis))
    );
    send(&req("mazze_unsubscribe", json!([id]), 8));
    assert_eq!(json(&read_line(&mut reader)), json!({"jsonrpc":"2.0","result":true,"id":8}));
    send(&req("mazze_unsubscribe", json!([id]), 9));
    assert_eq!(json(&read_line(&mut reader)), json!({"jsonrpc":"2.0","result":false,"id":9}));

    // Throttling applies over TCP as well.
    send(&req("trace_epoch", json!(["earliest"]), 1));
    assert_ne!(json(&read_line(&mut reader))["error"]["code"], json!(-32072));
    send(&req("trace_epoch", json!(["earliest"]), 2));
    assert_eq!(json(&read_line(&mut reader))["error"]["code"], json!(-32072));
}

#[test]
fn ws_server() {
    let node = Node::new();
    let server = node.ws(64 * 1024);
    let url = format!("ws://{}", server.local_addr());
    let rt = tokio::runtime::Runtime::new().unwrap();

    rt.block_on(async {
        let client = WsClientBuilder::default().build(&url).await.unwrap();

        // A known method gives the same result as before.
        let result: Value = client
            .request("trace_block", rpc_params![ZERO_HASH])
            .await
            .unwrap();
        assert_eq!(result, json(OLD_TRACE_BLOCK_UNKNOWN)["result"]);

        // Unknown method.
        let err = client
            .request::<Value, _>("mazze_doesNotExist", rpc_params![])
            .await
            .unwrap_err();
        match err {
            jsonrpsee::core::client::Error::Call(e) => {
                assert_eq!(e.code(), -32601);
                assert_eq!(e.message(), "Method not found");
            }
            e => panic!("unexpected error {:?}", e),
        }

        // A subscription delivers notifications.
        let mut sub = client
            .subscribe::<Value, _>(
                "mazze_subscribe",
                rpc_params!["epochs"],
                "mazze_unsubscribe",
            )
            .await
            .unwrap();
        // Let the epoch loop subscribe to the channel before publishing.
        tokio::time::sleep(Duration::from_millis(200)).await;
        assert!(node
            .notifications
            .epochs_ordered
            .send((0, vec![node.genesis])));
        let item = tokio::time::timeout(Duration::from_secs(30), sub.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert_eq!(
            item,
            json!({"epochNumber": "0x0", "epochHashesOrdered": [format!("{:?}", node.genesis)]})
        );
        sub.unsubscribe().await.unwrap();

        // Batches work.
        let mut batch = jsonrpsee::core::params::BatchRequestBuilder::new();
        batch.insert("trace_block", rpc_params![ZERO_HASH]).unwrap();
        batch.insert("nope", rpc_params![]).unwrap();
        let responses =
            client.batch_request::<Value>(batch).await.unwrap();
        assert_eq!(responses.num_successful_calls(), 1);
        assert_eq!(responses.num_failed_calls(), 1);
    });

    // Requests above `jsonrpc_ws_max_payload_bytes` are rejected.
    rt.block_on(async {
        let client = WsClientBuilder::default()
            .max_request_size(u32::MAX)
            .build(&url)
            .await
            .unwrap();
        // Below the limit the request reaches the method.
        let almost = format!("0x{}", "ab".repeat(30 * 1024));
        match client
            .request::<Value, _>("trace_block", rpc_params![almost])
            .await
        {
            Err(jsonrpsee::core::client::Error::Call(e)) => {
                assert_eq!(e.code(), -32602, "{:?}", e)
            }
            r => panic!("unexpected response {:?}", r),
        }
        // Above it, the server answers with an `id: null` "request too big"
        // error (-32007), which the jsonrpsee client reports as a failure.
        let big = format!("0x{}", "ab".repeat(64 * 1024));
        assert!(client
            .request::<Value, _>("trace_block", rpc_params![big])
            .await
            .is_err());
    });
}

/// The configured limits are enforced: batch length on HTTP and TCP,
/// response size, request line size on TCP, and subscriptions per
/// connection.
#[test]
fn limits_are_enforced() {
    let node = Node::new();
    let batch = |n: u64| {
        format!(
            "[{}]",
            (0..n)
                .map(|i| req("rpc_modules", json!([]), i))
                .collect::<Vec<_>>()
                .join(",")
        )
    };

    // Pick a response limit between a small batch and the method list.
    let unlimited = node.http(None, false);
    let small = post(unlimited.local_addr(), &batch(2)).body.len();
    let large =
        post(unlimited.local_addr(), &req("rpc_methods", json!([]), 1)).body.len();
    assert!(small < large, "{small} vs {large}");
    let limits = RpcLimits {
        max_batch_size: 2,
        max_response_bytes: ((small + large) / 2) as u32,
        max_subscriptions_per_connection: 1,
        tcp_max_request_bytes: 4096,
        ..RpcLimits::default()
    };

    let server = node.http_with(limits);
    let addr = server.local_addr();
    let rp = json(&post(addr, &batch(2)).body);
    assert_eq!(rp.as_array().map(Vec::len), Some(2), "{rp}");
    let rp = json(&post(addr, &batch(3)).body);
    assert_eq!(rp["error"]["code"], -32010, "batch over the limit: {rp}");
    // `rpc_methods` lists every method: far more than 64 bytes.
    let rp = json(&post(addr, &req("rpc_methods", json!([]), 1)).body);
    assert_eq!(rp["error"]["code"], -32008, "oversized response: {rp}");

    let server = node.tcp_with(limits);
    let stream = TcpStream::connect(server.local_addr()).unwrap();
    stream.set_read_timeout(Some(Duration::from_secs(30))).unwrap();
    let mut writer = stream.try_clone().unwrap();
    let mut reader = BufReader::new(stream);
    let mut send = |line: &str| {
        writer.write_all(line.as_bytes()).unwrap();
        writer.write_all(b"\n").unwrap();
    };
    send(&batch(3));
    let rp = json(&read_line(&mut reader));
    assert_eq!(rp["error"]["code"], -32010, "TCP batch over the limit: {rp}");
    send(&req("rpc_methods", json!([]), 1));
    let rp = json(&read_line(&mut reader));
    assert_eq!(rp["error"]["code"], -32008, "TCP oversized response: {rp}");
    send(&req("mazze_subscribe", json!(["newHeads"]), 5));
    let rp = json(&read_line(&mut reader));
    assert!(rp["result"].is_string(), "first subscription: {rp}");
    send(&req("mazze_subscribe", json!(["newHeads"]), 6));
    let rp = json(&read_line(&mut reader));
    assert!(rp["error"].is_object(), "second subscription over the limit: {rp}");
    // A request line over the TCP limit closes the connection.
    send(&"x".repeat(8192));
    let mut rest = String::new();
    let closed = matches!(reader.read_line(&mut rest), Ok(0) | Err(_))
        || rest.contains("error");
    assert!(closed, "oversized TCP line accepted: {rest}");
}

#[test]
fn default_limits() {
    let l = RpcLimits::default();
    assert_eq!(l.max_batch_size, 1000);
    assert_eq!(l.max_response_bytes, 25 * 1024 * 1024);
    assert_eq!(l.http_max_connections, 1000);
    assert_eq!(l.ws_max_connections, 100);
    assert_eq!(l.max_subscriptions_per_connection, 1024);
    assert_eq!(l.tcp_max_request_bytes, 5 * 1024 * 1024);
}
