// Copyright 2024 Mazze Foundation. All rights reserved.
// Mazze is free software and distributed under GNU General Public License.
// See http://www.gnu.org/licenses/

use jsonrpsee::{
    server::{
        middleware::rpc::RpcServiceBuilder, BatchRequestConfig, Server,
        ServerConfig, ServerHandle,
    },
    Methods, RpcModule,
};
use std::{
    net::{Ipv4Addr, SocketAddr, SocketAddrV4},
    sync::Arc,
};

mod authcodes;
pub mod error_codes;
mod helpers;
pub mod http_common;
pub mod impls;
pub mod informant;
mod interceptor;
pub mod rpc_apis;
pub mod tcp;
pub mod traits;
pub mod types;

pub use mazzecore::rpc_errors::{
    invalid_params, invalid_params_check, BoxFuture as RpcBoxFuture,
    Error as RpcError, ErrorKind as RpcErrorKind,
    ErrorKind::JsonRpcError as JsonRpcErrorKind, Result as RpcResult,
};

use self::{
    impls::{
        common::RpcImpl as CommonImpl,
        eth_pubsub::PubSubClient as EthPubSubClient,
        light::{
            DebugRpcImpl as LightDebugRpcImpl,
            MazzeHandler as LightMazzeHandler, RpcImpl as LightImpl,
            TestRpcImpl as LightTestRpcImpl,
        },
        mazze::{LocalRpcImpl, MazzeHandler, RpcImpl, TestRpcImpl},
        mazze_filter::MazzeFilterClient,
        pool::TransactionPoolHandler,
        pubsub::PubSubClient,
        trace::TraceHandler,
    },
    traits::{
        debug::LocalRpcServer,
        eth_space::{
            eth::{EthFilterServer, EthServer},
            eth_pubsub::EthPubSubServer,
            trace::TraceServer as EthTraceServer,
        },
        mazze::{MazzeFilterServer, MazzeServer},
        pool::TransactionPoolServer,
        pubsub::PubSubServer,
        test::TestRpcServer,
        trace::TraceServer,
    },
};

pub use self::types::{Block as RpcBlock, Origin};
use crate::{
    configuration::Configuration,
    rpc::{
        error_codes::request_rejected_too_many_request_error,
        impls::{
            eth::{EthHandler, GethDebugHandler},
            eth_filter::EthFilterClient,
            trace::EthTraceHandler,
            RpcImplConfiguration,
        },
        helpers::HexIdProvider,
        http_common::{
            AccessControlAllowOrigin, DomainsValidation, HttpCompatLayer,
            NoSubscriptions,
        },
        interceptor::{intercept, RpcInterceptor},
        rpc_apis::{Api, ApiSet},
        traits::eth_space::debug::DebugServer,
    },
};
use lazy_static::lazy_static;
use mazze_rpc_error::Result as JsonRpcResult;
use metrics::{register_timer_with_group, ScopeTimer, Timer};
use runtime::Runtime;
use std::any::Any;
use parking_lot::Mutex;
use std::collections::{HashMap, HashSet};
use throttling::token_bucket::{ThrottleResult, TokenBucketManager};

lazy_static! {
    static ref METRICS_INTERCEPTOR_TIMERS: Mutex<HashMap<String, Arc<dyn Timer>>> =
        Default::default();
}

#[derive(Debug, PartialEq)]
pub struct TcpConfiguration {
    pub enabled: bool,
    pub address: SocketAddr,
}

impl TcpConfiguration {
    pub fn new(ip: Option<(u8, u8, u8, u8)>, port: Option<u16>) -> Self {
        let ipv4 = match ip {
            Some(ip) => Ipv4Addr::new(ip.0, ip.1, ip.2, ip.3),
            None => Ipv4Addr::new(0, 0, 0, 0),
        };
        TcpConfiguration {
            enabled: port.is_some(),
            address: SocketAddr::V4(SocketAddrV4::new(ipv4, port.unwrap_or(0))),
        }
    }
}

#[derive(Debug, PartialEq)]
pub struct HttpConfiguration {
    pub enabled: bool,
    pub address: SocketAddr,
    pub cors_domains: DomainsValidation<AccessControlAllowOrigin>,
    pub keep_alive: bool,
    // If it's Some, we will manually set the number of threads of HTTP RPC
    // server
    pub threads: Option<usize>,
}

