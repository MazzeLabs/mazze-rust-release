// Copyright 2024 Mazze Foundation. All rights reserved.
// Mazze is free software and distributed under GNU General Public License.
// See http://www.gnu.org/licenses/

//! Per-method interception of RPC calls (throttling, metrics).
//!
//! The interceptor is applied to the method callbacks of a module, not to a
//! server, so a throttled API is throttled the same way on every transport
//! (HTTP, WebSocket and the raw TCP endpoint) and an API that is mounted
//! without an interceptor is never throttled, exactly as with the previous
//! `RpcProxy` over `jsonrpc-core` delegates.

use jsonrpsee::{
    types::ErrorObjectOwned, MethodCallback, MethodResponse, Methods,
};
use mazze_rpc_error::Result as RpcResult;
use std::{any::Any, sync::Arc};

pub trait RpcInterceptor: Send + Sync + 'static {
    /// Called before the method runs; an error is returned to the caller
    /// instead of running the method.
    fn before(&self, name: &String) -> RpcResult<()>;

    /// Returns a guard that is kept alive while the method runs.
    fn around(&self, _name: &String) -> Option<Box<dyn Any + Send>> { None }
}

/// Returns a copy of `methods` in which every callback first runs
/// `interceptor` (method calls, subscriptions and unsubscriptions alike).
pub fn intercept<I: RpcInterceptor>(
    methods: impl Into<Methods>, interceptor: I,
) -> Methods {
    let methods: Methods = methods.into();
    let interceptor = Arc::new(interceptor);
    let mut intercepted = Methods::new();

    for name in methods.method_names() {
        let callback = methods
            .method(name)
            .cloned()
            .expect("name comes from the same module; qed");
        let wrapped = wrap(name, callback, interceptor.clone());
        intercepted
            .verify_and_insert(name, wrapped)
            .expect("names of a single module are unique; qed");
    }
    intercepted
}

fn rejected(
    id: jsonrpsee::types::Id<'_>, e: mazze_rpc_error::Error,
) -> MethodResponse {
    MethodResponse::error(id, ErrorObjectOwned::from(e))
}

fn wrap<I: RpcInterceptor>(
    name: &'static str, callback: MethodCallback, interceptor: Arc<I>,
) -> MethodCallback {
    let name = name.to_string();
    match callback {
        MethodCallback::Sync(cb) => {
            MethodCallback::Sync(Arc::new(move |id, params, max, ext| {
                if let Err(e) = interceptor.before(&name) {
                    return rejected(id, e);
                }
                let _guard = interceptor.around(&name);
                cb(id, params, max, ext)
            }))
        }
        MethodCallback::Async(cb) => MethodCallback::Async(Arc::new(
            move |id, params, conn_id, max, ext| {
                let name = name.clone();
                let interceptor = interceptor.clone();
                let cb = cb.clone();
                Box::pin(async move {
                    if let Err(e) = interceptor.before(&name) {
                        return rejected(id, e);
                    }
                    let _guard = interceptor.around(&name);
                    cb(id, params, conn_id, max, ext).await
                })
            },
        )),
        MethodCallback::Subscription(cb) => MethodCallback::Subscription(
            Arc::new(move |id, params, sink, state, ext| {
                if let Err(e) = interceptor.before(&name) {
                    let rp = rejected(id, e);
                    return Box::pin(async move { rp });
                }
                cb(id, params, sink, state, ext)
            }),
        ),
        MethodCallback::Unsubscription(cb) => MethodCallback::Unsubscription(
            Arc::new(move |id, params, conn_id, max, ext| {
                if let Err(e) = interceptor.before(&name) {
                    return rejected(id, e);
                }
                cb(id, params, conn_id, max, ext)
            }),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::{intercept, RpcInterceptor};
    use jsonrpsee::{proc_macros::rpc, RpcModule};
    use mazze_rpc_error::{Error as RpcError, Result as RpcResult};
    use std::sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    };

    #[rpc(server)]
    pub trait Foo {
        #[method(name = "mazze_balance")]
        fn balance(&self, id: usize) -> RpcResult<usize>;
    }

    struct FooImpl;

    impl FooServer for FooImpl {
        fn balance(&self, id: usize) -> RpcResult<usize> { Ok(id) }
    }

    #[derive(Default)]
    struct Bar {
        handled: Arc<AtomicBool>,
        error: Option<RpcError>,
    }

    impl RpcInterceptor for Bar {
        fn before(&self, _name: &String) -> RpcResult<()> {
            self.handled.store(true, Ordering::SeqCst);
            match self.error {
                Some(ref err) => Err(err.clone()),
                None => Ok(()),
            }
        }
    }

    async fn call(module: RpcModule<()>, request: &str) -> String {
        let (response, _) =
            module.raw_json_request(request, 1).await.unwrap();
        response.get().to_string()
    }

    fn module_of(methods: jsonrpsee::Methods) -> RpcModule<()> {
        let mut module = RpcModule::new(());
        module.merge(methods).unwrap();
        module
    }

    #[tokio::test]
    async fn test_interceptor_success() {
        let bar = Bar::default();
        let interceptor_handled = bar.handled.clone();
        let module = module_of(intercept(FooImpl.into_rpc(), bar));

        let request = r#"{"jsonrpc": "2.0", "method": "mazze_balance", "params": [8], "id": 1}"#;
        assert_eq!(
            call(module, request).await,
            r#"{"jsonrpc":"2.0","id":1,"result":8}"#
        );
        assert_eq!(interceptor_handled.load(Ordering::SeqCst), true);
    }

    #[tokio::test]
    async fn test_interceptor_failure() {
        let mut bar = Bar::default();
        bar.error = Some(RpcError::invalid_params("some test error"));
        let module = module_of(intercept(FooImpl.into_rpc(), bar));

        let request = r#"{"jsonrpc": "2.0", "method": "mazze_balance", "params": [8], "id": 1}"#;
        assert_eq!(
            call(module, request).await,
            r#"{"jsonrpc":"2.0","id":1,"error":{"code":-32602,"message":"some test error"}}"#
        );
    }

    #[tokio::test]
    async fn test_interceptor_embedded() {
        let bar = Bar::default();
        let interceptor_1_handled = bar.handled.clone();
        let proxy_1 = intercept(FooImpl.into_rpc(), bar);

        let bar = Bar::default();
        let interceptor_2_handled = bar.handled.clone();
        let module = module_of(intercept(proxy_1, bar));

        let request = r#"{"jsonrpc": "2.0", "method": "mazze_balance", "params": [8], "id": 1}"#;
        assert_eq!(
            call(module, request).await,
            r#"{"jsonrpc":"2.0","id":1,"result":8}"#
        );
        assert_eq!(interceptor_1_handled.load(Ordering::SeqCst), true);
        assert_eq!(interceptor_2_handled.load(Ordering::SeqCst), true);
    }
}
