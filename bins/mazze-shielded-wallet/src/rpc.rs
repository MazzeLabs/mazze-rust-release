//! A blocking JSON-RPC client for the node.

use std::time::Duration;

use jsonrpsee::{
    core::{client::ClientT, params::ArrayParams},
    http_client::{HttpClient, HttpClientBuilder},
};
use serde_json::Value;

pub struct Node {
    client: HttpClient,
    runtime: tokio::runtime::Runtime,
}

impl Node {
    pub fn connect(url: &str) -> Result<Self, String> {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|e| e.to_string())?;
        let client = HttpClientBuilder::default()
            .max_request_size(4 * 1024 * 1024)
            .max_response_size(64 * 1024 * 1024)
            .request_timeout(Duration::from_secs(120))
            .build(url)
            .map_err(|e| e.to_string())?;
        Ok(Node { client, runtime })
    }

    pub fn call(&self, method: &str, params: Vec<Value>) -> Result<Value, String> {
        let mut array = ArrayParams::new();
        for p in params {
            array.insert(p).map_err(|e| e.to_string())?;
        }
        self.runtime
            .block_on(self.client.request::<Value, _>(method, array))
            .map_err(|e| format!("{method}: {e}"))
    }
}

pub fn hex0x(bytes: &[u8]) -> Value { Value::String(format!("0x{}", hex::encode(bytes))) }

pub fn unhex(v: &Value) -> Result<Vec<u8>, String> {
    let s = v.as_str().ok_or("expected a hex string")?;
    hex::decode(s.trim_start_matches("0x")).map_err(|e| e.to_string())
}

pub fn as_u256(v: &Value) -> Result<mazze_types::U256, String> {
    let s = v.as_str().ok_or_else(|| format!("expected a quantity, got {v}"))?;
    let hex = s.trim_start_matches("0x");
    if hex.is_empty() || hex.len() > 64 {
        return Err(format!("bad quantity {s}"));
    }
    hex.parse().map_err(|e| format!("bad quantity {s}: {e:?}"))
}

pub fn as_u64(v: &Value) -> Result<u64, String> {
    match v {
        Value::String(s) => u64::from_str_radix(s.trim_start_matches("0x"), 16)
            .map_err(|e| e.to_string()),
        Value::Number(n) => n.as_u64().ok_or_else(|| "not a u64".into()),
        _ => Err(format!("expected a quantity, got {v}")),
    }
}
