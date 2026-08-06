//! C5: X-Request-Id tower middleware.
//!
//! Honors a sanitized incoming `X-Request-Id` header; generates a fresh
//! UUID v7 otherwise. Wraps downstream processing in a span that carries
//! the id as a declared field, and stashes the id in request extensions.
//! Echoes on the response.

use std::task::{Context, Poll};

use axum::extract::Request;
use axum::http::{HeaderName, HeaderValue, Response};
use futures::future::BoxFuture;
use tower::{Layer, Service};
use tracing::Instrument;
use uuid::Uuid;

pub const HEADER: HeaderName = HeaderName::from_static("x-request-id");
const MAX_LEN: usize = 64;

/// Newtype carried in axum request extensions. Handlers reach for it via
/// `axum::Extension<RequestId>` if they need to log it explicitly.
#[derive(Clone, Debug)]
pub struct RequestId(pub String);

#[derive(Clone, Default)]
pub struct RequestIdLayer;

impl<S> Layer<S> for RequestIdLayer {
    type Service = RequestIdService<S>;
    fn layer(&self, inner: S) -> Self::Service {
        RequestIdService { inner }
    }
}

#[derive(Clone)]
pub struct RequestIdService<S> {
    inner: S,
}

/// Returns true if the candidate is acceptable as a request id:
/// 1-64 bytes, ASCII alphanumeric / `.` / `_` / `-`.
fn is_valid(candidate: &str) -> bool {
    if candidate.is_empty() || candidate.len() > MAX_LEN {
        return false;
    }
    candidate
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'_' || b == b'-')
}

impl<S, B> Service<Request<B>> for RequestIdService<S>
where
    S: Service<Request<B>, Response = Response<axum::body::Body>> + Send + Clone + 'static,
    S::Future: Send + 'static,
    B: Send + 'static,
{
    type Response = S::Response;
    type Error = S::Error;
    type Future = BoxFuture<'static, Result<Self::Response, Self::Error>>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, mut req: Request<B>) -> Self::Future {
        let id_str = req
            .headers()
            .get(&HEADER)
            .and_then(|v| v.to_str().ok())
            .filter(|s| is_valid(s))
            .map_or_else(
                || Uuid::now_v7().to_string(),
                std::string::ToString::to_string,
            );

        req.extensions_mut().insert(RequestId(id_str.clone()));

        // FIX (audit Medium, request_id.rs ~77): the old code called
        // `Span::current().record("request_id", ..)` from the OUTERMOST tower
        // layer. That was a silent no-op on two counts: (1) no per-request HTTP
        // span is active here — the inner TraceLayer's span is created deeper in
        // the stack, after we've already returned the future; (2) even if a span
        // existed, DefaultMakeSpan never declares a `request_id` field, and
        // `record()` on an undeclared field is dropped by tracing.
        //
        // Instead build our OWN span that *declares and records* the field at
        // creation (so it can never be dropped), then `Instrument` the downstream
        // future with it. Because this layer is outermost, our span becomes the
        // parent of the TraceLayer span; with the subscriber's
        // `with_current_span(true)` every downstream event — including
        // TraceLayer's — is serialized with `request_id`. The opening event also
        // surfaces the id for event-only consumers.
        let span = tracing::info_span!("http_request", request_id = %id_str);

        let mut inner = self.inner.clone();
        Box::pin(
            async move {
                tracing::debug!(event = "request_id_assigned", "request id bound to span");
                let mut response = inner.call(req).await?;
                if let Ok(hv) = HeaderValue::from_str(&id_str) {
                    response.headers_mut().insert(HEADER, hv);
                }
                Ok(response)
            }
            .instrument(span),
        )
    }
}

// All four tests below exercise the same `tracing::info_span!("http_request",
// ..)` callsite (via `app()`). `request_id_is_recorded_on_the_span` installs
// a scoped `tracing::subscriber::with_default` capturing layer to inspect
// that span's fields; tracing's per-callsite interest cache is process-global,
// so running these tests concurrently races the cache (verified: each test
// passes reliably alone, but `request_id_is_recorded_on_the_span` fails
// intermittently — sometimes deterministically — when run alongside its
// siblings under default parallelism). `#[serial_test::serial]` forces this
// module's tests to run one at a time, matching the pattern used elsewhere in
// this workspace for tests that touch process-wide shared state.
#[cfg(test)]
mod tests {
    use super::*;
    use axum::Router;
    use axum::body::Body;
    use axum::http::Request as HttpRequest;
    use axum::routing::get;
    use tower::ServiceExt;

