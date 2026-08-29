//! `GET /logs/stream` — SSE feed of the server's `tracing` events, so the desktop
//! app can show live log windows for external-service activity. Any authenticated
//! user may watch. On connect the client receives recent history (the replay ring)
//! then live lines; the UI filters by event target into per-page panels.

use rocket::get;
use rocket::response::stream::{Event, EventStream};
use tokio::sync::broadcast::error::RecvError;

use crate::auth::guard::AuthUser;
use crate::logstream::hub;

/// SSE events: `log` (a `LogLine` JSON) and, if a slow client falls behind, a
/// `warn` noting how many lines were dropped.
#[get("/logs/stream")]
pub fn logs_stream(_user: AuthUser) -> EventStream![] {
    let (snapshot, mut rx) = hub().subscribe();

    EventStream! {
        // Recent history first, so a panel opened mid-operation isn't blank.
        for line in snapshot {
            yield Event::json(&line).event("log");
        }
        loop {
            match rx.recv().await {
                Ok(line) => yield Event::json(&line).event("log"),
                // Fell behind the broadcast backlog: report the gap, keep going.
                Err(RecvError::Lagged(n)) => {
                    yield Event::data(format!("dropped {n} log lines")).event("warn");
                }
                Err(RecvError::Closed) => break,
            }
        }
    }
}
