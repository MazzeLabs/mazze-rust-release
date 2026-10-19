// Generated from the former `cli.yaml` (clap 2 `yaml` feature),
// translated 1:1 into builder calls so the `yaml-rust` 0.3
// dependency (RUSTSEC-2018-0006) can be dropped. Edit directly.

use clap::{App, AppSettings, Arg, SubCommand};

pub fn cli_app() -> App<'static, 'static> {
    App::new("mazze")
        .author("The Mazze Team")
        .about("Mazze client.")
        .arg(
            Arg::with_name("mode")
                .help("Use the preset testing configurations.")
                .long("mode")
                .value_name("MODE")
                .takes_value(true)
                .possible_value("dev")
                .possible_value("test"),
        )
        .arg(
            Arg::with_name("port")
                .help("Specify the port for P2P connections.")
                .short("p")
                .long("port")
                .value_name("PORT")
                .takes_value(true),
        )
        .arg(
            Arg::with_name("udp-port")
                .help("Specify the UDP port for peer discovery.")
                .long("udp-port")
                .value_name("PORT")
                .takes_value(true),
        )
        .arg(
            Arg::with_name("jsonrpc-ws-port")
                .help("Specify the port for the WebSocket JSON-RPC API server.")
                .long("jsonrpc-ws-port")
                .value_name("PORT")
                .takes_value(true),
        )
        .arg(
            Arg::with_name("jsonrpc-tcp-port")
                .help("Specify the port for the TCP JSON-RPC API server.")
                .long("jsonrpc-tcp-port")
                .value_name("PORT")
                .takes_value(true),
        )
        .arg(
            Arg::with_name("jsonrpc-http-port")
                .help("Specify the port for the HTTP JSON-RPC API server.")
                .long("jsonrpc-http-port")
                .value_name("PORT")
                .takes_value(true),
        )
        .arg(
            Arg::with_name("jsonrpc-cors")
                .help("Specify CORS header for HTTP JSON-RPC API responses.")
                .long("jsonrpc-cors")
                .value_name("URL")
                .takes_value(true),
        )
        .arg(
            Arg::with_name("jsonrpc-http-keep-alive")
                .help("Enable HTTP/1.1 keep alive header. Enabling keep alive will re-use the same TCP connection to fire multiple requests.")
                .long("jsonrpc-http-keep-alive")
                .value_name("BOOL")
                .takes_value(true),
        )
        .arg(
            Arg::with_name("log-file")
                .help("Specify the filename for the log. Stdout will be used by default if omitted.")
                .long("log-file")
                .value_name("FILE")
                .takes_value(true),
        )
        .arg(
            Arg::with_name("log-level")
                .help("Can be error/warn/info/debug/trace. Default is the info level.")
                .long("log-level")
                .value_name("LEVEL")
                .takes_value(true),
        )
        .arg(
            Arg::with_name("log-conf")
                .help("Sets a custom log config file.")
                .long("log-conf")
                .value_name("FILE")
                .takes_value(true),
        )
        .arg(
            Arg::with_name("config")
                .help("Sets a custom config file.")
                .short("c")
                .long("config")
                .value_name("FILE")
                .takes_value(true),
        )
        .arg(
            Arg::with_name("bootnodes")
                .help("Sets a custom list of bootnodes.")
                .long("bootnodes")
                .value_name("NODES")
                .takes_value(true),
        )
        .arg(
            Arg::with_name("netconf-dir")
                .help("Sets a custom directory for network configurations.")
                .long("netconf-dir")
                .value_name("DIR")
                .takes_value(true),
        )
        .arg(
            Arg::with_name("public-address")
                .help("Sets a custom public address to be connected by others.")
                .long("public-address")
                .value_name("IP ADDRESS")
                .takes_value(true),
        )
        .arg(
            Arg::with_name("net-key")
                .help("Sets a custom secret key to generate unique node ID.")
                .long("net-key")
                .value_name("KEY")
                .takes_value(true),
        )
        .arg(
            Arg::with_name("start-mining")
                .help("Start mining if set to true. Ensure that mining-author is set.")
                .long("start-mining")
                .value_name("BOOL")
                .takes_value(true),
        )
        .arg(
            Arg::with_name("mining-author")
                .help("Set the address to receive mining rewards.")
                .long("mining-author")
                .value_name("ADDRESS")
                .takes_value(true),
        )
        .arg(
            Arg::with_name("ledger-cache-size")
                .help("Sets the ledger cache size.")
                .short("lcs")
                .long("ledger-cache-size")
                .value_name("SIZE")
                .takes_value(true),
        )
        .arg(
            Arg::with_name("db-cache-size")
                .help("Sets the db cache size.")
                .short("dcs")
                .long("db-cache-size")
                .value_name("SIZE")
                .takes_value(true),
        )
        .arg(
            Arg::with_name("enable-discovery")
                .help("Enable discovery protocol.")
                .long("enable-discovery")
                .value_name("BOOL")
                .takes_value(true),
        )
        .arg(
            Arg::with_name("node-table-timeout-s")
                .help("How often Mazze updates its peer table (default 300).")
                .long("node-table-timeout-s")
                .value_name("SEC")
                .takes_value(true),
        )
        .arg(
            Arg::with_name("node-table-promotion-timeout-s")
                .help("How long Mazze waits for promoting a peer to trustworthy (default 3 * 24 * 3600).")
                .long("node-table-promotion-timeout-s")
                .value_name("SEC")
                .takes_value(true),
        )
        .arg(
            Arg::with_name("test-mode")
                .help("Sets test mode for adding latency")
                .long("test-mode")
                .value_name("BOOL")
                .takes_value(true),
        )
        .arg(
            Arg::with_name("db-compact-profile")
                .help("Sets the compaction profile of RocksDB.")
                .long("db-compact-profile")
                .value_name("ENUM")
                .takes_value(true),
        )
        .arg(
            Arg::with_name("block-db-dir")
                .help("Sets the root path of db.")
                .long("block-db-dir")
                .value_name("DIR")
                .takes_value(true),
        )
        .arg(
            Arg::with_name("load-test-chain")
                .help("Sets the test chain json file.")
                .long("load-test-chain")
                .value_name("FILE")
                .takes_value(true),
        )
        .arg(
            Arg::with_name("egress-queue-capacity")
                .help("Sets egress queue capacity of P2P network.")
                .long("egress-queue-capacity")
                .value_name("MB")
                .takes_value(true),
        )
        .arg(
            Arg::with_name("egress-min-throttle")
                .help("Sets minimum throttling queue size of egress.")
                .long("egress-min-throttle")
                .value_name("MB")
                .takes_value(true),
        )
        .arg(
            Arg::with_name("egress-max-throttle")
                .help("Sets maximum throttling queue size of egress.")
                .long("egress-max-throttle")
                .value_name("MB")
                .takes_value(true),
        )
        .arg(
            Arg::with_name("get-logs-epoch-batch-size")
                .help("Sets the size of the epoch batches used during log filtering.")
                .long("get-logs-epoch-batch-size")
                .value_name("SIZE")
                .takes_value(true),
        )
        .arg(
            Arg::with_name("get-logs-filter-max-epoch-range")
                .help("Sets the maximum number of allowed epochs during log filtering.")
                .long("get-logs-filter-max-epoch-range")
                .value_name("SIZE")
                .takes_value(true),
        )
        .arg(
            Arg::with_name("get-logs-filter-max-limit")
                .help("Sets the maximum number of log entries returned during log filtering.")
                .long("get-logs-filter-max-limit")
                .value_name("SIZE")
                .takes_value(true),
        )
        .arg(
            Arg::with_name("get-logs-filter-max-block-number-range")
                .help("Sets the maximum number of allowed blocks during log filtering.")
                .long("get-logs-filter-max-block-number-range")
                .value_name("SIZE")
                .takes_value(true),
        )
        .arg(
            Arg::with_name("account-provider-refresh-time-ms")
                .help("Sets the time after which accounts are re-read from disk.")
                .long("account-provider-refresh-time-ms")
                .value_name("MS")
                .takes_value(true),
        )
        .arg(
            Arg::with_name("dev-pos-private-key-encryption-password")
                .help("Sets the encryption password for the pos private key file. It's used to encrypt a new generated key or to decrypt an existing key file.")
                .long("dev-pos-private-key-encryption-password")
                .value_name("PASSWD")
                .takes_value(true),
        )
        .arg(
            Arg::with_name("pos-started-as-voter")
                .help("If true, the node will start PoS election and voting if it's available.")
                .long("pos-started-as-voter")
                .value_name("BOOL")
                .takes_value(true),
        )
        .arg(
            Arg::with_name("light")
                .long("light"),
        )
        .arg(
            Arg::with_name("archive")
                .long("archive"),
        )
        .arg(
            Arg::with_name("tg_archive")
                .long("tg_archive"),
        )
        .arg(
            Arg::with_name("full")
                .long("full"),
        )
        .arg(
            Arg::with_name("full-fast")
                .long("full-fast"),
        )
        .arg(
            Arg::with_name("mining")
                .long("mining"),
        )
        .subcommand(
            SubCommand::with_name("account")
                .about("Manage accounts")
                .setting(AppSettings::SubcommandRequiredElseHelp)
                .subcommand(
                    SubCommand::with_name("new")
                        .about("Create a new account (and its associated key) for the given --chain (default mazze).")
                        .arg(
                            Arg::with_name("keys-iterations")
                                .help("Specify the number of iterations to use when deriving key from the password (bigger is more secure).")
                                .long("keys-iterations")
                                .value_name("NUM")
                                .default_value("10240")
                                .takes_value(true),
                        )
                        .arg(
                            Arg::with_name("password")
                                .help("Provide a file containing a password for unlocking an account. Leading and trailing whitespace is trimmed.")
                                .long("password")
                                .value_name("FILE")
                                .takes_value(true),
                        ),
                )
                .subcommand(
                    SubCommand::with_name("list")
                        .about("List existing accounts of the given --chain (default mazze)."),
                )
                .subcommand(
                    SubCommand::with_name("import")
                        .about("Import accounts from JSON UTC keystore files to the specified --chain (default mazze)")
                        .arg(
                            Arg::with_name("import-path")
                                .help("A list of file paths to import.")
                                .long("import-path")
                                .multiple(true)
                                .value_name("PATH")
                                .takes_value(true)
                                .required(true),
                        ),
                ),
        )
        .subcommand(
            SubCommand::with_name("rpc")
                .about("RPC based subcommands to query blockchain information and send transactions")
                .setting(AppSettings::SubcommandRequiredElseHelp)
                .arg(
                    Arg::with_name("url")
                        .help("URL of RPC server")
                        .long("url")
                        .takes_value(true)
                        .default_value("http://localhost:12539")
                        .global(true),
                )
                .subcommand(
                    SubCommand::with_name("price")
                        .about("Get recent mean gas price")
                        .arg(
                            Arg::with_name("rpc-method")
                                .default_value("mazze_gasPrice")
                                .hidden(true),
                        ),
                )
                .subcommand(
                    SubCommand::with_name("epoch")
                        .about("Get epoch number")
                        .arg(
                            Arg::with_name("rpc-method")
                                .default_value("mazze_epochNumber")
                                .hidden(true),
                        )
                        .arg(
                            Arg::with_name("rpc-args")
                                .multiple(true)
                                .use_delimiter(true)
                                .default_value("epoch")
                                .hidden(true),
                        )
                        .arg(
                            Arg::with_name("epoch")
                                .help("Epoch (latest_mined, latest_state, earliest or epoch number in HEX format)")
                                .long("epoch")
                                .takes_value(true),
                        ),
                )
                .subcommand(
                    SubCommand::with_name("balance")
                        .about("Get balance of specified account")
                        .arg(
                            Arg::with_name("rpc-method")
                                .default_value("mazze_getBalance")
                                .hidden(true),
                        )
                        .arg(
                            Arg::with_name("rpc-args")
                                .multiple(true)
                                .use_delimiter(true)
                                .default_value("address,epoch")
                                .hidden(true),
                        )
                        .arg(
                            Arg::with_name("address")
                                .help("Account address")
                                .long("address")
                                .required(true)
                                .takes_value(true),
                        )
                        .arg(
                            Arg::with_name("epoch")
                                .help("Epoch (latest_mined, latest_state, earliest or epoch number in HEX format)")
                                .long("epoch")
                                .takes_value(true),
                        ),
                )
                .subcommand(
                    SubCommand::with_name("code")
                        .about("Get bytecode of specified contract")
                        .arg(
                            Arg::with_name("rpc-method")
                                .default_value("mazze_getCode")
                                .hidden(true),
                        )
                        .arg(
                            Arg::with_name("rpc-args")
                                .multiple(true)
                                .use_delimiter(true)
                                .default_value("address,epoch")
                                .hidden(true),
                        )
                        .arg(
                            Arg::with_name("address")
                                .help("Contract address")
                                .long("address")
                                .required(true)
                                .takes_value(true),
                        )
                        .arg(
                            Arg::with_name("epoch")
                                .help("Epoch (latest_mined, latest_state, earliest or epoch number in HEX format)")
                                .long("epoch")
                                .takes_value(true),
                        ),
                )
                .subcommand(
                    SubCommand::with_name("block-by-hash")
                        .about("Get block by hash")
                        .arg(
                            Arg::with_name("rpc-method")
                                .default_value("mazze_getBlockByHash")
                                .hidden(true),
                        )
                        .arg(
                            Arg::with_name("rpc-args")
                                .multiple(true)
                                .use_delimiter(true)
                                .default_value("hash,include-txs:bool")
                                .hidden(true),
                        )
                        .arg(
                            Arg::with_name("hash")
                                .help("Block hash")
                                .long("hash")
                                .required(true)
                                .takes_value(true),
                        )
                        .arg(
                            Arg::with_name("include-txs")
                                .help("Whether to return detailed transactions in block")
                                .long("include-txs"),
                        ),
                )
                .subcommand(
                    SubCommand::with_name("block-with-assumption")
                        .about("Get block by hash with main chain assumption")
                        .arg(
                            Arg::with_name("rpc-method")
                                .default_value("mazze_getBlockByHashWithMainAssumption")
                                .hidden(true),
                        )
                        .arg(
                            Arg::with_name("rpc-args")
                                .multiple(true)
                                .use_delimiter(true)
                                .default_value("block-hash,main-hash,epoch-number:u64")
                                .hidden(true),
                        )
                        .arg(
                            Arg::with_name("block-hash")
                                .help("Block hash")
                                .long("block-hash")
                                .required(true)
                                .takes_value(true)
                                .value_name("HASH"),
                        )
                        .arg(
                            Arg::with_name("main-hash")
                                .help("Main block hash")
                                .long("main-hash")
                                .required(true)
                                .takes_value(true)
                                .value_name("HASH"),
                        )
                        .arg(
                            Arg::with_name("epoch-number")
                                .help("Epoch number")
                                .long("epoch-number")
                                .required(true)
                                .takes_value(true)
                                .value_name("NUMBER"),
                        ),
                )
                .subcommand(
                    SubCommand::with_name("block-by-epoch")
                        .about("Get block by epoch")
                        .arg(
                            Arg::with_name("rpc-method")
                                .default_value("mazze_getBlockByEpochNumber")
                                .hidden(true),
                        )
                        .arg(
                            Arg::with_name("rpc-args")
                                .multiple(true)
                                .use_delimiter(true)
                                .default_value("epoch,include-txs:bool")
                                .hidden(true),
                        )
                        .arg(
                            Arg::with_name("epoch")
                                .help("Epoch (latest_mined, latest_state, earliest or epoch number in HEX format)")
                                .long("epoch")
                                .required(true)
                                .takes_value(true),
                        )
                        .arg(
                            Arg::with_name("include-txs")
                                .help("Whether to return detailed transactions in block")
                                .long("include-txs"),
                        ),
                )
                .subcommand(
                    SubCommand::with_name("best-block-hash")
                        .about("Get the best block hash")
                        .arg(
                            Arg::with_name("rpc-method")
                                .default_value("mazze_getBestBlockHash")
                                .hidden(true),
                        ),
                )
                .subcommand(
                    SubCommand::with_name("nonce")
                        .about("Get nonce of specified account")
                        .arg(
                            Arg::with_name("rpc-method")
                                .default_value("mazze_getNextNonce")
                                .hidden(true),
                        )
                        .arg(
                            Arg::with_name("rpc-args")
                                .multiple(true)
                                .use_delimiter(true)
                                .default_value("address,epoch")
                                .hidden(true),
                        )
                        .arg(
                            Arg::with_name("address")
                                .help("Account address")
                                .long("address")
                                .required(true)
                                .takes_value(true),
                        )
                        .arg(
                            Arg::with_name("epoch")
                                .help("Epoch (latest_mined, latest_state, earliest or epoch number in HEX format)")
                                .long("epoch")
                                .takes_value(true),
                        ),
                )
                .subcommand(
                    SubCommand::with_name("send")
                        .about("Send a signed transaction and return its hash")
                        .arg(
                            Arg::with_name("rpc-method")
                                .default_value("mazze_sendRawTransaction")
                                .hidden(true),
                        )
                        .arg(
                            Arg::with_name("rpc-args")
                                .multiple(true)
                                .use_delimiter(true)
                                .default_value("raw-bytes")
                                .hidden(true),
                        )
                        .arg(
                            Arg::with_name("raw-bytes")
                                .help("Signed transaction data")
                                .long("raw-bytes")
                                .required(true)
                                .takes_value(true)
                                .value_name("HEX"),
                        ),
                )
                .subcommand(
                    SubCommand::with_name("tx")
                        .about("Get transaction by hash")
                        .arg(
                            Arg::with_name("rpc-method")
                                .default_value("mazze_getTransactionByHash")
                                .hidden(true),
                        )
                        .arg(
                            Arg::with_name("rpc-args")
                                .multiple(true)
                                .use_delimiter(true)
                                .default_value("hash")
                                .hidden(true),
                        )
                        .arg(
                            Arg::with_name("hash")
                                .help("Transaction hash")
                                .long("hash")
                                .required(true)
                                .takes_value(true),
                        ),
                )
                .subcommand(
                    SubCommand::with_name("blocks")
                        .about("Get blocks of specified epoch")
                        .arg(
                            Arg::with_name("rpc-method")
                                .default_value("mazze_getBlocksByEpoch")
                                .hidden(true),
                        )
                        .arg(
                            Arg::with_name("rpc-args")
                                .multiple(true)
                                .use_delimiter(true)
                                .default_value("epoch")
                                .hidden(true),
                        )
                        .arg(
                            Arg::with_name("epoch")
                                .help("Epoch (latest_mined, latest_state, earliest or epoch number in HEX format)")
                                .long("epoch")
                                .required(true)
                                .takes_value(true),
                        ),
                )
                .subcommand(
                    SubCommand::with_name("skipped-blocks")
                        .about("Get skipped blocks of specified epoch")
                        .arg(
                            Arg::with_name("rpc-method")
                                .default_value("mazze_getSkippedBlocksByEpoch")
                                .hidden(true),
                        )
                        .arg(
                            Arg::with_name("rpc-args")
                                .multiple(true)
                                .use_delimiter(true)
                                .default_value("epoch")
                                .hidden(true),
                        )
                        .arg(
                            Arg::with_name("epoch")
                                .help("Epoch (latest_mined, latest_state, earliest or epoch number in HEX format)")
                                .long("epoch")
                                .required(true)
                                .takes_value(true),
                        ),
                )
                .subcommand(
                    SubCommand::with_name("receipt")
                        .about("Get receipt by transaction hash")
                        .arg(
                            Arg::with_name("rpc-method")
                                .default_value("mazze_getTransactionReceipt")
                                .hidden(true),
                        )
                        .arg(
                            Arg::with_name("rpc-args")
                                .multiple(true)
                                .use_delimiter(true)
                                .default_value("hash")
                                .hidden(true),
                        )
                        .arg(
                            Arg::with_name("hash")
                                .help("Transaction hash")
                                .long("hash")
                                .required(true)
                                .takes_value(true),
                        ),
                )
                .subcommand(
                    SubCommand::with_name("call")
                        .about("Executes a new message call immediately without creating a transaction")
                        .arg(
                            Arg::with_name("rpc-method")
                                .default_value("mazze_call")
                                .hidden(true),
                        )
                        .arg(
                            Arg::with_name("rpc-args")
                                .multiple(true)
                                .use_delimiter(true)
                                .default_value("tx:map(from;to;gasPrice;gas;value;data;nonce),epoch")
                                .hidden(true),
                        )
                        .arg(
                            Arg::with_name("from")
                                .help("Transaction from address")
                                .long("from")
                                .takes_value(true)
                                .value_name("ADDRESS"),
                        )
                        .arg(
                            Arg::with_name("to")
                                .help("Transaction to address")
                                .long("to")
                                .takes_value(true)
                                .value_name("ADDRESS"),
                        )
                        .arg(
                            Arg::with_name("gasPrice")
                                .help("Transaction gas price")
                                .long("gas-price")
                                .takes_value(true)
                                .value_name("HEX"),
                        )
                        .arg(
                            Arg::with_name("gas")
                                .help("Gas provided for transaction execution")
                                .long("gas")
                                .takes_value(true)
                                .value_name("HEX"),
                        )
                        .arg(
                            Arg::with_name("value")
                                .help("value sent with this transaction")
                                .long("value")
                                .takes_value(true)
                                .value_name("HEX"),
                        )
                        .arg(
                            Arg::with_name("data")
                                .help("Hash of the method signature and encoded parameters")
                                .long("data")
                                .takes_value(true)
                                .value_name("HEX"),
                        )
                        .arg(
                            Arg::with_name("nonce")
                                .help("Transaction nonce")
                                .long("nonce")
                                .takes_value(true)
                                .value_name("HEX"),
                        )
                        .arg(
                            Arg::with_name("epoch")
                                .help("Epoch (latest_mined, latest_state, earliest or epoch number in HEX format)")
                                .long("epoch")
                                .takes_value(true)
                                .value_name("EPOCH"),
                        ),
                )
                .subcommand(
                    SubCommand::with_name("estimate-gas")
                        .about("Executes a call request immediately without creating a transaction and returns the gas used")
                        .arg(
                            Arg::with_name("rpc-method")
                                .default_value("mazze_estimateGas")
                                .hidden(true),
                        )
                        .arg(
                            Arg::with_name("rpc-args")
                                .multiple(true)
                                .use_delimiter(true)
                                .default_value("tx:map(from;to;gasPrice;gas;value;data;nonce),epoch")
                                .hidden(true),
                        )
                        .arg(
                            Arg::with_name("from")
                                .help("Transaction from address")
                                .long("from")
                                .takes_value(true)
                                .value_name("ADDRESS"),
                        )
                        .arg(
                            Arg::with_name("to")
                                .help("Transaction to address")
                                .long("to")
                                .takes_value(true)
                                .value_name("ADDRESS"),
                        )
                        .arg(
                            Arg::with_name("gasPrice")
                                .help("Transaction gas price")
                                .long("gas-price")
                                .takes_value(true)
                                .value_name("HEX"),
                        )
                        .arg(
                            Arg::with_name("gas")
                                .help("Gas provided for transaction execution")
                                .long("gas")
                                .takes_value(true)
                                .value_name("HEX"),
                        )
                        .arg(
                            Arg::with_name("value")
                                .help("value sent with this transaction")
                                .long("value")
                                .takes_value(true)
                                .value_name("HEX"),
                        )
                        .arg(
                            Arg::with_name("data")
                                .help("Hash of the method signature and encoded parameters")
                                .long("data")
                                .takes_value(true)
                                .value_name("HEX"),
                        )
                        .arg(
                            Arg::with_name("nonce")
                                .help("Transaction nonce")
                                .long("nonce")
                                .takes_value(true)
                                .value_name("HEX"),
                        )
                        .arg(
                            Arg::with_name("epoch")
                                .help("Epoch (latest_mined, latest_state, earliest or epoch number in HEX format)")
                                .long("epoch")
                                .takes_value(true)
                                .value_name("EPOCH"),
                        ),
                )
                .subcommand(
                    SubCommand::with_name("local")
                        .about("Local subcommands (requires jsonrpc_local_http_port configured)")
                        .setting(AppSettings::SubcommandRequiredElseHelp)
                        .subcommand(
                            SubCommand::with_name("send")
                                .about("Send a transaction and return its hash")
                                .arg(
                                    Arg::with_name("rpc-method")
                                        .default_value("mazze_sendTransaction")
                                        .hidden(true),
                                )
                                .arg(
                                    Arg::with_name("rpc-args")
                                        .multiple(true)
                                        .use_delimiter(true)
                                        .default_value("tx:map(from;to;gasPrice;gas;value;data;nonce;storageLimit),password:password")
                                        .hidden(true),
                                )
                                .arg(
                                    Arg::with_name("from")
                                        .help("Transaction from address")
                                        .long("from")
                                        .required(true)
                                        .takes_value(true)
                                        .value_name("ADDRESS"),
                                )
                                .arg(
                                    Arg::with_name("to")
                                        .help("Transaction to address (empty to create contract)")
                                        .long("to")
                                        .takes_value(true)
                                        .value_name("ADDRESS"),
                                )
                                .arg(
                                    Arg::with_name("gasPrice")
                                        .help("Transaction gas price")
                                        .long("gas-price")
                                        .takes_value(true)
                                        .value_name("HEX")
                                        .default_value("0x2540BE400"),
                                )
                                .arg(
                                    Arg::with_name("gas")
                                        .help("Gas provided for transaction execution")
                                        .long("gas")
                                        .takes_value(true)
                                        .value_name("HEX")
                                        .default_value("0x5208"),
                                )
                                .arg(
                                    Arg::with_name("value")
                                        .help("value sent with this transaction")
                                        .long("value")
                                        .required(true)
                                        .takes_value(true)
                                        .value_name("HEX"),
                                )
                                .arg(
                                    Arg::with_name("data")
                                        .help("Hash of the method signature and encoded parameters")
                                        .long("data")
                                        .takes_value(true)
                                        .value_name("HEX"),
                                )
                                .arg(
                                    Arg::with_name("nonce")
                                        .help("Transaction nonce")
                                        .long("nonce")
                                        .takes_value(true)
                                        .value_name("HEX"),
                                )
                                .arg(
                                    Arg::with_name("storageLimit")
                                        .help("Storage limit for the transaction")
                                        .long("storage-limit")
                                        .takes_value(true)
                                        .value_name("HEX")
                                        .default_value("0x0"),
                                ),
                        )
                        .subcommand(
                            SubCommand::with_name("account")
                                .about("Account related subcommands")
                                .setting(AppSettings::SubcommandRequiredElseHelp)
                                .subcommand(
                                    SubCommand::with_name("list")
                                        .about("List all accounts")
                                        .arg(
                                            Arg::with_name("rpc-method")
                                                .default_value("accounts")
                                                .hidden(true),
                                        ),
                                )
                                .subcommand(
                                    SubCommand::with_name("new")
                                        .about("Create a new account")
                                        .arg(
                                            Arg::with_name("rpc-method")
                                                .default_value("new_account")
                                                .hidden(true),
                                        )
                                        .arg(
                                            Arg::with_name("rpc-args")
                                                .multiple(true)
                                                .use_delimiter(true)
                                                .default_value("password:password2")
                                                .hidden(true),
                                        ),
                                )
                                .subcommand(
                                    SubCommand::with_name("unlock")
                                        .about("Unlock an account")
                                        .arg(
                                            Arg::with_name("rpc-method")
                                                .default_value("unlock_account")
                                                .hidden(true),
                                        )
                                        .arg(
                                            Arg::with_name("rpc-args")
                                                .multiple(true)
                                                .use_delimiter(true)
                                                .default_value("address,password:password,duration")
                                                .hidden(true),
                                        )
                                        .arg(
                                            Arg::with_name("address")
                                                .help("Address of the account")
                                                .long("address")
                                                .required(true)
                                                .takes_value(true),
                                        )
                                        .arg(
                                            Arg::with_name("duration")
                                                .help("Duration to unlock the account, use 0x0 to unlock permanently (strongly not recommended!).")
                                                .long("duration")
                                                .default_value("0x3c")
                                                .takes_value(true),
                                        ),
                                )
                                .subcommand(
                                    SubCommand::with_name("lock")
                                        .about("Lock an unlocked account")
                                        .arg(
                                            Arg::with_name("rpc-method")
                                                .default_value("lock_account")
                                                .hidden(true),
                                        )
                                        .arg(
                                            Arg::with_name("rpc-args")
                                                .multiple(true)
                                                .use_delimiter(true)
                                                .default_value("address")
                                                .hidden(true),
                                        )
                                        .arg(
                                            Arg::with_name("address")
                                                .help("Address of the account")
                                                .long("address")
                                                .required(true)
                                                .takes_value(true),
                                        ),
                                ),
                        )
                        .subcommand(
                            SubCommand::with_name("txpool")
                                .about("Transaction pool subcommands")
                                .setting(AppSettings::SubcommandRequiredElseHelp)
                                .subcommand(
                                    SubCommand::with_name("status")
                                        .about("Get the number of transactions for different status")
                                        .arg(
                                            Arg::with_name("rpc-method")
                                                .default_value("txpool_status")
                                                .hidden(true),
                                        ),
                                )
                                .subcommand(
                                    SubCommand::with_name("inspect-one")
                                        .about("Get the detailed status of specified transaction")
                                        .arg(
                                            Arg::with_name("rpc-method")
                                                .default_value("txpool_txWithPoolInfo")
                                                .hidden(true),
                                        )
                                        .arg(
                                            Arg::with_name("rpc-args")
                                                .multiple(true)
                                                .use_delimiter(true)
                                                .default_value("hash")
                                                .hidden(true),
                                        )
                                        .arg(
                                            Arg::with_name("hash")
                                                .help("Transaction hash")
                                                .long("hash")
                                                .required(true)
                                                .takes_value(true),
                                        ),
                                )
                                .subcommand(
                                    SubCommand::with_name("inspect")
                                        .about("List textual summary of all transactions")
                                        .arg(
                                            Arg::with_name("rpc-method")
                                                .default_value("txpool_inspect")
                                                .hidden(true),
                                        )
                                        .arg(
                                            Arg::with_name("rpc-args")
                                                .multiple(true)
                                                .use_delimiter(true)
                                                .default_value("address")
                                                .hidden(true),
                                        )
                                        .arg(
                                            Arg::with_name("address")
                                                .help("Account address")
                                                .long("address")
                                                .required(false)
                                                .takes_value(true),
                                        ),
                                )
                                .subcommand(
                                    SubCommand::with_name("content")
                                        .about("List exact details of all transactions")
                                        .arg(
                                            Arg::with_name("rpc-method")
                                                .default_value("txpool_content")
                                                .hidden(true),
                                        )
                                        .arg(
                                            Arg::with_name("rpc-args")
                                                .multiple(true)
                                                .use_delimiter(true)
                                                .default_value("address")
                                                .hidden(true),
                                        )
                                        .arg(
                                            Arg::with_name("address")
                                                .help("Account address")
                                                .long("address")
                                                .required(false)
                                                .takes_value(true),
                                        ),
                                )
                                .subcommand(
                                    SubCommand::with_name("clear")
                                        .about("Remove all transactions")
                                        .arg(
                                            Arg::with_name("rpc-method")
                                                .default_value("txpool_clear")
                                                .hidden(true),
                                        ),
                                ),
                        )
                        .subcommand(
                            SubCommand::with_name("net")
                                .about("Network subcommands")
                                .setting(AppSettings::SubcommandRequiredElseHelp)
                                .subcommand(
                                    SubCommand::with_name("throttling")
                                        .about("Get the current throttling information")
                                        .arg(
                                            Arg::with_name("rpc-method")
                                                .default_value("net_throttling")
                                                .hidden(true),
                                        ),
                                )
                                .subcommand(
                                    SubCommand::with_name("node")
                                        .about("Get node information by ID")
                                        .arg(
                                            Arg::with_name("rpc-method")
                                                .default_value("net_node")
                                                .hidden(true),
                                        )
                                        .arg(
                                            Arg::with_name("rpc-args")
                                                .multiple(true)
                                                .use_delimiter(true)
                                                .default_value("id")
                                                .hidden(true),
                                        )
                                        .arg(
                                            Arg::with_name("id")
                                                .help("Node ID")
                                                .long("id")
                                                .required(true)
                                                .takes_value(true),
                                        ),
                                )
                                .subcommand(
                                    SubCommand::with_name("disconnect")
                                        .about("Disconnect a node")
                                        .arg(
                                            Arg::with_name("rpc-method")
                                                .default_value("net_disconnect_node")
                                                .hidden(true),
                                        )
                                        .arg(
                                            Arg::with_name("rpc-args")
                                                .multiple(true)
                                                .use_delimiter(true)
                                                .default_value("id,operation")
                                                .hidden(true),
                                        )
                                        .arg(
                                            Arg::with_name("id")
                                                .help("Node ID")
                                                .long("id")
                                                .required(true)
                                                .takes_value(true),
                                        )
                                        .arg(
                                            Arg::with_name("operation")
                                                .help("Operation to update node database")
                                                .long("operation")
                                                .takes_value(true)
                                                .possible_value("Failure")
                                                .possible_value("Demotion")
                                                .possible_value("Remove"),
                                        ),
                                )
                                .subcommand(
                                    SubCommand::with_name("session")
                                        .about("Get active session(s)")
                                        .arg(
                                            Arg::with_name("rpc-method")
                                                .default_value("net_sessions")
                                                .hidden(true),
                                        )
                                        .arg(
                                            Arg::with_name("rpc-args")
                                                .multiple(true)
                                                .use_delimiter(true)
                                                .default_value("id")
                                                .hidden(true),
                                        )
                                        .arg(
                                            Arg::with_name("id")
                                                .help("Node ID")
                                                .long("id")
                                                .takes_value(true),
                                        ),
                                ),
                        )
                        .subcommand(
                            SubCommand::with_name("sync-phase")
                                .about("Get the current synchronization phase")
                                .arg(
                                    Arg::with_name("rpc-method")
                                        .default_value("current_sync_phase")
                                        .hidden(true),
                                ),
                        )
                        .subcommand(
                            SubCommand::with_name("consensus-graph-state")
                                .about("Get the consensus graph state")
                                .arg(
                                    Arg::with_name("rpc-method")
                                        .default_value("consensus_graph_state")
                                        .hidden(true),
                                ),
                        )
                        .subcommand(
                            SubCommand::with_name("test")
                                .about("Test subcommands (used for test purpose only)")
                                .setting(AppSettings::SubcommandRequiredElseHelp)
                                .subcommand(
                                    SubCommand::with_name("block-count")
                                        .about("Get the total block count")
                                        .arg(
                                            Arg::with_name("rpc-method")
                                                .default_value("getblockcount")
                                                .hidden(true),
                                        ),
                                )
                                .subcommand(
                                    SubCommand::with_name("goodput")
                                        .about("Get the recent transaction good TPS")
                                        .arg(
                                            Arg::with_name("rpc-method")
                                                .default_value("getgoodput")
                                                .hidden(true),
                                        ),
                                )
                                .subcommand(
                                    SubCommand::with_name("chain")
                                        .about("List \"ALL\" blocks in topological order")
                                        .arg(
                                            Arg::with_name("rpc-method")
                                                .default_value("mazze_getChain")
                                                .hidden(true),
                                        ),
                                )
                                .subcommand(
                                    SubCommand::with_name("stop")
                                        .about("Stop the mazze program")
                                        .arg(
                                            Arg::with_name("rpc-method")
                                                .default_value("stop")
                                                .hidden(true),
                                        ),
                                )
                                .subcommand(
                                    SubCommand::with_name("status")
                                        .about("Get the current status of mazze")
                                        .arg(
                                            Arg::with_name("rpc-method")
                                                .default_value("mazze_getStatus")
                                                .hidden(true),
                                        ),
                                ),
                        )
                        .subcommand(
                            SubCommand::with_name("snapshot")
                                .about("Snapshot management helpers")
                                .setting(AppSettings::SubcommandRequiredElseHelp)
                                .subcommand(
                                    SubCommand::with_name("export")
                                        .about("Export a snapshot bundle to a local directory")
                                        .arg(
                                            Arg::with_name("path")
                                                .help("Destination directory for the exported bundle")
                                                .long("path")
                                                .takes_value(true)
                                                .required(true),
                                        )
                                        .arg(
                                            Arg::with_name("include-delta")
                                                .help("Include delta MPT data in the bundle")
                                                .long("include-delta")
                                                .takes_value(false),
                                        ),
                                )
                                .subcommand(
                                    SubCommand::with_name("import")
                                        .about("Import a snapshot bundle from a directory")
                                        .arg(
                                            Arg::with_name("path")
                                                .help("Directory containing the snapshot bundle to import")
                                                .long("path")
                                                .takes_value(true)
                                                .required(true),
                                        )
                                        .arg(
                                            Arg::with_name("force")
                                                .help("Overwrite existing snapshot data if present")
                                                .long("force")
                                                .takes_value(false),
                                        ),
                                ),
                        ),
                ),
        )
}