impl HttpConfiguration {
    pub fn new(
        ip: Option<(u8, u8, u8, u8)>, port: Option<u16>, cors: Option<String>,
        keep_alive: bool, threads: Option<usize>,
    ) -> Self {
        let ipv4 = match ip {
            Some(ip) => Ipv4Addr::new(ip.0, ip.1, ip.2, ip.3),
            None => Ipv4Addr::new(0, 0, 0, 0),
        };
        HttpConfiguration {
            enabled: port.is_some(),
            address: SocketAddr::V4(SocketAddrV4::new(ipv4, port.unwrap_or(0))),
            cors_domains: match cors {
                None => DomainsValidation::Disabled,
                Some(cors_list) => match cors_list.as_str() {
                    "none" => DomainsValidation::Disabled,
                    "all" => DomainsValidation::AllowOnly(vec![
                        AccessControlAllowOrigin::Any,
                    ]),
                    _ => DomainsValidation::AllowOnly(
                        cors_list.split(',').map(Into::into).collect(),
                    ),
                },
            },
            keep_alive,
            threads,
        }
    }
}

#[derive(Debug, PartialEq)]
pub struct WsConfiguration {
    pub enabled: bool,
    pub address: SocketAddr,
    pub max_payload_bytes: usize,
}

impl WsConfiguration {
    pub fn new(
        ip: Option<(u8, u8, u8, u8)>, port: Option<u16>,
        max_payload_bytes: usize,
    ) -> Self {
        let ipv4 = match ip {
            Some(ip) => Ipv4Addr::new(ip.0, ip.1, ip.2, ip.3),
            None => Ipv4Addr::new(0, 0, 0, 0),
        };
        WsConfiguration {
            enabled: port.is_some(),
            address: SocketAddr::V4(SocketAddrV4::new(ipv4, port.unwrap_or(0))),
            max_payload_bytes,
        }
    }
}

pub fn setup_public_rpc_apis(
    common: Arc<CommonImpl>, rpc: Arc<RpcImpl>, pubsub: PubSubClient,
    eth_pubsub: EthPubSubClient, conf: &Configuration,
) -> RpcModule<()> {
    setup_rpc_apis(
        common,
        rpc,
        pubsub,
        eth_pubsub,
        &conf.raw_conf.throttling_conf,
        "rpc",
        conf.raw_conf.public_rpc_apis.list_apis(),
    )
}

pub fn setup_public_eth_rpc_apis(
    common: Arc<CommonImpl>, rpc: Arc<RpcImpl>, pubsub: PubSubClient,
    eth_pubsub: EthPubSubClient, conf: &Configuration,
) -> RpcModule<()> {
    setup_rpc_apis(
        common,
        rpc,
        pubsub,
        eth_pubsub,
        &conf.raw_conf.throttling_conf,
        "rpc",
        conf.raw_conf.public_evm_rpc_apis.list_apis(),
    )
}

pub fn setup_debug_rpc_apis(
    common: Arc<CommonImpl>, rpc: Arc<RpcImpl>, pubsub: PubSubClient,
    eth_pubsub: EthPubSubClient, conf: &Configuration,
) -> RpcModule<()> {
    let mut debug_apis = ApiSet::All.list_apis();
    if !conf.is_test_or_dev_mode() {
        debug_apis.remove(&Api::Test);
    }
    setup_rpc_apis(
        common,
        rpc,
        pubsub,
        eth_pubsub,
        &conf.raw_conf.throttling_conf,
        "rpc_local",
        debug_apis,
    )
}

