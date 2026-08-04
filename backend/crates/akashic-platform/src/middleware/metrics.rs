//! C5: HTTP metrics tower middleware.
//!
//! Auto-emits per-request counters and histograms using the `metrics` crate.
//! Route label resolves via axum's `MatchedPath` extension (the route
//! template, e.g. `/api/v1/notes/:id`); requests with no matched path are
//! labeled `route="<unmatched>"`. Method label is the HTTP method string;
//! status label is the response status code as a string (e.g. `"200"`).

use std::task::{Context, Poll};
use std::time::Instant;

use axum::extract::{MatchedPath, Request};
use axum::http::Response;
use futures::future::BoxFuture;
use tower::{Layer, Service};

const M_REQUESTS_TOTAL: &str = "akashic_http_requests_total";
const M_REQUEST_DURATION: &str = "akashic_http_request_duration_seconds";
const M_IN_FLIGHT: &str = "akashic_http_requests_in_flight";

/// RAII guard for the in-flight gauge: increments on construction and
/// decrements on drop. Using Drop (rather than a manual decrement after
/// `.await`) guarantees the gauge is decremented even if the inner future
/// panics or is cancelled mid-flight — otherwise a panicking handler would
/// leak `+1` into the gauge permanently (there is no `CatchPanicLayer`).
struct InFlightGuard;

impl InFlightGuard {
    fn new() -> Self {
        metrics::gauge!(M_IN_FLIGHT).increment(1.0);
        InFlightGuard
    }
}

impl Drop for InFlightGuard {
    fn drop(&mut self) {
        metrics::gauge!(M_IN_FLIGHT).decrement(1.0);
    }
}

#[derive(Clone, Default)]
pub struct MetricsLayer;

impl<S> Layer<S> for MetricsLayer {
    type Service = MetricsService<S>;
    fn layer(&self, inner: S) -> Self::Service {
        MetricsService { inner }
    }
}

#[derive(Clone)]
pub struct MetricsService<S> {
    inner: S,
}

impl<S, B> Service<Request<B>> for MetricsService<S>
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

    fn call(&mut self, req: Request<B>) -> Self::Future {
        let start = Instant::now();
        let method = req.method().as_str().to_string();
        let route = req
            .extensions()
            .get::<MatchedPath>()
            .map_or_else(|| "<unmatched>".to_string(), |m| m.as_str().to_string());

        let in_flight = InFlightGuard::new();

        let mut inner = self.inner.clone();
        Box::pin(async move {
            // Held across the await; decrements on completion, panic, or cancel.
            let _in_flight = in_flight;
            let response_result = inner.call(req).await;

            let elapsed = start.elapsed().as_secs_f64();
            let status_str = match &response_result {
                Ok(resp) => resp.status().as_u16().to_string(),
                Err(_) => "500".to_string(),
            };

            metrics::counter!(
                M_REQUESTS_TOTAL,
                "route" => route.clone(),
                "method" => method.clone(),
                "status" => status_str,
            )
            .increment(1);

            metrics::histogram!(
                M_REQUEST_DURATION,
                "route" => route,
                "method" => method,
            )
            .record(elapsed);

            response_result
        })
    }
}
