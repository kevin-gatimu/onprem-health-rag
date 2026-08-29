# Live Log Streaming

> The system for streaming server-side trace events to app panels in real-time, so users can watch long-running operations (model downloads, embeddings generation, ingestion) without checking the server terminal. Companion docs: [`architecture-overview.md`](architecture-overview.md).

## Problem

Long external operations gave the desktop app zero feedback. The worst case observed: fastembed pulling BGE-M3 (~2.3 GB ONNX) took ~20 min with a silent UI — indistinguishable from a hang. The only signal was the server's terminal `tracing` output, invisible to the desktop client.

## Solution

Capture **every** server-side `tracing` event (info/warn/error/debug), stream them over SSE to the app, relay via Tauri events, and display in filtered UI panels per page.

## Architecture

```
Server tracing events (info!/warn!/error!)
  │
  ├─▶ BroadcastLayer (tracing subscriber layer)
  │
  ├─▶ LogHub (broadcast + ring buffer)
  │   ├─▶ broadcast::Sender<LogLine> (for streaming clients)
  │   └─▶ VecDeque<LogLine> (last 300 entries, for late joiners)
  │
  ├─▶ GET /logs/stream (SSE endpoint, authenticated)
  │
  ├─▶ Bridge (Tauri command: start_log_stream)
  │   └─▶ Relay SSE events to Tauri event bus (logs://line)
  │
  └─▶ React components (useLogs hook)
      └─▶ LogConsole panels (per page, filtered by category)
```

## Server Implementation

### `logstream.rs` — The Hub

**`LogLine` (serializable):**
```rust
pub struct LogLine {
    pub seq: u64,           // static AtomicU64, unique per line
    pub ts: DateTime<Utc>,  // chrono::Utc::now()
    pub level: String,      // "info", "warn", "error", "debug"
    pub target: String,     // Rust module path, e.g. "onprem_server::foundry"
    pub message: String,    // the log message + context fields
}
```

**`LogHub` (process-global):**
```rust
pub struct LogHub {
    tx: broadcast::Sender<LogLine>,           // tokio::sync::broadcast
    ring: Mutex<VecDeque<LogLine>>,           // RING_CAP = 300
}

impl LogHub {
    pub fn push(&self, line: LogLine)
        // Lock ring, append + truncate to cap, then tx.send (ignore "no receivers" error)
    
    pub fn subscribe(&self) -> (Vec<LogLine>, Receiver<LogLine>)
        // Return snapshot of ring + new receiver, locked atomically
        // (no gap between snapshot and live tail; rare duplicates dedupd client-side by seq)
    
    pub fn hub() -> &'static LogHub  // via OnceLock
}
```

**`BroadcastLayer` (tracing subscriber layer):**
```rust
pub struct BroadcastLayer;

impl<S> Layer<S> for BroadcastLayer {
    fn on_event(&self, event: &tracing::Event<'_>, _ctx: Context<'_, S>) {
        // Extract level, target, message
        // Call hub().push(line)
    }
}
```

### Subscriber Setup (`main.rs`)

```rust
let subscriber = Registry::default()
    .with(EnvFilter::from_default_env())        // RUST_LOG
    .with(fmt::layer())                         // terminal output
    .with(BroadcastLayer);                      // broadcast to app

tracing::subscriber::set_global_default(subscriber)?;
```

**`RUST_LOG` (env):** Controls what **both** terminal and app see. Default: `info,onprem_server=debug`.

### SSE Endpoint (`routes/logs.rs`)

**`GET /logs/stream`** (authenticated: `AuthUser` guard)

```rust
#[get("/logs/stream")]
fn log_stream(user: AuthUser) -> EventStream![] {
    let (snapshot, mut rx) = logstream::hub().subscribe();
    
    EventStream! {
        // Yield snapshot as historical events
        for line in snapshot {
            yield Event::default()
                .event("log")
                .data(serde_json::to_string(&line)?);
        }
        
        // Stream live events
        loop {
            match rx.recv().await {
                Ok(line) => {
                    yield Event::default()
                        .event("log")
                        .data(serde_json::to_string(&line)?);
                }
                Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                    yield Event::default()
                        .event("warn")
                        .data(format!("Dropped {} log lines", n));
                }
                Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
            }
        }
    }
}
```

## Bridge Implementation

### `state.rs` — Guard Against Multiple Streams

```rust
pub struct Bridge {
    pub log_streaming: AtomicBool,  // prevents >1 relay per session
}
```

### `commands.rs` — Relay Command

```rust
#[tauri::command]
pub async fn start_log_stream(
    app: AppHandle,
    bridge: State<'_, Bridge>,
) -> Result<()> {
    // Swap guard to true; if already true, early return (idempotent)
    if bridge.log_streaming.swap(true, Ordering::SeqCst) {
        return Ok(());  // Already streaming
    }
    
    // GET /logs/stream with bearer auth
    let client = reqwest::Client::new();
    let response = client
        .get(format!("{}/logs/stream", bridge.server_url))
        .bearer_auth(&bridge.jwt)
        .send()
        .await?;
    
    let mut stream = response.bytes_stream();
    
    // Parse and relay SSE events
    loop {
        match stream.next().await {
            Some(Ok(bytes)) => {
                let text = String::from_utf8(bytes.to_vec())?;
                if text.starts_with("event: log") {
                    let data = extract_sse_data(&text);
                    app.emit("logs://line", data)?;
                }
                if text.contains("event: warn") {
                    app.emit("logs://error", extract_sse_data(&text))?;
                }
            }
            Some(Err(e)) => {
                app.emit("logs://error", e.to_string())?;
                break;
            }
            None => break,  // Stream ended
        }
    }
    
    bridge.log_streaming.store(false, Ordering::SeqCst);  // Clear guard
    Ok(())
}
```