fn setup_rpc_apis(
    common: Arc<CommonImpl>, rpc: Arc<RpcImpl>, pubsub: PubSubClient,
    eth_pubsub: EthPubSubClient, throttling_conf: &Option<String>,
    throttling_section: &str, apis: HashSet<Api>,
) -> RpcModule<()> {
    let mut handler = RpcModule::new(());
    for api in &apis {
        match api {
            Api::Mazze => {
                let mazze = MazzeHandler::new(common.clone(), rpc.clone())
                    .into_rpc();
                extend_with_interceptor(
                    &mut handler,
                    &rpc.config,
                    mazze,
                    throttling_conf,
                    throttling_section,
                );

                if let Some(poll_lifetime) = rpc.config.poll_lifetime_in_seconds
                {
                    if let Some(h) = pubsub.handler().upgrade() {
                        let filter_client = MazzeFilterClient::new(
                            rpc.consensus.clone(),
                            rpc.tx_pool.clone(),
                            eth_pubsub.epochs_ordered(),
                            h.executor.clone(),
                            poll_lifetime,
                            rpc.config.get_logs_filter_max_limit,
                            h.network.clone(),
                        )
                        .into_rpc();

                        extend_with_interceptor(
                            &mut handler,
                            &rpc.config,
                            filter_client,
                            throttling_conf,
                            throttling_section,
                        );
                    }
                }
            }
            Api::Eth => {
                info!("Add EVM RPC");
                let evm = EthHandler::new(
                    rpc.config.clone(),
                    rpc.consensus.clone(),
                    rpc.sync.clone(),
                    rpc.tx_pool.clone(),
                )
                .into_rpc();
                let evm_trace_handler = EthTraceHandler {
                    trace_handler: TraceHandler::new(
                        rpc.consensus.get_data_manager().clone(),
                        *rpc.sync.network.get_network_type(),
                        rpc.consensus.clone(),
                    ),
                }
                .into_rpc();
                extend_with_interceptor(
                    &mut handler,
                    &rpc.config,
                    evm,
                    throttling_conf,
                    throttling_section,
                );
                extend_with(&mut handler, evm_trace_handler);

                if let Some(poll_lifetime) = rpc.config.poll_lifetime_in_seconds
                {
                    if let Some(h) = eth_pubsub.handler().upgrade() {
                        let filter_client = EthFilterClient::new(
                            rpc.consensus.clone(),
                            rpc.tx_pool.clone(),
                            eth_pubsub.epochs_ordered(),
                            h.executor.clone(),
                            poll_lifetime,
                            rpc.config.get_logs_filter_max_limit,
                        )
                        .into_rpc();

                        extend_with_interceptor(
                            &mut handler,
                            &rpc.config,
                            filter_client,
                            throttling_conf,
                            throttling_section,
                        );
                    }
                }
            }
            Api::Debug => {
                extend_with(&mut handler, 
                    LocalRpcImpl::new(common.clone(), rpc.clone())
                        .into_rpc(),
                );
            }
            Api::Pubsub => {
                extend_with_interceptor(
                    &mut handler,
                    &rpc.config,
                    pubsub.clone().into_rpc(),
                    throttling_conf,
                    throttling_section,
                );
            }
            Api::EthPubsub => {
                info!("Add EVM pubsub");
                extend_with_interceptor(
                    &mut handler,
                    &rpc.config,
                    eth_pubsub.clone().into_rpc(),
                    throttling_conf,
                    throttling_section,
                );
            }
            Api::EthDebug => {
                info!("Add geth debug method");
                let geth_debug = GethDebugHandler::new(rpc.consensus.clone());
                extend_with(&mut handler, geth_debug.into_rpc());
            }
            Api::Test => {
                extend_with(&mut handler, 
                    TestRpcImpl::new(common.clone(), rpc.clone()).into_rpc(),
                );
            }
            Api::Trace => {
                let trace = TraceHandler::new(
                    rpc.consensus.get_data_manager().clone(),
                    *rpc.sync.network.get_network_type(),
                    rpc.consensus.clone(),
                )
                .into_rpc();
                extend_with_interceptor(
                    &mut handler,
                    &rpc.config,
                    trace,
                    throttling_conf,
                    throttling_section,
                );
            }
            Api::TxPool => {
                let txpool =
                    TransactionPoolHandler::new(common.clone()).into_rpc();
                extend_with_interceptor(
                    &mut handler,
                    &rpc.config,
                    txpool,
                    throttling_conf,
                    throttling_section,
                );
            }
        }
    }

    add_meta_rpc_methods(handler, apis)
}

/// Adds `methods` to `handler`. A method that is already present is
/// replaced, like `MetaIoHandler::extend_with` did (e.g. `trace_block` of
/// the core space and of the eSpace when both APIs are enabled).
fn extend_with(handler: &mut RpcModule<()>, methods: impl Into<Methods>) {
    let methods: Methods = methods.into();
    for name in methods.method_names() {
        handler.remove_method(name);
    }
    handler
        .merge(methods)
        .expect("conflicting method names were removed above; qed");
}

