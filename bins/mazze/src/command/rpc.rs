// Copyright 2024 Mazze Foundation. All rights reserved.
// Mazze is free software and distributed under GNU General Public License.
// See http://www.gnu.org/licenses/

use crate::command::helpers::{input_password, password_prompt};
use clap::ArgMatches;
use jsonrpsee::{
    core::{
        client::{ClientT, Error as ClientError},
        params::ArrayParams,
    },
    http_client::HttpClientBuilder,
};
use serde_json::{Map, Value};
use std::{str::FromStr, time::Duration};

/// Positional parameters of an RPC call; `None` sends no parameters.
pub enum Params {
    None,
    Array(Vec<Value>),
}

pub struct RpcCommand {
    pub url: String,
    pub method: String,
    pub args: Params,
}

impl RpcCommand {
    pub fn parse(matches: &ArgMatches) -> Result<Option<RpcCommand>, String> {
        let method = match matches.value_of("rpc-method") {
            Some(method) => method,
            None => return Ok(None),
        };

        let url = matches
            .value_of("url")
            .ok_or_else(|| String::from("RPC URL not specified"))?;

        let args = match matches.values_of("rpc-args") {
            Some(args) => args,
            None => {
                return Ok(Some(RpcCommand {
                    url: url.into(),
                    method: method.into(),
                    args: Params::None,
                }));
            }
        };

        let mut params = Vec::new();
        for arg in args {
            match ArgSchema::parse(arg).value(matches)? {
                Some(val) => params.push(val),
                None => break,
            }
        }

        Ok(Some(RpcCommand {
            url: url.into(),
            method: method.into(),
            args: Params::Array(params),
        }))
    }

    pub fn execute(self) -> Result<String, String> {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|e| format!("{:?}", e))?;

        // The previous client had no response size limit and no timeout.
        let client = HttpClientBuilder::default()
            .max_response_size(u32::MAX)
            .request_timeout(Duration::from_secs(u32::MAX as u64))
            .build(self.url.as_str())
            .map_err(|e| format!("{:?}", e))?;

        let mut params = ArrayParams::new();
        if let Params::Array(args) = self.args {
            for arg in args {
                params.insert(arg).map_err(|e| format!("{:?}", e))?;
            }
        }

        runtime
            .block_on(client.request::<Value, _>(self.method.as_str(), params))
            .map(|result| format!("{:#}", result))
            .map_err(|e| match e {
                // Same rendering as the old `jsonrpc-core-client` error.
                ClientError::Call(e) => format!(
                    "JsonRpcError({:?})",
                    mazze_rpc_error::Error::from(e)
                ),
                e => format!("{:?}", e),
            })
    }
}

struct ArgSchema<'a> {
    arg_name: &'a str,
    arg_type: &'a str,
}

impl<'a> ArgSchema<'a> {
    fn parse(arg: &'a str) -> Self {
        let schema: Vec<&str> = arg.splitn(2, ':').collect();
        ArgSchema {
            arg_name: schema[0],
            arg_type: schema.get(1).cloned().unwrap_or("string"),
        }
    }

    fn value(&self, matches: &ArgMatches) -> Result<Option<Value>, String> {
        match self.arg_type {
            "string" => match matches.value_of(self.arg_name) {
                Some(val) => Ok(Some(Value::String(val.into()))),
                None => Ok(None),
            },
            "bool" => Ok(Some(Value::Bool(matches.is_present(self.arg_name)))),
            "u64" => self.u64(matches),
            "password" => Ok(Some(self.password()?)),
            "password2" => Ok(Some(self.password2()?)),
            _ => {
                if self.arg_type.starts_with("map(")
                    && self.arg_type.ends_with(')')
                {
                    return Ok(Some(self.object(matches)?));
                }

                panic!("unsupported RPC argument type: {}", self.arg_type);
            }
        }
    }

    fn u64(&self, matches: &ArgMatches) -> Result<Option<Value>, String> {
        let val = match matches.value_of(self.arg_name) {
            Some(val) => val,
            None => return Ok(None),
        };

        let val = u64::from_str(val).map_err(|e| {
            format!("failed to parse argument [--{}]: {:?}", self.arg_name, e)
        })?;

        Ok(Some(Value::String(format!("{:#x}", val))))
    }

    fn object(&self, matches: &ArgMatches) -> Result<Value, String> {
        let fields: Vec<&str> = self
            .arg_type
            .trim_start_matches("map(")
            .trim_end_matches(')')
            .split(';')
            .collect();

        let mut object = Map::new();

        for field in fields {
            let schema = ArgSchema::parse(field);
            if let Some(val) = schema.value(matches)? {
                object.insert(schema.arg_name.into(), val);
            }
        }

        Ok(Value::Object(object))
    }

    fn password(&self) -> Result<Value, String> {
        input_password().map(|pwd| Value::String(pwd.as_str().to_string()))
    }

    fn password2(&self) -> Result<Value, String> {
        password_prompt().map(|pwd| Value::String(pwd.as_str().to_string()))
    }
}
