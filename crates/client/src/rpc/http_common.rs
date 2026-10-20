// Copyright 2024 Mazze Foundation. All rights reserved.
// Mazze is free software and distributed under GNU General Public License.
// See http://www.gnu.org/licenses/

//! HTTP transport details of the RPC servers that `jsonrpsee` does not
//! provide out of the box, reproduced from the previous
//! `jsonrpc-http-server` 15 behaviour:
//!
//! * CORS: the `jsonrpc_cors` setting (`none`/unset = echo any origin,
//!   `all` = any origin, or a comma separated list of origins with glob
//!   patterns). A request whose `Origin` is not allowed is rejected with
//!   `403`, an allowed one gets `Access-Control-Allow-*` and `Vary: origin`
//!   headers, and `OPTIONS` pre-flight requests are answered directly.
//! * `jsonrpc_http_keep_alive = false` (the default) answers every request
//!   with `Connection: close`.
//! * JSON bodies end with a newline, non-`POST`/`OPTIONS` methods get `405`
//!   and oversized bodies get `413`, with the same plain-text messages.

use futures::future::BoxFuture;
use globset::{GlobBuilder, GlobMatcher};
use http::{header, HeaderMap, HeaderValue, Method, StatusCode};
use http_body_util::BodyExt;
use jsonrpsee::{
    core::{
        middleware::{Batch, Notification, RpcServiceT},
        BoxError,
    },
    server::{HttpBody, HttpRequest, HttpResponse},
    types::{ErrorObject, Request},
    MethodCallback, MethodResponse, Methods,
};
use std::{
    collections::HashSet,
    sync::Arc,
    task::{Context, Poll},
};
use tower::{Layer, Service};

/// An origin (`protocol://host[:port]`) or origin pattern.
#[derive(Clone, Debug)]
pub struct Origin {
    as_string: String,
    matcher: Option<GlobMatcher>,
}

impl PartialEq for Origin {
    fn eq(&self, other: &Self) -> bool { self.as_string == other.as_string }
}

impl Origin {
    /// Parses an origin the way `jsonrpc-server-utils` did: the protocol
    /// defaults to `http`, any path is dropped and the host is lowercased.
    pub fn parse(data: &str) -> Self {
        let mut it = data.split("://");
        let first = it.next().unwrap_or("");
        let (proto, rest) = match it.next() {
            None => (None, first),
            Some(rest) => (Some(first.to_lowercase()), rest),
        };
        let host = rest.split('/').next().unwrap_or("").to_lowercase();
        let proto = proto.unwrap_or_else(|| "http".to_string());
        let as_string = format!("{}://{}", proto, host);
        let matcher = GlobBuilder::new(&as_string)
            .case_insensitive(true)
            .build()
            .map(|g| g.compile_matcher())
            .ok();
        Origin { as_string, matcher }
    }

    fn host(&self) -> &str {
        self.as_string.splitn(2, "://").nth(1).unwrap_or("")
    }

    fn matches(&self, other: &str) -> bool {
        match &self.matcher {
            Some(m) => m.is_match(other),
            None => self.as_string.eq_ignore_ascii_case(other),
        }
    }
}

/// One entry of the CORS allow list.
#[derive(Debug, Clone, PartialEq)]
pub enum AccessControlAllowOrigin {
    Value(Origin),
    Null,
    Any,
}

impl<T: Into<String>> From<T> for AccessControlAllowOrigin {
    fn from(s: T) -> AccessControlAllowOrigin {
        match s.into().as_str() {
            "all" | "*" | "any" => AccessControlAllowOrigin::Any,
            "null" => AccessControlAllowOrigin::Null,
            origin => AccessControlAllowOrigin::Value(Origin::parse(origin)),
        }
    }
}

/// CORS domain validation policy.
#[derive(Debug, Clone, PartialEq)]
pub enum DomainsValidation<T> {
    /// Only the listed origins are allowed.
    AllowOnly(Vec<T>),
    /// No validation: every origin is allowed (and echoed back).
    Disabled,
}

#[derive(Debug, PartialEq)]
enum AllowCors {
    NotRequired,
    Invalid,
    Ok(HeaderValue),
}

fn read_header<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    headers.get(name).and_then(|v| v.to_str().ok())
}

