use std::convert::Infallible;

use axum::{
    extract::State,
    response::sse::{Event, KeepAlive, Sse},
};
use futures::stream::Stream;
use tokio::sync::broadcast;
use tokio_stream::{StreamExt, wrappers::BroadcastStream};

use akashic_context::AppState;

// AppEvent is defined in the kernel so ingestion can emit events without
// importing `crate::api`.
pub use akashic_kernel::AppEvent;

/// Create the broadcast channel used to fan out events to SSE clients.
pub fn create_channel() -> broadcast::Sender<AppEvent> {
    let (tx, _) = broadcast::channel(256);
    tx
}

/// SSE endpoint: `GET /api/v1/events`
pub async fn event_stream(
    State(state): State<AppState>,
) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    let rx = state.event_tx.subscribe();

    let stream = BroadcastStream::new(rx).filter_map(|result| match result {
        Ok(event) => {
            let sse_event_name = match &event {
                AppEvent::JobUpdate { .. } => "job_update",
                AppEvent::ReposChanged => "repos_changed",
                AppEvent::ServerShuttingDown { .. } => "server_shutting_down",
            };
            match Event::default().event(sse_event_name).json_data(&event) {
                Ok(ev) => Some(Ok(ev)),
                Err(_) => None,
            }
        }
        Err(_) => None, // lagged receiver — skip
    });

    Sse::new(stream).keep_alive(KeepAlive::default())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn server_shutting_down_serializes_with_type_tag() {
        let ev = AppEvent::ServerShuttingDown {
            reconnect_hint_secs: 30,
        };
        let json = serde_json::to_string(&ev).unwrap();
        assert!(
            json.contains(r#""type":"server_shutting_down""#),
            "got {json}"
        );
        assert!(json.contains(r#""reconnect_hint_secs":30"#), "got {json}");
    }
}
