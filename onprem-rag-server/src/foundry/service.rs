//! HTTP client for the standalone **Foundry Local daemon**.
//!
//! Chat generation runs here rather than in this process. The SDK's `ChatClient` drives the
//! native core in-process, and a second concurrent generation there fail-fasts the whole server
//! with `STATUS_STACK_BUFFER_OVERRUN` — on one of the core's own thread-pool threads, where no
//! Rust frame exists to catch it. The daemon runs the same model on the same GPU through the same
//! ONNX Runtime, but behind a process boundary: 8 concurrent generations complete, and a native
//! ORT fault comes back as an HTTP error body instead of killing us. See
//! `plans/docs/foundry-local-webgpu-concurrency-crash.md`.
//!
//! The daemon speaks OpenAI, and the SDK's stream type is already
//! `JsonStream<CreateChatCompletionStreamResponse>` over `async-openai` types. Deserialising the
//! daemon's SSE into those same types is what lets `GuardedChatStream` keep its item type, so no
//! consumer of the stream had to change.

use std::path::PathBuf;
use std::time::Duration;

use foundry_local_sdk::{
    ChatCompletionRequestMessage, ChatCompletionTools, CreateChatCompletionResponse,
    CreateChatCompletionStreamResponse,
};
use futures::{Stream, StreamExt};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::error::{AppError, AppResult};

/// Long enough for a cold generation on a busy iGPU — the daemon serialises requests, so a
/// queued caller legitimately waits for the ones ahead of it. Only a hung daemon should hit this.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(600);
/// Model load pulls several GB off disk into GPU memory.
const LOAD_TIMEOUT: Duration = Duration::from_secs(900);
/// Startup probe: either the daemon answers promptly or it is not there.
const PROBE_TIMEOUT: Duration = Duration::from_secs(5);

/// What the daemon writes to `~/.foundry/daemon.json` on every start.
#[derive(Debug, Deserialize)]
struct DaemonInfo {
    #[serde(default)]
    web_urls: Vec<String>,
    #[serde(default)]
    daemon_version: Option<String>,
}

fn daemon_info_path() -> Option<PathBuf> {
    // `USERPROFILE` on Windows, `HOME` elsewhere — the daemon writes under the user's home
    // either way.
    let home = std::env::var("USERPROFILE")
        .or_else(|_| std::env::var("HOME"))
        .ok()?;
    Some(PathBuf::from(home).join(".foundry").join("daemon.json"))
}

/// Resolve the daemon's base URL: explicit config first, then the daemon's own status file.
///
/// The port is assigned per daemon start (it moves between restarts), so there is nothing
/// sensible to fall back to when the file is absent — that means no daemon is running.
pub fn discover_endpoint(configured: Option<&str>) -> Option<String> {
    if let Some(url) = configured {
        return Some(url.trim_end_matches('/').to_string());
    }
    let path = daemon_info_path()?;
    let raw = std::fs::read_to_string(&path).ok()?;
    let info: DaemonInfo = serde_json::from_str(&raw).ok()?;
    if let Some(version) = &info.daemon_version {
        tracing::debug!(version = %version, path = %path.display(), "found Foundry daemon status file");
    }
    info.web_urls
        .into_iter()
        .next()
        .map(|u| u.trim_end_matches('/').to_string())
}

/// Generation parameters that ride along with a chat request.
#[derive(Debug, Clone, Copy, Default)]
pub struct ChatParams {
    pub temperature: f32,
    pub max_tokens: Option<u32>,
}

/// Talks to one Foundry Local daemon.
///
/// The base URL is mutable because the daemon is not a fixed endpoint: `foundrylocald`
/// exits when idle and is restarted on demand **on a fresh port** (observed moving
/// 55862 → 58006 → 62796), rewriting `~/.foundry/daemon.json` each time. A URL resolved
/// once at boot therefore goes stale on its own, so every request that fails to connect
/// re-reads the status file and retries before giving up.
#[derive(Debug, Clone)]
pub struct ServiceClient {
    base: std::sync::Arc<std::sync::RwLock<String>>,
    /// Set when the operator pinned a URL. Pinned endpoints are never re-discovered —
    /// silently wandering off a configured address would be worse than failing.
    pinned: bool,
    http: reqwest::Client,
}