fn cors_allow_origin(
    headers: &HeaderMap, allowed: &DomainsValidation<AccessControlAllowOrigin>,
) -> AllowCors {
    let origin = match read_header(headers, "origin") {
        None => return AllowCors::NotRequired,
        Some(origin) => origin,
    };
    if let Some(host) = read_header(headers, "host") {
        if origin.ends_with(host) && Origin::parse(origin).host() == host {
            return AllowCors::NotRequired;
        }
    }
    let echo = || {
        AllowCors::Ok(
            HeaderValue::from_str(&Origin::parse(origin).as_string)
                .unwrap_or_else(|_| HeaderValue::from_static("null")),
        )
    };
    match allowed {
        DomainsValidation::Disabled if origin == "null" => {
            AllowCors::Ok(HeaderValue::from_static("null"))
        }
        DomainsValidation::Disabled => echo(),
        DomainsValidation::AllowOnly(list) if origin == "null" => {
            if list.contains(&AccessControlAllowOrigin::Null) {
                AllowCors::Ok(HeaderValue::from_static("null"))
            } else {
                AllowCors::Invalid
            }
        }
        DomainsValidation::AllowOnly(list) => {
            let found = list.iter().any(|cors| match cors {
                AccessControlAllowOrigin::Any => true,
                AccessControlAllowOrigin::Value(val) => val.matches(origin),
                AccessControlAllowOrigin::Null => false,
            });
            if found {
                echo()
            } else {
                AllowCors::Invalid
            }
        }
    }
}

/// `Access-Control-Request-Headers`, echoed back (any header is allowed).
fn cors_allow_headers(headers: &HeaderMap) -> Option<HeaderValue> {
    let requested: Vec<&str> = headers
        .get_all("access-control-request-headers")
        .iter()
        .filter_map(|val| val.to_str().ok())
        .flat_map(|val| val.split(", "))
        .flat_map(|val| val.split(','))
        .collect();
    if requested.is_empty() {
        None
    } else {
        HeaderValue::from_str(&requested.join(", ")).ok()
    }
}

fn keep_alive(headers: &HeaderMap, keep_alive: bool) -> bool {
    match read_header(headers, "connection") {
        Some(val) => keep_alive && val != "close",
        None => keep_alive,
    }
}

const ALLOWED_METHODS: &str = "OPTIONS, POST";

fn text_response(status: StatusCode, body: &'static str) -> HttpResponse {
    let mut response = HttpResponse::new(HttpBody::from(body));
    *response.status_mut() = status;
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/plain; charset=utf-8"),
    );
    response
}

fn json_response(body: Vec<u8>) -> HttpResponse {
    let mut response = HttpResponse::new(HttpBody::from(body));
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/json; charset=utf-8"),
    );
    response
}

struct ResponseHeaders {
    is_options: bool,
    allow_origin: Option<HeaderValue>,
    allow_headers: Option<HeaderValue>,
    keep_alive: bool,
}

impl ResponseHeaders {
    fn apply(&self, headers: &mut HeaderMap) {
        let allowed = HeaderValue::from_static(ALLOWED_METHODS);
        if self.is_options {
            headers.append(header::ALLOW, allowed.clone());
            headers.append(
                header::ACCEPT,
                HeaderValue::from_static("application/json"),
            );
        }
        if let Some(origin) = &self.allow_origin {
            headers.append(header::VARY, HeaderValue::from_static("origin"));
            headers.append(header::ACCESS_CONTROL_ALLOW_METHODS, allowed);
            headers.append(header::ACCESS_CONTROL_ALLOW_ORIGIN, origin.clone());
            if let Some(allow_headers) = &self.allow_headers {
                headers.append(
                    header::ACCESS_CONTROL_ALLOW_HEADERS,
                    allow_headers.clone(),
                );
            }
        }
        if !self.keep_alive {
            headers.append(header::CONNECTION, HeaderValue::from_static("close"));
        }
    }
}

/// Tower layer applying the HTTP behaviour described in the module docs.
#[derive(Clone, Debug)]
pub struct HttpCompatLayer {
    cors: Arc<DomainsValidation<AccessControlAllowOrigin>>,
    keep_alive: bool,
}

impl HttpCompatLayer {
    pub fn new(
        cors: DomainsValidation<AccessControlAllowOrigin>, keep_alive: bool,
    ) -> Self {
        HttpCompatLayer {
            cors: Arc::new(cors),
            keep_alive,
        }
    }
}

impl<S> Layer<S> for HttpCompatLayer {
    type Service = HttpCompat<S>;

