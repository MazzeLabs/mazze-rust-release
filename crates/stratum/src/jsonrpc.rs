// Copyright 2024 Mazze Foundation. All rights reserved.
// Mazze is free software and distributed under GNU General Public License.
// See http://www.gnu.org/licenses/

// The request/response types below reproduce the wire format of
// `jsonrpc-core` 15 (Parity Technologies, MIT licensed), which the stratum
// server used before: JSON-RPC 1.0 and 2.0 requests are both accepted, a
// response carries `"jsonrpc":"2.0"` only when the request did.

//! JSON-RPC 1.0 / 2.0 wire types of the stratum protocol.

use mazze_rpc_error::{Error, ErrorCode, Value};
use serde::{
    de::{self, Visitor},
    Deserialize, Deserializer, Serialize, Serializer,
};
use std::fmt;

/// Protocol version (`"2.0"`; absent for JSON-RPC 1.0).
#[derive(Debug, PartialEq, Clone, Copy, Hash, Eq)]
pub enum Version {
    V2,
}

impl Serialize for Version {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where S: Serializer {
        match *self {
            Version::V2 => serializer.serialize_str("2.0"),
        }
    }
}

impl<'a> Deserialize<'a> for Version {
    fn deserialize<D>(deserializer: D) -> Result<Version, D::Error>
    where D: Deserializer<'a> {
        deserializer.deserialize_identifier(VersionVisitor)
    }
}

struct VersionVisitor;

impl<'a> Visitor<'a> for VersionVisitor {
    type Value = Version;

    fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
        formatter.write_str("a string")
    }

    fn visit_str<E>(self, value: &str) -> Result<Self::Value, E>
    where E: de::Error {
        match value {
            "2.0" => Ok(Version::V2),
            _ => Err(de::Error::custom("invalid version")),
        }
    }
}

/// Request id.
#[derive(Debug, PartialEq, Clone, Hash, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
#[serde(untagged)]
pub enum Id {
    Null,
    Num(u64),
    Str(String),
}

/// Request parameters.
#[derive(Debug, PartialEq, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[serde(untagged)]
pub enum Params {
    None,
    Array(Vec<Value>),
    Map(serde_json::Map<String, Value>),
}

impl Params {
    /// Parses the parameters into the expected type.
    pub fn parse<D>(self) -> Result<D, Error>
    where D: serde::de::DeserializeOwned {
        let value: Value = match self {
            Params::Array(vec) => Value::Array(vec),
            Params::Map(map) => Value::Object(map),
            Params::None => Value::Null,
        };
        serde_json::from_value(value).map_err(|e| {
            Error::invalid_params(format!("Invalid params: {}.", e))
        })
    }
}

fn default_params() -> Params { Params::None }

fn default_id() -> Id { Id::Null }

#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct MethodCall {
    pub jsonrpc: Option<Version>,
    pub method: String,
    #[serde(default = "default_params")]
    pub params: Params,
    pub id: Id,
}

#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Notification {
    pub jsonrpc: Option<Version>,
    pub method: String,
    #[serde(default = "default_params")]
    pub params: Params,
}

#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
#[serde(untagged)]
pub enum Call {
    MethodCall(MethodCall),
    Notification(Notification),
    Invalid {
        #[serde(default = "default_id")]
        id: Id,
    },
}

#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
#[serde(untagged)]
pub enum Request {
    Single(Call),
    Batch(Vec<Call>),
}

#[derive(Debug, PartialEq, Clone, Serialize)]
pub struct Success {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub jsonrpc: Option<Version>,
    pub result: Value,
    pub id: Id,
}

#[derive(Debug, PartialEq, Clone, Serialize)]
pub struct Failure {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub jsonrpc: Option<Version>,
    pub error: Error,
    pub id: Id,
}

#[derive(Debug, PartialEq, Clone, Serialize)]
#[serde(untagged)]
pub enum Output {
    Success(Success),
    Failure(Failure),
}

impl Output {
    pub fn from(
        result: Result<Value, Error>, id: Id, jsonrpc: Option<Version>,
    ) -> Self {
        match result {
            Ok(result) => Output::Success(Success {
                id,
                jsonrpc,
                result,
            }),
            Err(error) => Output::Failure(Failure { id, jsonrpc, error }),
        }
    }