    async fn echo() -> &'static str {
        "ok"
    }

    fn app() -> Router {
        Router::new()
            .route("/probe", get(echo))
            .layer(RequestIdLayer)
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn generates_v7_when_no_incoming_header() {
        let resp = app()
            .oneshot(
                HttpRequest::builder()
                    .uri("/probe")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let id = resp
            .headers()
            .get(&HEADER)
            .expect("response has X-Request-Id");
        let id_str = id.to_str().unwrap();
        let parsed = Uuid::parse_str(id_str).expect("response id is a UUID");
        assert_eq!(
            parsed.get_version_num(),
            7,
            "expected v7, got {}",
            parsed.get_version_num()
        );
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn honors_valid_incoming_header() {
        let resp = app()
            .oneshot(
                HttpRequest::builder()
                    .uri("/probe")
                    .header("x-request-id", "test-fixed-id-12345")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let id = resp.headers().get(&HEADER).unwrap();
        assert_eq!(id.to_str().unwrap(), "test-fixed-id-12345");
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn replaces_invalid_incoming_with_v7() {
        let too_long = "a".repeat(MAX_LEN + 1);
        let resp = app()
            .oneshot(
                HttpRequest::builder()
                    .uri("/probe")
                    .header("x-request-id", too_long)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let id = resp.headers().get(&HEADER).unwrap();
        let parsed = Uuid::parse_str(id.to_str().unwrap()).expect("valid uuid generated");
        assert_eq!(parsed.get_version_num(), 7);
    }

    // FIX (audit Medium, request_id.rs ~77) regression guard: prove the request
    // id is actually DECLARED + RECORDED on a span created during request
    // processing. The old `Span::current().record(..)` produced a span with no
    // `request_id` field, so the capture below would stay empty.
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex};
    use tracing::field::{Field, Visit};
    use tracing::subscriber::with_default;
    use tracing_subscriber::Layer;
    use tracing_subscriber::layer::{Context as LayerCtx, SubscriberExt};
    use tracing_subscriber::registry::LookupSpan;

    type Captured = Arc<Mutex<HashMap<String, String>>>;

    /// Visitor that records every span field as `name -> Debug(value)`.
    struct FieldGrabber<'a>(&'a mut HashMap<String, String>);

    impl Visit for FieldGrabber<'_> {
        fn record_str(&mut self, field: &Field, value: &str) {
            self.0.insert(field.name().to_string(), value.to_string());
        }
        fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
            self.0
                .entry(field.name().to_string())
                .or_insert_with(|| format!("{value:?}"));
        }
    }

    /// Capturing layer: on every new span, snapshot its declared+recorded
    /// fields into the shared map keyed by span name.
    struct CaptureLayer {
        sink: Captured,
    }

    impl<S> Layer<S> for CaptureLayer
    where
        S: tracing::Subscriber + for<'a> LookupSpan<'a>,
    {
        fn on_new_span(
            &self,
            attrs: &tracing::span::Attributes<'_>,
            _id: &tracing::span::Id,
            _ctx: LayerCtx<'_, S>,
        ) {
            if attrs.metadata().name() == "http_request" {
                let mut fields = HashMap::new();
                attrs.record(&mut FieldGrabber(&mut fields));
                let mut guard = self.sink.lock().unwrap();
                for (k, v) in fields {
                    guard.insert(k, v);
                }
            }
        }
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn request_id_is_recorded_on_the_span() {
        let sink: Captured = Arc::new(Mutex::new(HashMap::new()));
        let subscriber = tracing_subscriber::registry().with(CaptureLayer { sink: sink.clone() });

        let incoming = "span-field-probe-9876";
        with_default(subscriber, || {
            futures::executor::block_on(async {
                let resp = app()
                    .oneshot(
                        HttpRequest::builder()
                            .uri("/probe")
                            .header("x-request-id", incoming)
                            .body(Body::empty())
                            .unwrap(),
                    )
                    .await
                    .unwrap();
                let echoed = resp.headers().get(&HEADER).unwrap();
                assert_eq!(echoed.to_str().unwrap(), incoming);
            });
        });

        let captured = sink.lock().unwrap();
        let recorded = captured
            .get("request_id")
            .expect("`request_id` must be a declared field on the http_request span");
        // `%id_str` formats via Display; the capture stores the raw string.
        assert_eq!(
            recorded, incoming,
            "span must carry the resolved request id, not a placeholder"
        );
    }
}