    fn layer(&self, inner: S) -> Self::Service {
        HttpCompat {
            inner,
            cors: self.cors.clone(),
            keep_alive: self.keep_alive,
        }
    }
}

#[derive(Clone, Debug)]
pub struct HttpCompat<S> {
    inner: S,
    cors: Arc<DomainsValidation<AccessControlAllowOrigin>>,
    keep_alive: bool,
}

impl<S, B> Service<HttpRequest<B>> for HttpCompat<S>
where
    S: Service<HttpRequest<B>, Response = HttpResponse>,
    S::Error: Into<BoxError> + 'static,
    S::Future: Send + 'static,
    B: Send + 'static,
{
    type Error = BoxError;
    type Future = BoxFuture<'static, Result<HttpResponse, BoxError>>;
    type Response = HttpResponse;

    fn poll_ready(
        &mut self, cx: &mut Context<'_>,
    ) -> Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx).map_err(Into::into)
    }

    fn call(&mut self, request: HttpRequest<B>) -> Self::Future {
        let headers = request.headers();
        let response_headers = ResponseHeaders {
            is_options: request.method() == Method::OPTIONS,
            allow_origin: None,
            allow_headers: cors_allow_headers(headers),
            keep_alive: keep_alive(headers, self.keep_alive),
        };
        let mut response_headers = response_headers;
        match cors_allow_origin(headers, &self.cors) {
            AllowCors::Invalid => {
                let mut response = text_response(
                    StatusCode::FORBIDDEN,
                    "Origin of the request is not whitelisted. CORS headers \
                     would not be sent and any side-effects were cancelled \
                     as well.\n",
                );
                response_headers.allow_origin = None;
                response_headers.apply(response.headers_mut());
                return Box::pin(async move { Ok(response) });
            }
            AllowCors::Ok(origin) => {
                response_headers.allow_origin = Some(origin)
            }
            AllowCors::NotRequired => {}
        }

        match *request.method() {
            Method::POST => {}
            Method::OPTIONS => {
                let mut response = json_response(Vec::new());
                response_headers.apply(response.headers_mut());
                return Box::pin(async move { Ok(response) });
            }
            _ => {
                let mut response = text_response(
                    StatusCode::METHOD_NOT_ALLOWED,
                    "Used HTTP Method is not allowed. POST or OPTIONS is \
                     required\n",
                );
                response_headers.apply(response.headers_mut());
                return Box::pin(async move { Ok(response) });
            }
        }

        let fut = self.inner.call(request);
        Box::pin(async move {
            let response = fut.await.map_err(Into::into)?;
            let mut response = if response.status() == StatusCode::OK {
                // Add a new line to have nice output with CLI clients (curl).
                let (mut parts, body) = response.into_parts();
                let mut bytes = body.collect().await?.to_bytes().to_vec();
                if bytes == b"null" {
                    // jsonrpsee answers a notification (or a batch of
                    // notifications) with `null`; the response used to be
                    // empty.
                    bytes.clear();
                } else if !bytes.is_empty() {
                    bytes.push(b'\n');
                }
                parts.headers.remove(header::CONTENT_LENGTH);
                HttpResponse::from_parts(parts, HttpBody::from(bytes))
            } else if response.status() == StatusCode::PAYLOAD_TOO_LARGE {
                text_response(
                    StatusCode::PAYLOAD_TOO_LARGE,
                    "request body size exceeds allowed maximum",
                )
            } else if response.status() == StatusCode::UNSUPPORTED_MEDIA_TYPE
            {
                text_response(
                    StatusCode::UNSUPPORTED_MEDIA_TYPE,
                    "Supplied content type is not allowed. Content-Type: \
                     application/json is required\n",
                )
            } else {
                response
            };
            response_headers.apply(response.headers_mut());
            Ok(response)
        })
    }
}

/// Error code of `jsonrpc-pubsub` for a subscription call on a transport
/// without sessions (HTTP).
pub const SUBSCRIPTIONS_UNAVAILABLE_CODE: i32 = -32090;

/// RPC middleware answering subscription and unsubscription calls made over
/// HTTP with the error `jsonrpc-pubsub` returned, instead of jsonrpsee's
/// generic internal error.
#[derive(Clone)]
pub struct NoSubscriptions<S> {
    service: S,
    names: Arc<HashSet<&'static str>>,
}

