// Copyright 2024 Mazze Foundation. All rights reserved.
// Mazze is free software and distributed under GNU General Public License.
// See http://www.gnu.org/licenses/

//! The JSON-RPC error object returned by the Mazze RPC handlers.
//!
//! The handlers used to build `jsonrpc_core::Error` values directly (public
//! `code` / `message` / `data` fields, `ErrorCode::ServerError(i64)` and so
//! on). This crate keeps that exact shape so the hundreds of call sites, and
//! the error objects they put on the wire, stay unchanged while the
//! transport is provided by `jsonrpsee`. Conversion to and from
//! [`jsonrpsee_types::ErrorObjectOwned`] happens at the server and client
//! boundary.

use jsonrpsee_types::ErrorObjectOwned;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::{fmt, future::Future, pin::Pin};

pub use serde_json::{to_value, Value};

/// JSON-RPC error code.
#[derive(Debug, PartialEq, Eq, Clone)]
pub enum ErrorCode {
    /// Invalid JSON was received by the server.
    ParseError,
    /// The JSON sent is not a valid Request object.
    InvalidRequest,
    /// The method does not exist / is not available.
    MethodNotFound,
    /// Invalid method parameter(s).
    InvalidParams,
    /// Internal JSON-RPC error.
    InternalError,
    /// Reserved for implementation-defined server-errors.
    ServerError(i64),
}

impl ErrorCode {
    /// Returns the integer code value.
    pub fn code(&self) -> i64 {
        match *self {
            ErrorCode::ParseError => -32700,
            ErrorCode::InvalidRequest => -32600,
            ErrorCode::MethodNotFound => -32601,
            ErrorCode::InvalidParams => -32602,
            ErrorCode::InternalError => -32603,
            ErrorCode::ServerError(code) => code,
        }
    }

    /// Returns the human-readable description.
    pub fn description(&self) -> String {
        let desc = match *self {
            ErrorCode::ParseError => "Parse error",
            ErrorCode::InvalidRequest => "Invalid request",
            ErrorCode::MethodNotFound => "Method not found",
            ErrorCode::InvalidParams => "Invalid params",
            ErrorCode::InternalError => "Internal error",
            ErrorCode::ServerError(_) => "Server error",
        };
        desc.to_string()
    }
}

impl From<i64> for ErrorCode {
    fn from(code: i64) -> Self {
        match code {
            -32700 => ErrorCode::ParseError,
            -32600 => ErrorCode::InvalidRequest,
            -32601 => ErrorCode::MethodNotFound,
            -32602 => ErrorCode::InvalidParams,
            -32603 => ErrorCode::InternalError,
            code => ErrorCode::ServerError(code),
        }
    }
}

impl<'a> Deserialize<'a> for ErrorCode {
    fn deserialize<D>(deserializer: D) -> std::result::Result<ErrorCode, D::Error>
    where D: Deserializer<'a> {
        let code: i64 = Deserialize::deserialize(deserializer)?;
        Ok(ErrorCode::from(code))
    }
}

impl Serialize for ErrorCode {
    fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where S: Serializer {
        serializer.serialize_i64(self.code())
    }
}

/// JSON-RPC error object, serialized as `{"code", "message", "data"?}`.
#[derive(Debug, PartialEq, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Error {
    /// Code
    pub code: ErrorCode,
    /// Message
    pub message: String,
    /// Optional data
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

impl Error {
    /// Wraps the given `ErrorCode`, using its description as message.
    pub fn new(code: ErrorCode) -> Self {
        Error {
            message: code.description(),
            code,
            data: None,
        }
    }

    /// Creates a new `ParseError`.
    pub fn parse_error() -> Self { Self::new(ErrorCode::ParseError) }

    /// Creates a new `InvalidRequest`.
    pub fn invalid_request() -> Self { Self::new(ErrorCode::InvalidRequest) }

    /// Creates a new `MethodNotFound`.
    pub fn method_not_found() -> Self { Self::new(ErrorCode::MethodNotFound) }

    /// Creates a new `InvalidParams` with the given message.
    pub fn invalid_params<M>(message: M) -> Self
    where M: Into<String> {
        Error {
            code: ErrorCode::InvalidParams,
            message: message.into(),
            data: None,
        }
    }

    /// Creates `InvalidParams` for the given parameter, with details.
    pub fn invalid_params_with_details<M, T>(message: M, details: T) -> Error
    where
        M: Into<String>,
        T: fmt::Debug,
    {
        Error {
            code: ErrorCode::InvalidParams,
            message: format!("Invalid parameters: {}", message.into()),
            data: Some(Value::String(format!("{:?}", details))),
        }
    }

    /// Creates a new `InternalError`.
    pub fn internal_error() -> Self { Self::new(ErrorCode::InternalError) }

    /// Creates a new `InvalidRequest` with the invalid version description.
    pub fn invalid_version() -> Self {
        Error {
            code: ErrorCode::InvalidRequest,
            message: "Unsupported JSON-RPC protocol version".to_owned(),
            data: None,
        }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "{}: {}", self.code.description(), self.message)
    }
}

impl std::error::Error for Error {}

impl From<Error> for ErrorObjectOwned {
    fn from(e: Error) -> Self {
        // Every code this node defines fits in an i32 (they live in the
        // JSON-RPC reserved range or in [-32999, -30000]).
        ErrorObjectOwned::owned(e.code.code() as i32, e.message, e.data)
    }
}

impl From<ErrorObjectOwned> for Error {
    fn from(e: ErrorObjectOwned) -> Self {
        Error {
            code: ErrorCode::from(e.code() as i64),
            message: e.message().to_owned(),
            data: e.data().and_then(|raw| serde_json::from_str(raw.get()).ok()),
        }
    }
}

/// Result of an RPC handler.
pub type Result<T> = std::result::Result<T, Error>;

/// Boxed future returned by asynchronous RPC handlers.
pub type BoxFuture<T> = Pin<Box<dyn Future<Output = Result<T>> + Send>>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serializes_like_the_jsonrpc_spec() {
        let e = Error {
            code: ErrorCode::ServerError(-32072),
            message: "Request rejected.".into(),
            data: Some(Value::String("throttled".into())),
        };
        assert_eq!(
            serde_json::to_string(&e).unwrap(),
            r#"{"code":-32072,"message":"Request rejected.","data":"throttled"}"#
        );
        let e = Error::method_not_found();
        assert_eq!(
            serde_json::to_string(&e).unwrap(),
            r#"{"code":-32601,"message":"Method not found"}"#
        );
    }

    #[test]
    fn round_trips_through_jsonrpsee() {
        let e = Error {
            code: ErrorCode::InvalidParams,
            message: "Invalid parameters: x".into(),
            data: Some(Value::String("\"bad\"".into())),
        };
        let obj: ErrorObjectOwned = e.clone().into();
        assert_eq!(
            serde_json::to_string(&obj).unwrap(),
            serde_json::to_string(&e).unwrap()
        );
        assert_eq!(Error::from(obj), e);

        let obj: ErrorObjectOwned = Error::internal_error().into();
        assert_eq!(
            serde_json::to_string(&obj).unwrap(),
            r#"{"code":-32603,"message":"Internal error"}"#
        );
    }
}