pub fn extend_with_interceptor(
    handler: &mut RpcModule<()>, rpc_conf: &RpcImplConfiguration,
    rpc_impl: impl Into<Methods>, throttling_conf: &Option<String>,
    throttling_section: &str,
) {
    let interceptor =
        ThrottleInterceptor::new(throttling_conf, throttling_section);
    if rpc_conf.enable_metrics {
        extend_with(
            handler,
            intercept(rpc_impl, MetricsInterceptor::new(interceptor)),
        );
    } else {
        extend_with(handler, intercept(rpc_impl, interceptor));
    }
}

/// Adds `rpc_methods` (every method of `handler`) and `rpc_modules` (the
/// names of `apis`).
pub fn add_meta_rpc_methods(
    mut handler: RpcModule<()>, apis: HashSet<Api>,
) -> RpcModule<()> {
    // rpc_methods to return all available methods
    let methods: Vec<String> =
        handler.method_names().map(String::from).collect();
    handler
        .register_method("rpc_methods", move |_, _, _| methods.clone())
        .expect("rpc_methods is not registered by any API; qed");

    // rpc_modules
    let namespaces: Vec<String> =
        apis.into_iter().map(|item| format!("{}", item)).collect();
    handler
        .register_method("rpc_modules", move |_, _, _| namespaces.clone())
        .expect("rpc_modules is not registered by any API; qed");

    handler
}

pub fn setup_public_rpc_apis_light(
    common: Arc<CommonImpl>, rpc: Arc<LightImpl>, pubsub: PubSubClient,
    eth_pubsub: EthPubSubClient, conf: &Configuration,
) -> RpcModule<()> {
    setup_rpc_apis_light(
        common,
        rpc,
        pubsub,
        eth_pubsub,
        &conf.raw_conf.throttling_conf,
        "rpc",
        conf.raw_conf.public_rpc_apis.list_apis(),
    )
}

pub fn setup_debug_rpc_apis_light(
    common: Arc<CommonImpl>, rpc: Arc<LightImpl>, pubsub: PubSubClient,
    eth_pubsub: EthPubSubClient, conf: &Configuration,
) -> RpcModule<()> {
    let mut light_debug_apis = ApiSet::All.list_apis();
    light_debug_apis.remove(&Api::Trace);
    if !conf.is_test_or_dev_mode() {
        light_debug_apis.remove(&Api::Test);
    }
    setup_rpc_apis_light(
        common,
        rpc,
        pubsub,
        eth_pubsub,
        &conf.raw_conf.throttling_conf,
        "rpc_local",
        light_debug_apis,
    )
}

fn setup_rpc_apis_light(
    common: Arc<CommonImpl>, rpc: Arc<LightImpl>, pubsub: PubSubClient,
    eth_pubsub: EthPubSubClient, throttling_conf: &Option<String>,
    throttling_section: &str, apis: HashSet<Api>,
) -> RpcModule<()> {
    let mut handler = RpcModule::new(());
    for api in apis {
        match api {
            Api::Mazze => {
                let mazze = LightMazzeHandler::new(common.clone(), rpc.clone())
                    .into_rpc();
                let interceptor = ThrottleInterceptor::new(
                    throttling_conf,
                    throttling_section,
                );
                extend_with(&mut handler, intercept(mazze, interceptor));
            }
            Api::Eth => {
                warn!("Light nodes do not support evm ports.");
            }
            Api::EthDebug => {
                warn!("Light nodes do not support evm ports.");
            }
            Api::Debug => {
                extend_with(&mut handler, 
                    LightDebugRpcImpl::new(common.clone(), rpc.clone())
                        .into_rpc(),
                );
            }
            Api::Pubsub => extend_with(&mut handler, pubsub.clone().into_rpc()),
            Api::EthPubsub => {
                extend_with(&mut handler, eth_pubsub.clone().into_rpc())
            }
            Api::Test => {
                extend_with(&mut handler, 
                    LightTestRpcImpl::new(common.clone(), rpc.clone())
                        .into_rpc(),
                );
            }
            Api::Trace => {
                warn!("Light nodes do not support trace RPC");
            }
            Api::TxPool => {
                warn!("Light nodes do not support txpool RPC");
            }
        }
    }
    handler
}