impl ServiceClient {
    pub fn new(base: impl Into<String>, pinned: bool) -> AppResult<Self> {
        let http = reqwest::Client::builder()
            .timeout(REQUEST_TIMEOUT)
            .build()
            .map_err(|e| AppError::Internal(format!("could not build the Foundry HTTP client: {e}")))?;
        Ok(Self {
            base: std::sync::Arc::new(std::sync::RwLock::new(
                base.into().trim_end_matches('/').to_string(),
            )),
            pinned,
            http,
        })
    }

    pub fn base_url(&self) -> String {
        self.base
            .read()
            .map(|b| b.clone())
            .unwrap_or_else(|e| e.into_inner().clone())
    }

    /// Re-read the daemon's status file. Returns the new base URL when it differs from
    /// the one we were using, meaning a retry is worth attempting.
    fn rediscover(&self) -> Option<String> {
        if self.pinned {
            return None;
        }
        let found = discover_endpoint(None)?;
        let current = self.base_url();
        if found == current {
            return None;
        }
        tracing::info!(from = %current, to = %found, "Foundry daemon moved; following it");
        if let Ok(mut base) = self.base.write() {
            *base = found.clone();
        }
        Some(found)
    }

    /// Issue a request, following the daemon to a new port if the first attempt cannot
    /// connect. `make` is called with the base URL to use, so the retry rebuilds the
    /// request against the new address.
    async fn send<F, Fut>(&self, make: F) -> AppResult<reqwest::Response>
    where
        F: Fn(String) -> Fut,
        Fut: std::future::Future<Output = reqwest::Result<reqwest::Response>>,
    {
        let error = match make(self.base_url()).await {
            Ok(resp) => return Ok(resp),
            Err(e) => e,
        };
        // Only a connect/transport failure can mean "the daemon moved". A timeout or a
        // protocol error is about this request, and retrying it elsewhere would be wrong.
        if error.is_connect() {
            if let Some(next) = self.rediscover() {
                return make(next).await.map_err(unreachable_error);
            }
        }
        Err(unreachable_error(error))
    }