**Guard lifecycle:**
- Set true when stream starts
- Cleared when stream ends (logout, 401, network error)
- Allows re-login to restart streaming

## Frontend Implementation

### `lib/bridge.ts` — Types & Binding

```typescript
export interface LogLine {
  seq: number;
  ts: string;
  level: "info" | "warn" | "error" | "debug";
  target: string;
  message: string;
}

export async function startLogStream(): Promise<void> {
  await invoke("start_log_stream");
}
```

### `logs/logStore.ts` — Shared Buffer & Hook

```rust
const RING_CAP = 1000;
const CATEGORY_TARGETS = {
  foundry: ["onprem_server::foundry"],
  embed: ["onprem_server::embed"],
  ingest: ["onprem_server::ingest"],
  connectors: ["onprem_server::connectors"],
  documentdb: ["onprem_server::documentdb"],
  retrieval: ["onprem_server::retrieval", "onprem_server::rag"],
};

let buffer: LogLine[] = [];
let bufferSeq = -1;

export function useLogs(categories: string[]): LogLine[] {
  // On first mount, startLogStream() + register one logs://line listener
  // (idempotent; subsequent mounts share the listener)
  
  // Parse incoming lines, dedup by seq, append to ring
  
  // Return snapshot of filtered lines
  // ⚠️ GOTCHA: getSnapshot() must return stable reference if nothing changed,
  // or useSyncExternalStore will render-loop
  // Solution: cache filtered array against buffer identity + category key
  
  return useSyncExternalStore(subscribe, getSnapshot, getSnapshot);
}
```

**Why the `getSnapshot` gotcha?** If the component re-renders but the buffer hasn't changed, `getSnapshot` must return the same array reference. Otherwise the hook thinks the state changed and loops forever. Solution: only recompute the filtered array when the buffer or category selection changes.

### `logs/LogConsole.tsx` — Reusable Panel

```typescript
interface LogConsoleProps {
  title: string;
  categories: string[];
  height?: number;
}

export function LogConsole({ title, categories, height = 200 }: LogConsoleProps) {
  const lines = useLogs(categories);
  const [paused, setPaused] = useState(false);
  const [minLevel, setMinLevel] = useState("info");
  
  return (
    <div style={{ height, overflowY: "auto", fontFamily: "monospace" }}>
      {/* Controls */}
      <div>
        <button onClick={() => setPaused(!paused)}>
          {paused ? "Resume" : "Pause"}
        </button>
        <button onClick={() => /* clear buffer */}>Clear</button>
        <select value={minLevel} onChange={(e) => setMinLevel(e.target.value)}>
          <option>debug</option>
          <option>info</option>
          <option>warn</option>
          <option>error</option>
        </select>
      </div>
      
      {/* Lines */}
      <div>
        {lines
          .filter((line) => levelOrder[line.level] >= levelOrder[minLevel])
          .map((line) => (
            <div key={line.seq} style={{ color: levelColor[line.level] }}>
              [{line.level.toUpperCase()}] {line.message}
            </div>
          ))}
      </div>
    </div>
  );
}
```

**Features:**
- **Pause:** freezes the view, buffers continue collecting
- **Clear:** empties the shared buffer
- **Min-level dropdown:** show only info/warn/error/debug or above
- **Auto-scroll:** sticks to the bottom unless paused
- **Level-colored:** different colors for info/warn/error/debug

### Embedded Panels

**Settings.tsx** (`categories={["foundry"]}`):
```
"Model & hardware activity" panel
```

**Sources.tsx** (`categories={["ingest", "connectors", "documentdb", "embed"]}`):
```
"Ingestion activity" panel
```

**Future: Chat.tsx** (`categories={["retrieval", "embed"]}`):
```
"Retrieval activity" panel (trivial addition)
```

## Configuration

### Server

**`RUST_LOG` (env):** Controls subscriber filter. Default: `info,onprem_server=debug`.
- `debug` — very verbose (tracing::debug! + tracing::info!/warn!/error!)
- `info` — normal (tracing::info!/warn!/error!)
- `warn` — errors and warnings only
- Module-specific: `onprem_server::foundry=trace` (only foundry at trace level)

## Performance Considerations

- **Ring buffer:** 300 entries on server, 1000 in React; prevents unbounded memory
- **Broadcast channel:** 512-slot capacity; if lagged clients lag >512 events, they get a warning event and keep going
- **Late joiners:** subscribe() snapshots the ring, so a new panel sees the last ~300 events immediately
- **No byte-level progress:** Foundry/fastembed download % lives on their stderr, not in `tracing` (out of scope; would require SDK callbacks)

## Verification

- ✅ `cargo check` clean (both crates, only pre-existing benign dead-code warnings)
- ✅ `npx tsc --noEmit` clean
- ✅ Manual: Server terminal shows `info` logs; Settings panel shows "Model & hardware activity" in real-time

## Possible Follow-ups

- **Chat page panel:** `categories={["retrieval", "embed"]}` — trivial, can be added anytime
- **Byte-level progress:** Requires sourcing from Foundry/fastembed SDK directly (their stderr / callback API), not from `tracing`
- **Log persistence:** Save logs to disk on shutdown (audit trail); currently ephemeral (ring buffer lifetime)

---

Source: synthesized from `plans/04-log-streaming.md`