// Limits of the HTTP and WebSocket servers. Each one is at least as
// permissive as the effective behaviour of the previous
// `jsonrpc-http-server` / `jsonrpc-ws-server` 15 servers.

/// Maximum HTTP request body: the `jsonrpc-http-server` default (5 MiB),
/// which the node never changed.
pub const HTTP_MAX_REQUEST_BODY_SIZE: u32 = 5 * 1024 * 1024;
/// Maximum HTTP response body: responses were not limited before, so use
/// the largest value jsonrpsee accepts.
pub const HTTP_MAX_RESPONSE_BODY_SIZE: u32 = u32::MAX;
/// Maximum concurrent HTTP connections: hyper 0.12 did not limit them.
pub const HTTP_MAX_CONNECTIONS: u32 = u32::MAX;
/// Maximum concurrent WebSocket connections: the `jsonrpc-ws-server`
/// default (100), which the node never changed.
pub const WS_MAX_CONNECTIONS: u32 = 100;
/// Maximum subscriptions per connection: `jsonrpc-pubsub` had no limit.
pub const MAX_SUBSCRIPTIONS_PER_CONNECTION: u32 = u32::MAX;
/// Messages buffered per WebSocket connection before notifications wait
/// for the client (jsonrpsee default; the old ws-rs out buffer was bounded
/// by `jsonrpc_ws_max_payload_bytes` instead).
pub const WS_MESSAGE_BUFFER_CAPACITY: u32 = 1024;
/// Batch requests: the old servers accepted batches of any length.
pub fn batch_request_config() -> BatchRequestConfig {
    BatchRequestConfig::Unlimited
}

pub use self::tcp::TcpServer;
/// An HTTP RPC server.
pub type HttpServer = RpcServerHandle;
/// A WebSocket RPC server.
pub type WsServer = RpcServerHandle;

/// Handle of a running jsonrpsee server; dropping it stops the server.
pub struct RpcServerHandle {
    handle: ServerHandle,
    local_addr: SocketAddr,
}

impl RpcServerHandle {
    /// The address the server listens on.
    pub fn local_addr(&self) -> SocketAddr { self.local_addr }
}

impl Drop for RpcServerHandle {
    fn drop(&mut self) { let _ = self.handle.stop(); }
}

pub fn start_tcp(
    conf: TcpConfiguration, handler: RpcModule<()>, runtime: &Runtime,
) -> Result<Option<TcpServer>, String> {
    if !conf.enabled {
        return Ok(None);
    }

    match tcp::start(
        &conf.address,
        handler.into(),
        Arc::new(HexIdProvider::default()),
        tcp::TcpLimits::default(),
        runtime.handle(),
    ) {
        Ok(server) => Ok(Some(server)),
        Err(io_error) => {
            Err(format!("TCP error: {} (addr = {})", io_error, conf.address))
        }
    }
}

pub fn start_http(
    conf: HttpConfiguration, handler: RpcModule<()>, runtime: &Runtime,
) -> Result<Option<HttpServer>, String> {
    if !conf.enabled {
        return Ok(None);
    }

    let config = ServerConfig::builder()
        .http_only()
        .max_request_body_size(HTTP_MAX_REQUEST_BODY_SIZE)
        .max_response_body_size(HTTP_MAX_RESPONSE_BODY_SIZE)
        .max_connections(HTTP_MAX_CONNECTIONS)
        .max_subscriptions_per_connection(MAX_SUBSCRIPTIONS_PER_CONNECTION)
        .set_batch_request_config(batch_request_config())
        .set_id_provider(HexIdProvider::default())
        // The old server did not time out idle new connections.
        .set_first_request_timeout(None)
        .build();
    let middleware = tower::ServiceBuilder::new()
        .layer(HttpCompatLayer::new(conf.cors_domains.clone(), conf.keep_alive));
    let methods: Methods = handler.into();
    let subscriptions = methods.clone();
    let rpc_middleware = RpcServiceBuilder::new()
        .layer_fn(move |service| NoSubscriptions::new(service, &subscriptions));
    let builder = Server::builder()
        .set_config(config)
        .set_rpc_middleware(rpc_middleware)
        .set_http_middleware(middleware);
    let handler = methods;

    let address = conf.address;
    let start = || -> std::io::Result<RpcServerHandle> {
        let server = runtime.handle().block_on(builder.build(address))?;
        let local_addr = server.local_addr()?;
        let _guard = runtime.handle().enter();
        Ok(RpcServerHandle {
            handle: server.start(handler),
            local_addr,
        })
    };
    match start() {
        Ok(server) => Ok(Some(server)),
        Err(io_error) => Err(format!(
            "HTTP error: {} (addr = {})",
            io_error, conf.address
        )),
    }
}

