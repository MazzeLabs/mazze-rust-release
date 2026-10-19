// Copyright 2024 Mazze Foundation. All rights reserved.
// Mazze is free software and distributed under GNU General Public License.
// See http://www.gnu.org/licenses/

use std::{
    collections::HashSet,
    fmt::{Display, Formatter},
    str::FromStr,
};

#[derive(Debug, PartialEq, Clone, Eq, Hash)]
pub enum Api {
    Mazze,
    Eth,
    Debug, // core space parity style debug
    Pubsub,
    Test,
    Trace,
    TxPool,
    EthPubsub,
    EthDebug,
}

impl FromStr for Api {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        use self::Api::*;
        match s {
            "mazze" => Ok(Mazze),
            "eth" => Ok(Eth),
            "debug" => Ok(Debug),
            "pubsub" => Ok(Pubsub),
            // `test` exposes state-mutation RPCs (e.g. `test_setChainParams`).
            // Compiled out of release builds entirely — operators who
            // include "test" in `public_rpc_apis` on a release build get
            // an explicit error rather than a silently-enabled namespace.
            // See docs/security-audit.md finding C-2.
            #[cfg(any(test, debug_assertions, feature = "test-rpc"))]
            "test" => Ok(Test),
            #[cfg(not(any(test, debug_assertions, feature = "test-rpc")))]
            "test" => Err(
                "`test` API is not available in release builds; rebuild with `--features test-rpc` if you really mean this"
                    .into(),
            ),
            "trace" => Ok(Trace),
            "txpool" => Ok(TxPool),
            "ethpubsub" => Ok(EthPubsub),
            "ethdebug" => Ok(EthDebug),
            _ => Err("Unknown api type".into()),
        }
    }
}

impl Display for Api {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Api::Mazze => write!(f, "mazze"),
            Api::Eth => write!(f, "eth"),
            Api::Debug => write!(f, "debug"),
            Api::Pubsub => write!(f, "pubsub"),
            Api::Test => write!(f, "test"),
            Api::Trace => write!(f, "trace"),
            Api::TxPool => write!(f, "txpool"),
            Api::EthPubsub => write!(f, "ethpubsub"),
            Api::EthDebug => write!(f, "ethdebug"),
        }
    }
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub enum ApiSet {
    All, // core space all apis
    Safe,
    Evm, // Ethereum api set
    List(HashSet<Api>),
}

impl ApiSet {
    pub fn list_apis(&self) -> HashSet<Api> {
        match *self {
            ApiSet::List(ref apis) => apis.clone(),
            // `Api::Test` is included only on test/debug/feature-gated
            // release builds. See docs/security-audit.md finding C-2.
            ApiSet::All => {
                let mut s: HashSet<Api> = [
                    Api::Mazze,
                    Api::Debug,
                    Api::Pubsub,
                    Api::Trace,
                    Api::TxPool,
                ]
                .iter()
                .cloned()
                .collect();
                #[cfg(any(test, debug_assertions, feature = "test-rpc"))]
                {
                    s.insert(Api::Test);
                }
                s
            }
            ApiSet::Safe => [Api::Mazze, Api::Pubsub, Api::TxPool]
                .iter()
                .cloned()
                .collect(),
            ApiSet::Evm => [Api::Eth, Api::EthPubsub].iter().cloned().collect(),
        }
    }
}

impl Default for ApiSet {
    fn default() -> Self {
        ApiSet::Safe
    }
}

impl FromStr for ApiSet {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let mut apis = HashSet::new();

        for api in s.split(',') {
            match api {
                "all" => {
                    apis.extend(ApiSet::All.list_apis());
                }
                "safe" => {
                    // Safe APIs are those that are safe even in UnsafeContext.
                    apis.extend(ApiSet::Safe.list_apis());
                }
                "evm" => {
                    apis.extend(ApiSet::Evm.list_apis());
                }
                // Remove the API
                api if api.starts_with("-") => {
                    let api = api[1..].parse()?;
                    apis.remove(&api);
                }
                api => {
                    let api = api.parse()?;
                    apis.insert(api);
                }
            }
        }

        Ok(ApiSet::List(apis))
    }
}
