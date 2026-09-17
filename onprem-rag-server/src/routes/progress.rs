//! `GET /runs/<run_id>/progress` — live pipeline stages for one `/chat` or
//! `/agents` run.
//!
//! The client mints `run_id`, opens this stream, then posts the question with the
//! same id. Backlog first (the POST normally wins the race), then live events,
//! ending on the run's `done` marker. An abandoned stream self-terminates after
//! `IDLE_TIMEOUT` so a client that never posts can't hold a connection open.

use std::time::Duration;

use rocket::get;
use rocket::response::stream::{Event, EventStream};
use rocket::{State, tokio::time::timeout};
use tokio::sync::broadcast::error::RecvError;

use crate::auth::guard::AuthUser;
use crate::state::AppState;

/// No stage transition for this long means the run is gone (client cancelled,
/// server restarted mid-request). Comfortably longer than the slowest cold
/// model load, so a legitimately slow stage is never cut off.
const IDLE_TIMEOUT: Duration = Duration::from_secs(180);

/// SSE events: `stage` (a `ProgressEvent` JSON) and a terminal `done`.
#[get("/runs/<run_id>/progress")]
pub fn run_progress(state: &State<AppState>, _user: AuthUser, run_id: String) -> EventStream![] {
    let (backlog, mut rx) = state.run_progress.subscribe(&run_id);

    EventStream! {
        // Replay whatever already happened, so a strip that connected late still
        // shows the stages the request has finished.
        for event in backlog {
            if event.status == "done" {
                yield Event::data("").event("done");
                return;
            }
            yield Event::json(&event).event("stage");
        }

        loop {
            match timeout(IDLE_TIMEOUT, rx.recv()).await {
                Ok(Ok(event)) => {
                    if event.status == "done" {
                        break;
                    }
                    yield Event::json(&event).event("stage");
                }
                // A strip that fell behind just resumes; stages are advisory.
                Ok(Err(RecvError::Lagged(_))) => continue,
                Ok(Err(RecvError::Closed)) => break,
                Err(_elapsed) => break,
            }
        }
        yield Event::data("").event("done");
    }
}