pub fn start_ws(
    conf: WsConfiguration, handler: RpcModule<()>, runtime: &Runtime,
) -> Result<Option<WsServer>, String> {
    if !conf.enabled {
        return Ok(None);
    }

    // `jsonrpc_ws_max_payload_bytes` bounded both the incoming and the
    // outgoing buffer of a WebSocket connection.
    let max_payload =
        u32::try_from(conf.max_payload_bytes).unwrap_or(u32::MAX);
    let config = ServerConfig::builder()
        .ws_only()
        .max_request_body_size(max_payload)
        .max_response_body_size(max_payload)
        .max_connections(WS_MAX_CONNECTIONS)
        .max_subscriptions_per_connection(MAX_SUBSCRIPTIONS_PER_CONNECTION)
        .set_message_buffer_capacity(WS_MESSAGE_BUFFER_CAPACITY)
        .set_batch_request_config(batch_request_config())
        .set_id_provider(HexIdProvider::default())
        // The old server did not time out idle new connections.
        .set_first_request_timeout(None)
        .build();
    let builder = Server::builder().set_config(config);

    let address = conf.address;
    let start = || -> std::io::Result<RpcServerHandle> {
        let server = runtime.handle().block_on(builder.build(address))?;
        let local_addr = server.local_addr()?;
        let _guard = runtime.handle().enter();
        Ok(RpcServerHandle {
            handle: server.start(handler),
            local_addr,
        })
    };
    match start() {
        Ok(server) => Ok(Some(server)),
        Err(io_error) => {
            Err(format!("WS error: {} (addr = {})", io_error, conf.address))
        }
    }
}

struct ThrottleInterceptor {
    manager: TokenBucketManager,
}

impl ThrottleInterceptor {
    fn new(file: &Option<String>, section: &str) -> Self {
        let manager = match file {
            Some(file) => TokenBucketManager::load(file, Some(section))
                .expect("invalid throttling configuration file"),
            None => TokenBucketManager::default(),
        };

        ThrottleInterceptor { manager }
    }
}

impl RpcInterceptor for ThrottleInterceptor {
    fn before(&self, name: &String) -> JsonRpcResult<()> {
        let bucket = match self.manager.get(name) {
            Some(bucket) => bucket,
            None => return Ok(()),
        };

        let result = bucket.lock().throttle_default();

        match result {
            ThrottleResult::Success => Ok(()),
            ThrottleResult::Throttled(wait_time) => {
                debug!("RPC {} throttled in {:?}", name, wait_time);
                bail!(request_rejected_too_many_request_error(Some(format!(
                    "throttled in {:?}",
                    wait_time
                ))))
            }
            ThrottleResult::AlreadyThrottled => {
                debug!("RPC {} already throttled", name);
                bail!(request_rejected_too_many_request_error(Some(
                    "already throttled, please try again later".into()
                )))
            }
        }
    }
}

struct MetricsInterceptor {
    // TODO: Chain interceptors instead of wrapping up.
    throttle_interceptor: ThrottleInterceptor,
}

impl MetricsInterceptor {
    pub fn new(throttle_interceptor: ThrottleInterceptor) -> Self {
        Self {
            throttle_interceptor,
        }
    }
}

impl RpcInterceptor for MetricsInterceptor {
    fn before(&self, name: &String) -> JsonRpcResult<()> {
        self.throttle_interceptor.before(name)?;
        // Use a global variable here because `http` and `web3` setup different
        // interceptors for the same RPC API.
        let mut timers = METRICS_INTERCEPTOR_TIMERS.lock();
        if !timers.contains_key(name) {
            let timer = register_timer_with_group("rpc", name.as_str());
            timers.insert(name.clone(), timer);
        }
        Ok(())
    }

    fn around(&self, name: &String) -> Option<Box<dyn Any + Send>> {
        let timer = METRICS_INTERCEPTOR_TIMERS.lock().get(name).cloned()?;
        Some(Box::new(ScopeTimer::time_scope(timer)))
    }
}