    pub fn invalid_request(id: Id, jsonrpc: Option<Version>) -> Self {
        Output::Failure(Failure {
            id,
            jsonrpc,
            error: Error::new(ErrorCode::InvalidRequest),
        })
    }
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(untagged)]
pub enum Response {
    Single(Output),
    Batch(Vec<Output>),
}

/// Methods of a JSON-RPC 1.0 + 2.0 server, keyed by name.
pub trait Handler {
    /// Runs method `method`; `None` if no such method exists.
    fn call(&self, method: &str, params: Params) -> Option<Result<Value, Error>>;
}

/// Version used for responses that cannot be tied to a request (parse
/// errors, invalid requests): JSON-RPC 2.0.
const DEFAULT_VERSION: Option<Version> = Some(Version::V2);

fn handle_call<H: Handler>(handler: &H, call: Call) -> Option<Output> {
    match call {
        Call::MethodCall(method) => {
            let result = handler
                .call(&method.method, method.params)
                .unwrap_or_else(|| Err(Error::method_not_found()));
            Some(Output::from(result, method.id, method.jsonrpc))
        }
        // Only notification handlers run for notifications, and the stratum
        // protocol registers none: they are dropped without a response.
        Call::Notification(_) => None,
        Call::Invalid { id } => Some(Output::invalid_request(id, DEFAULT_VERSION)),
    }
}

/// Handles one request text and returns the serialized response, or `None`
/// when nothing is to be answered (notifications).
pub fn handle_request<H: Handler>(handler: &H, request: &str) -> Option<String> {
    let response = match serde_json::from_str::<Request>(request) {
        Err(_) => Some(Response::Single(Output::Failure(Failure {
            id: Id::Null,
            jsonrpc: DEFAULT_VERSION,
            error: Error::new(ErrorCode::ParseError),
        }))),
        Ok(Request::Single(call)) => {
            handle_call(handler, call).map(Response::Single)
        }
        Ok(Request::Batch(calls)) => {
            let outputs: Vec<Output> = calls
                .into_iter()
                .filter_map(|call| handle_call(handler, call))
                .collect();
            if outputs.is_empty() {
                None
            } else {
                Some(Response::Batch(outputs))
            }
        }
    };
    response.map(|r| {
        serde_json::to_string(&r).expect("responses always serialize; qed")
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Echo;

    impl Handler for Echo {
        fn call(
            &self, method: &str, params: Params,
        ) -> Option<Result<Value, Error>> {
            match method {
                "echo" => Some(params.parse::<(String,)>().map(|(s,)| s.into())),
                _ => None,
            }
        }
    }

    fn check(request: &str, response: Option<&str>) {
        assert_eq!(handle_request(&Echo, request).as_deref(), response);
    }

    #[test]
    fn matches_jsonrpc_core_wire_format() {
        check(
            r#"{"jsonrpc":"2.0","method":"echo","params":["a"],"id":1}"#,
            Some(r#"{"jsonrpc":"2.0","result":"a","id":1}"#),
        );
        // JSON-RPC 1.0: no "jsonrpc" member in the response either.
        check(
            r#"{"method":"echo","params":["a"],"id":"x"}"#,
            Some(r#"{"result":"a","id":"x"}"#),
        );
        check(
            r#"{"jsonrpc":"2.0","method":"nope","params":[],"id":1}"#,
            Some(r#"{"jsonrpc":"2.0","error":{"code":-32601,"message":"Method not found"},"id":1}"#),
        );
        check(
            r#"{"jsonrpc":"2.0","method":"echo","params":[1],"id":1}"#,
            Some(r#"{"jsonrpc":"2.0","error":{"code":-32602,"message":"Invalid params: invalid type: integer `1`, expected a string."},"id":1}"#),
        );
        check(
            "{bad",
            Some(r#"{"jsonrpc":"2.0","error":{"code":-32700,"message":"Parse error"},"id":null}"#),
        );
        check(
            r#"{"jsonrpc":"2.0","id":5}"#,
            Some(r#"{"jsonrpc":"2.0","error":{"code":-32600,"message":"Invalid request"},"id":5}"#),
        );
        check(r#"{"jsonrpc":"2.0","method":"echo","params":["a"]}"#, None);
        check(
            r#"[{"method":"echo","params":["a"],"id":1},{"method":"echo","params":["b"]}]"#,
            Some(r#"[{"result":"a","id":1}]"#),
        );
        check("[]", None);
    }
}
