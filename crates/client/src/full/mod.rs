// Copyright 2024 Mazze Foundation. All rights reserved.
// Mazze is free software and distributed under GNU General Public License.
// See http://www.gnu.org/licenses/

use jsonrpc_http_server::Server as HttpServer;
use jsonrpc_tcp_server::Server as TcpServer;
use jsonrpc_ws_server::Server as WsServer;

use crate::{
    common::{initialize_not_light_node_modules, ClientComponents},
    configuration::Configuration,
};
use blockgen::BlockGenerator;
use malloc_size_of::{MallocSizeOf, MallocSizeOfOps};
use mazzecore::{
    pow::PowComputer, ConsensusGraph, NodeType, SynchronizationService,
    TransactionPool,
};
use parking_lot::{Condvar, Mutex};
use runtime::Runtime;
use std::sync::Arc;

pub struct FullClientExtraComponents {
    pub consensus: Arc<ConsensusGraph>,
    pub debug_rpc_http_server: Option<HttpServer>,
    pub rpc_http_server: Option<HttpServer>,
    pub debug_rpc_tcp_server: Option<TcpServer>,
    pub rpc_tcp_server: Option<TcpServer>,
    pub debug_rpc_ws_server: Option<WsServer>,
    pub rpc_ws_server: Option<WsServer>,
    pub runtime: Runtime,
    pub sync: Arc<SynchronizationService>,
    pub txpool: Arc<TransactionPool>,
    pub pow: Arc<PowComputer>,
    pub eth_rpc_http_server: Option<HttpServer>,
    pub eth_rpc_ws_server: Option<WsServer>,
}

impl MallocSizeOf for FullClientExtraComponents {
    fn size_of(&self, _ops: &mut MallocSizeOfOps) -> usize {
        unimplemented!()
    }
}

pub struct FullClient {}

impl FullClient {
    // Start all key components of Mazze and pass out their handles
    pub fn start(
        mut conf: Configuration, exit: Arc<(Mutex<bool>, Condvar)>,
    ) -> Result<
        Box<ClientComponents<BlockGenerator, FullClientExtraComponents>>,
        String,
    > {
        let (
            data_man,
            pow,
            txpool,
            consensus,
            sync,
            blockgen,
            debug_rpc_http_server,
            rpc_http_server,
            debug_rpc_tcp_server,
            rpc_tcp_server,
            debug_rpc_ws_server,
            rpc_ws_server,
            runtime,
            eth_rpc_http_server,
            eth_rpc_ws_server,
        ) = initialize_not_light_node_modules(&mut conf, exit, NodeType::Full)?;
        Ok(Box::new(ClientComponents {
            data_manager_weak_ptr: Arc::downgrade(&data_man),
            blockgen: Some(blockgen),
            other_components: FullClientExtraComponents {
                consensus,
                debug_rpc_http_server,
                rpc_http_server,
                debug_rpc_tcp_server,
                rpc_tcp_server,
                debug_rpc_ws_server,
                rpc_ws_server,
                runtime,
                sync,
                txpool,
                pow,
                eth_rpc_http_server,
                eth_rpc_ws_server,
            },
        }))
    }
}