    /// Confirm the daemon is answering, returning the model ids it can serve.
    pub async fn probe(&self) -> AppResult<Vec<String>> {
        let resp = self
            .send(|base| {
                self.http
                    .get(format!("{base}/v1/models"))
                    .timeout(PROBE_TIMEOUT)
                    .send()
            })
            .await?;
        let body: Value = resp
            .json()
            .await
            .map_err(|e| AppError::Unavailable(format!("Foundry daemon sent no model list: {e}")))?;
        Ok(body["data"]
            .as_array()
            .map(|items| {
                items
                    .iter()
                    .filter_map(|m| m["id"].as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default())
    }

    /// Model ids currently resident in the daemon.
    pub async fn loaded(&self) -> AppResult<Vec<String>> {
        let resp = self
            .send(|base| self.http.get(format!("{base}/models/loaded")).send())
            .await?;
        let ids: Vec<String> = resp.json().await.unwrap_or_default();
        Ok(ids)
    }

    pub async fn load(&self, model_id: &str) -> AppResult<()> {
        let encoded = urlencode(model_id);
        let resp = self
            .send(|base| {
                self.http
                    .get(format!("{base}/models/load/{encoded}"))
                    .timeout(LOAD_TIMEOUT)
                    .send()
            })
            .await?;
        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(AppError::Internal(format!(
                "Foundry daemon refused to load {model_id} ({status}): {}",
                body.trim()
            )));
        }
        Ok(())
    }

    pub async fn unload(&self, model_id: &str) -> AppResult<()> {
        let encoded = urlencode(model_id);
        self.send(|base| self.http.get(format!("{base}/models/unload/{encoded}")).send())
            .await?;
        Ok(())
    }

    /// Non-streaming completion. Used by the structured planner, which needs the whole
    /// tool call before it can act on it.
    pub async fn chat_once(
        &self,
        model_id: &str,
        messages: &[ChatCompletionRequestMessage],
        tools: Option<&[ChatCompletionTools]>,
        forced_tool: Option<&str>,
        params: ChatParams,
    ) -> AppResult<CreateChatCompletionResponse> {
        let body = self.build_body(model_id, messages, tools, forced_tool, params, false)?;
        let resp = self
            .send(|base| {
                self.http
                    .post(format!("{base}/v1/chat/completions"))
                    .json(&body)
                    .send()
            })
            .await?;
        let text = read_body(resp).await?;
        serde_json::from_str(&text).map_err(|e| {
            AppError::Internal(format!(
                "Foundry daemon returned an unreadable completion: {e}; raw={}",
                truncate(&text)
            ))
        })
    }

    /// Streaming completion, yielding the same chunk type the in-process path yields.
    pub async fn chat_stream(
        &self,
        model_id: &str,
        messages: &[ChatCompletionRequestMessage],
        tools: Option<&[ChatCompletionTools]>,
        forced_tool: Option<&str>,
        params: ChatParams,
    ) -> AppResult<impl Stream<Item = AppResult<CreateChatCompletionStreamResponse>> + Send + 'static>
    {
        let body = self.build_body(model_id, messages, tools, forced_tool, params, true)?;
        let resp = self
            .send(|base| {
                self.http
                    .post(format!("{base}/v1/chat/completions"))
                    .json(&body)
                    .send()
            })
            .await?;

        let status = resp.status();
        if !status.is_success() {
            let text = resp.text().await.unwrap_or_default();
            return Err(daemon_error(status, &text));
        }

        // The daemon emits `data: {...}` records separated by a blank line, terminated by
        // `data: [DONE]`. Chunks arrive split across TCP reads, so records are reassembled
        // from a running buffer rather than parsed per byte-chunk.
        let mut bytes = resp.bytes_stream();
        let stream = async_stream::stream! {
            let mut buffer = String::new();
            while let Some(next) = bytes.next().await {
                let chunk = match next {
                    Ok(b) => b,
                    Err(e) => {
                        yield Err(AppError::Unavailable(format!(
                            "Foundry daemon stream ended early: {e}"
                        )));
                        return;
                    }
                };
                buffer.push_str(&String::from_utf8_lossy(&chunk));
                while let Some(cut) = find_record_end(&buffer) {
                    let record = buffer[..cut.0].to_string();
                    buffer.drain(..cut.1);
                    match parse_sse_record(&record) {
                        SseRecord::Done => return,
                        SseRecord::Skip => {}
                        SseRecord::Chunk(parsed) => yield Ok(*parsed),
                        SseRecord::Bad(e) => yield Err(e),
                    }
                }
            }
            // A daemon that closes without `[DONE]` has ended the generation; any trailing
            // partial record is incomplete by definition and is dropped.
        };
        Ok(stream)
    }

    fn build_body(
        &self,
        model_id: &str,
        messages: &[ChatCompletionRequestMessage],
        tools: Option<&[ChatCompletionTools]>,
        forced_tool: Option<&str>,
        params: ChatParams,
        stream: bool,
    ) -> AppResult<Value> {
        if messages.is_empty() {
            return Err(AppError::Internal(
                "refusing to send an empty message list to the model".into(),
            ));
        }
        let mut body = json!({
            "model": model_id,
            "messages": serde_json::to_value(messages)
                .map_err(|e| AppError::Internal(format!("could not encode messages: {e}")))?,
            "temperature": params.temperature,
        });
        if stream {
            body["stream"] = json!(true);
        }
        if let Some(max) = params.max_tokens {
            body["max_tokens"] = json!(max);
        }
        if let Some(tools) = tools {
            body["tools"] = serde_json::to_value(tools)
                .map_err(|e| AppError::Internal(format!("could not encode tools: {e}")))?;
        }
        if let Some(name) = forced_tool {
            body["tool_choice"] = json!({ "type": "function", "function": { "name": name } });
        }
        Ok(body)
    }
}

/// One parsed SSE record.
enum SseRecord {
    Chunk(Box<CreateChatCompletionStreamResponse>),
    /// Comment, keep-alive, or a non-`data:` line.
    Skip,
    Done,
    Bad(AppError),
}

/// Byte offsets of the first complete record: `(end of payload, end of separator)`.
fn find_record_end(buffer: &str) -> Option<(usize, usize)> {
    if let Some(i) = buffer.find("\n\n") {
        return Some((i, i + 2));
    }
    if let Some(i) = buffer.find("\r\n\r\n") {
        return Some((i, i + 4));
    }
    None
}

fn parse_sse_record(record: &str) -> SseRecord {
    let mut payload = String::new();
    for line in record.lines() {
        let line = line.trim_end_matches('\r');
        if let Some(rest) = line.strip_prefix("data:") {
            payload.push_str(rest.trim_start());
        }
    }
    if payload.is_empty() {
        return SseRecord::Skip;
    }
    if payload.trim() == "[DONE]" {
        return SseRecord::Done;
    }
    match serde_json::from_str::<CreateChatCompletionStreamResponse>(&payload) {
        Ok(parsed) => SseRecord::Chunk(Box::new(parsed)),
        // A mid-stream error arrives as an `{"error":{...}}` payload rather than a chunk.
        Err(e) => match serde_json::from_str::<Value>(&payload) {
            Ok(v) if v.get("error").is_some() => SseRecord::Bad(AppError::Internal(format!(
                "Foundry daemon: {}",
                v["error"]["message"].as_str().unwrap_or("generation failed")
            ))),
            _ => SseRecord::Bad(AppError::Internal(format!(
                "Foundry daemon sent an unreadable chunk: {e}; raw={}",
                truncate(&payload)
            ))),
        },
    }
}

/// The daemon is not answering. Names the remedy, because the usual cause is simply that
/// `foundrylocald` is not running — it exits when idle and only the CLI restarts it.
fn unreachable_error(e: reqwest::Error) -> AppError {
    AppError::Unavailable(format!(
        "Foundry daemon unreachable ({e}). Start it with `foundry model load <model>`,          or set ONPREM_FOUNDRY_SERVICE_URL if it listens elsewhere."
    ))
}

/// Read a response body, turning the daemon's own error envelope into an `AppError`.
async fn read_body(resp: reqwest::Response) -> AppResult<String> {
    let status = resp.status();
    let text = resp
        .text()
        .await
        .map_err(|e| AppError::Unavailable(format!("Foundry daemon sent no body: {e}")))?;
    if !status.is_success() {
        return Err(daemon_error(status, &text));
    }
    Ok(text)
}

/// The daemon reports native ONNX failures as `{"error":{"message":...}}` with a 4xx/5xx.
/// Surfacing that message is the whole point of running generation out of process — in-process
/// the same fault takes the server down with nothing logged.
fn daemon_error(status: reqwest::StatusCode, body: &str) -> AppError {
    let message = serde_json::from_str::<Value>(body)
        .ok()
        .and_then(|v| v["error"]["message"].as_str().map(str::to_string))
        .unwrap_or_else(|| truncate(body));
    AppError::Internal(format!("Foundry daemon ({status}): {message}"))
}

fn truncate(s: &str) -> String {
    let s = s.trim();
    if s.chars().count() <= 400 {
        return s.to_string();
    }
    let cut: String = s.chars().take(400).collect();
    format!("{cut}…")
}

/// Percent-encode a model id for a path segment. Ids are `[A-Za-z0-9._:-]` in practice, but
/// `:` in `qwen3-8b-generic-gpu:2` must not be read as a scheme separator.
fn urlencode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encodes_the_colon_in_a_variant_id() {
        assert_eq!(urlencode("qwen3-8b-generic-gpu:2"), "qwen3-8b-generic-gpu%3A2");
    }

    #[test]
    fn splits_records_on_either_line_ending() {
        assert_eq!(find_record_end("data: {}\n\nrest"), Some((8, 10)));
        assert_eq!(find_record_end("data: {}\r\n\r\nrest"), Some((8, 12)));
        assert_eq!(find_record_end("data: {} partial"), None);
    }

    #[test]
    fn recognises_the_done_sentinel() {
        assert!(matches!(parse_sse_record("data: [DONE]"), SseRecord::Done));
        assert!(matches!(parse_sse_record(": keep-alive"), SseRecord::Skip));
    }

    #[test]
    fn parses_a_daemon_chunk() {
        // Trimmed from a real qwen3-8b response; the daemon adds `message`, `IsDelta`,
        // `Successful` and `HttpStatusCode` fields that async-openai does not model.
        let record = r#"data: {"model":"qwen3-8b","choices":[{"delta":{"role":"assistant","content":"Hello"},"index":0}],"created":1788741969,"id":"chat.id.1","IsDelta":false,"Successful":true,"HttpStatusCode":0,"object":"chat.completion.chunk"}"#;
        match parse_sse_record(record) {
            SseRecord::Chunk(parsed) => {
                let delta = parsed.choices[0].delta.content.as_deref();
                assert_eq!(delta, Some("Hello"));
            }
            _ => panic!("expected a parsed chunk"),
        }
    }

    #[test]
    fn surfaces_a_mid_stream_error_payload() {
        let record = r#"data: {"error":{"message":"Non-zero status code returned while running GroupQueryAttention node","type":"server_error"}}"#;
        match parse_sse_record(record) {
            SseRecord::Bad(e) => assert!(e.to_string().contains("GroupQueryAttention")),
            _ => panic!("expected an error record"),
        }
    }
}