impl<S> NoSubscriptions<S> {
    /// `methods` are the methods served; their subscription and
    /// unsubscription methods are rejected.
    pub fn new(service: S, methods: &Methods) -> Self {
        let names = methods
            .method_names()
            .filter(|name| {
                matches!(
                    methods.method(name),
                    Some(MethodCallback::Subscription(_))
                        | Some(MethodCallback::Unsubscription(_))
                )
            })
            .collect();
        NoSubscriptions {
            service,
            names: Arc::new(names),
        }
    }
}

impl<S> RpcServiceT for NoSubscriptions<S>
where S: RpcServiceT<
            MethodResponse = MethodResponse,
            BatchResponse = MethodResponse,
            NotificationResponse = MethodResponse,
        > + Send
        + Sync
        + Clone
        + 'static
{
    type BatchResponse = MethodResponse;
    type MethodResponse = MethodResponse;
    type NotificationResponse = MethodResponse;

    fn call<'a>(
        &self, request: Request<'a>,
    ) -> impl std::future::Future<Output = MethodResponse> + Send + 'a {
        let rejected = if self.names.contains(request.method_name()) {
            Some(MethodResponse::error(
                request.id(),
                ErrorObject::owned(
                    SUBSCRIPTIONS_UNAVAILABLE_CODE,
                    "Subscriptions are not available on this transport.",
                    None::<()>,
                ),
            ))
        } else {
            None
        };
        let service = self.service.clone();
        async move {
            match rejected {
                Some(rp) => rp,
                None => service.call(request).await,
            }
        }
    }

    fn batch<'a>(
        &self, requests: Batch<'a>,
    ) -> impl std::future::Future<Output = MethodResponse> + Send + 'a {
        self.service.batch(requests)
    }

    fn notification<'a>(
        &self, n: Notification<'a>,
    ) -> impl std::future::Future<Output = MethodResponse> + Send + 'a {
        self.service.notification(n)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn headers(origin: Option<&str>, host: Option<&str>) -> HeaderMap {
        let mut h = HeaderMap::new();
        if let Some(o) = origin {
            h.insert("origin", HeaderValue::from_str(o).unwrap());
        }
        if let Some(host) = host {
            h.insert("host", HeaderValue::from_str(host).unwrap());
        }
        h
    }

    fn ok(v: &'static str) -> AllowCors {
        AllowCors::Ok(HeaderValue::from_static(v))
    }

    #[test]
    fn cors_disabled_echoes_any_origin() {
        let cors = DomainsValidation::Disabled;
        assert_eq!(
            cors_allow_origin(&headers(Some("http://example.com"), None), &cors),
            ok("http://example.com")
        );
        assert_eq!(
            cors_allow_origin(&headers(Some("null"), None), &cors),
            ok("null")
        );
        assert_eq!(
            cors_allow_origin(&headers(None, None), &cors),
            AllowCors::NotRequired
        );
    }

    #[test]
    fn cors_allow_list() {
        let cors = DomainsValidation::AllowOnly(
            "http://allowed.com,*.wild.org"
                .split(',')
                .map(Into::into)
                .collect(),
        );
        let check = |o| cors_allow_origin(&headers(Some(o), None), &cors);
        assert_eq!(check("http://allowed.com"), ok("http://allowed.com"));
        assert_eq!(check("http://a.wild.org"), ok("http://a.wild.org"));
        assert_eq!(check("https://allowed.com"), AllowCors::Invalid);
        assert_eq!(check("http://example.com"), AllowCors::Invalid);
        assert_eq!(check("null"), AllowCors::Invalid);
    }

    #[test]
    fn cors_any_and_same_origin() {
        let cors = DomainsValidation::AllowOnly(vec!["all".into()]);
        assert_eq!(
            cors_allow_origin(&headers(Some("https://x.io/path"), None), &cors),
            ok("https://x.io")
        );
        let cors = DomainsValidation::AllowOnly(vec![]);
        assert_eq!(
            cors_allow_origin(
                &headers(Some("http://127.0.0.1:80"), Some("127.0.0.1:80")),
                &cors
            ),
            AllowCors::NotRequired
        );
    }

    #[test]
    fn allow_headers_are_echoed() {
        let mut h = HeaderMap::new();
        h.insert(
            "access-control-request-headers",
            HeaderValue::from_static("content-type,x-foo"),
        );
        assert_eq!(
            cors_allow_headers(&h),
            Some(HeaderValue::from_static("content-type, x-foo"))
        );
        assert_eq!(cors_allow_headers(&HeaderMap::new()), None);
    }
}
