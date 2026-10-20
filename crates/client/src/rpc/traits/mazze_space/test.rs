// Copyright 2024 Mazze Foundation. All rights reserved.
// Mazze is free software and distributed under GNU General Public License.
// See http://www.gnu.org/licenses/

use crate::rpc::types::{BlameInfo, Block, Bytes};
use mazze_rpc_error::Result as RpcResult;
use jsonrpsee::proc_macros::rpc;
use mazze_types::{H256, U256};
use mazzecore::PeerInfo;
use network::node_table::NodeId;
use std::net::SocketAddr;

#[rpc(server)]
pub trait TestRpc {
    #[method(name = "sayhello", blocking)]
    fn say_hello(&self) -> RpcResult<String>;

    #[method(name = "getblockcount", blocking)]
    fn get_block_count(&self) -> RpcResult<u64>;

    #[method(name = "getgoodput", blocking)]
    fn get_goodput(&self) -> RpcResult<String>;

    #[method(name = "generate_empty_blocks", blocking)]
    fn generate_empty_blocks(&self, num_blocks: usize) -> RpcResult<Vec<H256>>;

    #[method(name = "generatefixedblock", blocking)]
    fn generate_fixed_block(
        &self, parent_hash: H256, referee: Vec<H256>, num_txs: usize,
        adaptive: bool, difficulty: Option<u64>,
    ) -> RpcResult<H256>;

    #[method(name = "addnode", blocking)]
    fn add_peer(&self, id: NodeId, addr: SocketAddr) -> RpcResult<()>;

    #[method(name = "removenode", blocking)]
    fn drop_peer(&self, id: NodeId, addr: SocketAddr) -> RpcResult<()>;

    #[method(name = "getpeerinfo", blocking)]
    fn get_peer_info(&self) -> RpcResult<Vec<PeerInfo>>;

    /// Returns the JSON of whole chain
    #[method(name = "mazze_getChain", blocking)]
    fn chain(&self) -> RpcResult<Vec<Block>>;

    #[method(name = "stop", blocking)]
    fn stop(&self) -> RpcResult<()>;

    #[method(name = "getnodeid", blocking)]
    fn get_nodeid(&self, challenge: Vec<u8>) -> RpcResult<Vec<u8>>;

    #[method(name = "addlatency", blocking)]
    fn add_latency(&self, id: NodeId, latency_ms: f64) -> RpcResult<()>;

    #[method(name = "generateoneblock", blocking)]
    fn generate_one_block(
        &self, num_txs: usize, block_size_limit: usize,
    ) -> RpcResult<H256>;

    #[method(name = "generate_one_block_with_direct_txgen", blocking)]
    fn generate_one_block_with_direct_txgen(
        &self, num_txs: usize, block_size_limit: usize, num_txs_simple: usize,
        num_txs_erc20: usize,
    ) -> RpcResult<H256>;

    #[method(name = "test_generatecustomblock", blocking)]
    fn generate_custom_block(
        &self, parent: H256, referees: Vec<H256>, raw: Bytes,
        adaptive: Option<bool>, custom: Option<Vec<Bytes>>,
    ) -> RpcResult<H256>;

    #[method(name = "test_generateblockwithfaketxs", blocking)]
    fn generate_block_with_fake_txs(
        &self, raw: Bytes, adaptive: Option<bool>, tx_data_len: Option<usize>,
    ) -> RpcResult<H256>;

    #[method(name = "test_generateblockwithblameinfo", blocking)]
    fn generate_block_with_blame_info(
        &self, num_txs: usize, block_size_limit: usize, blame_info: BlameInfo,
    ) -> RpcResult<H256>;

    #[method(name = "test_generate_block_with_nonce_and_timestamp", blocking)]
    fn generate_block_with_nonce_and_timestamp(
        &self, parent: H256, referees: Vec<H256>, raw: Bytes, nonce: U256,
        timestamp: u64, adaptive: bool,
    ) -> RpcResult<H256>;

    #[method(name = "get_block_status", blocking)]
    fn get_block_status(&self, block_hash: H256) -> RpcResult<(u8, bool)>;

    #[method(name = "expireblockgc", blocking)]
    fn expire_block_gc(&self, timeout: u64) -> RpcResult<()>;

    #[method(name = "getMainChainAndWeight", blocking)]
    fn get_main_chain_and_weight(
        &self, height_range: Option<(u64, u64)>,
    ) -> RpcResult<Vec<(H256, U256)>>;

    #[method(name = "getExecutedInfo", blocking)]
    fn get_executed_info(&self, block_hash: H256) -> RpcResult<(H256, H256)>;
    #[method(name = "test_sendUsableGenesisAccounts", blocking)]
    fn send_usable_genesis_accounts(
        &self, account_start_index: usize,
    ) -> RpcResult<Bytes>;

    #[method(name = "set_db_crash", blocking)]
    fn set_db_crash(
        &self, crash_probability: f64, crash_exit_code: i32,
    ) -> RpcResult<()>;

    #[method(name = "save_node_db", blocking)]
    fn save_node_db(&self) -> RpcResult<()>;
}
