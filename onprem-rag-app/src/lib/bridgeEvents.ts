// Single registration point for Tauri bridge events.
//
// The old pattern (see the model:// and agent:// helpers in bridge.ts) had each
// component call `listen`/`unlisten` around a single request — easy to leak and
// impossible to batch across a burst. Instead we register every long-lived
// listener ONCE at app boot here, fan events into the Zustand stores, and batch
// hot streams onto requestAnimationFrame via createRafBuffer.
//
// Per-stage listeners (chat://, ingest://, model://, agent://) are added to this
// file as those screens land, so there is always exactly one subscription each.
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { startLogStream, type LogLine, type IngestProgress, type ChatEvent, type Passage, type AgentKind, type AggRow } from "./bridge";
import { useStream, createRafBuffer } from "../stores/stream";
import { useIngestion } from "../stores/ingestion";
import { toast } from "../stores/ui";
import { useChat } from "../stores/chat";
import { useAgents } from "../stores/agents";

let started = false;
const unlisteners: UnlistenFn[] = [];

/**
 * Register all bridge listeners and start the always-on server log relay.
 * Idempotent — safe to call from React.StrictMode's double-invoke; only the
 * first call wires anything.
 */
export async function initBridgeEvents(): Promise<void> {
  if (started) return;
  started = true;

  // Server tracing log — one relay for the whole app, batched per frame.
  const logBuffer = createRafBuffer<LogLine>((batch) =>
    useStream.getState().appendServerLog(batch),
  );

  unlisteners.push(
    await listen<string>("logs://line", (ev) => {
      try {
        logBuffer.push(JSON.parse(ev.payload) as LogLine);
      } catch {
        /* ignore malformed line */
      }
    }),
  );

  // Backpressure / stream errors surface as a synthetic WARN line so the console
  // shows the gap rather than silently dropping it.
  unlisteners.push(
    await listen<string>("logs://error", (ev) => {
      logBuffer.push({
        seq: -1,
        ts: new Date().toISOString(),
        level: "WARN",
        target: "bridge",
        message: `log stream: ${ev.payload}`,
      });
    }),
  );

  // Kick off the relay. Idempotent server-side (at most one stream per session).
  startLogStream().catch(() => {
    /* server may not be reachable yet; the Connect flow retries */
  });

  // Ingest progress events — arrive at ~2/sec, no rAF buffering needed (log array
  // is replaced wholesale each event, not appended incrementally like chat tokens).
  unlisteners.push(
    await listen<IngestProgress>("ingest://progress", (ev) => {
      useIngestion.getState().applyProgress(ev.payload);
    }),
  );

  // ingest://done fires when the stream ends with a terminal (completed/partial/failed)
  // status. In a rare race the payload may be undefined/unit — guard against it.
  unlisteners.push(
    await listen<unknown>("ingest://done", (ev) => {
      const p = ev.payload;
      if (p !== null && p !== undefined && typeof p === "object" && "status" in p) {
        useIngestion.getState().applyProgress(p as IngestProgress);
      } else {
        // No payload or unexpected shape — force the step to complete so the UI
        // doesn't stay stuck on "ingesting" forever.
        useIngestion.getState().markComplete();
      }
    }),
  );

  // ingest://error fires on a failed job. Payload is either a full IngestProgress
  // (status: 'failed') or a raw error string (e.g. "job not found").
  unlisteners.push(
    await listen<unknown>("ingest://error", (ev) => {
      const p = ev.payload;
      if (p !== null && p !== undefined && typeof p === "object" && "status" in p) {
        useIngestion.getState().applyProgress(p as IngestProgress);
      } else {
        toast.error(typeof p === "string" ? p : "Ingestion failed");
        useIngestion.getState().markComplete();
      }
    }),
  );

  // ── Chat stream listeners (Stage 6) ──────────────────────────────────────────
  // Registered once at boot; each event carries a run_id so stale-run events are
  // dropped (double-filtered: the push guard + the store's setter guard).

  // Token buffer: coalesces per-token pushes to one store update per rAF frame.
  const tokenBuffer = createRafBuffer<string>((batch) => {
    const p = useChat.getState().pending;
    if (p) useChat.getState().appendAnswer(p.runId, batch);
  });

  unlisteners.push(
    await listen<ChatEvent>("chat://token", (ev) => {
      const { run_id, data } = ev.payload;
      if (run_id === useChat.getState().pending?.runId) {
        tokenBuffer.push(data);
      }
    }),
  );

  unlisteners.push(
    await listen<ChatEvent>("chat://citations", (ev) => {
      const { run_id, data } = ev.payload;
      try {
        const passages = JSON.parse(data) as Passage[];
        useChat.getState().setCitations(run_id, passages);
      } catch {
        /* ignore malformed citations payload */
      }
    }),
  );

  unlisteners.push(
    await listen<ChatEvent>("chat://error", (ev) => {
      const { run_id, data } = ev.payload;
      useChat.getState().setError(run_id, data);
    }),
  );

  unlisteners.push(
    await listen<ChatEvent>("chat://done", (ev) => {
      const { run_id } = ev.payload;
      tokenBuffer.flushNow();
      useChat.getState().finish(run_id);
    }),
  );

  // ── Agent stream listeners (Stage 7) ─────────────────────────────────────────
  // Registered once at boot; each event carries a run_id so stale-run events are
  // dropped (double-filtered: the push guard + the store's setter guard).

  // Token buffer: coalesces per-token pushes to one store update per rAF frame.
  const agentTokenBuffer = createRafBuffer<string>((batch) => {
    const p = useAgents.getState().pending;
    if (p) useAgents.getState().appendAnswer(p.runId, batch);
  });

  unlisteners.push(
    await listen<ChatEvent>("agent://token", (ev) => {
      const { run_id, data } = ev.payload;
      if (run_id === useAgents.getState().pending?.runId) {
        agentTokenBuffer.push(data);
      }
    }),
  );

  unlisteners.push(
    await listen<ChatEvent>("agent://routed", (ev) => {
      const { run_id, data } = ev.payload;
      try {
        const kind = JSON.parse(data) as AgentKind;
        useAgents.getState().setRouted(run_id, kind);
      } catch {
        /* ignore malformed routed payload */
      }
    }),
  );

  unlisteners.push(
    await listen<ChatEvent>("agent://spec", (ev) => {
      const { run_id, data } = ev.payload;
      try {
        useAgents.getState().setSpec(run_id, JSON.parse(data));
      } catch {
        /* ignore malformed spec payload */
      }
    }),
  );

  unlisteners.push(
    await listen<ChatEvent>("agent://rows", (ev) => {
      const { run_id, data } = ev.payload;
      try {
        useAgents.getState().setRows(run_id, JSON.parse(data) as AggRow[]);
      } catch {
        /* ignore malformed rows payload */
      }
    }),
  );

  unlisteners.push(
    await listen<ChatEvent>("agent://pipeline", (ev) => {
      const { run_id, data } = ev.payload;
      try {
        useAgents.getState().setPipeline(run_id, JSON.parse(data) as unknown[]);
      } catch {
        /* ignore malformed pipeline payload */
      }
    }),
  );

  unlisteners.push(
    await listen<ChatEvent>("agent://citations", (ev) => {
      const { run_id, data } = ev.payload;
      try {
        useAgents.getState().setCitations(run_id, JSON.parse(data) as Passage[]);
      } catch {
        /* ignore malformed citations payload */
      }
    }),
  );

  unlisteners.push(
    await listen<ChatEvent>("agent://error", (ev) => {
      const { run_id, data } = ev.payload;
      useAgents.getState().setError(run_id, data);
    }),
  );

  unlisteners.push(
    await listen<ChatEvent>("agent://done", (ev) => {
      const { run_id } = ev.payload;
      agentTokenBuffer.flushNow();
      useAgents.getState().finish(run_id);
    }),
  );
}

/** Tear down all listeners (used only on full teardown; normally lives for the session). */
export function disposeBridgeEvents(): void {
  unlisteners.forEach((fn) => fn());
  unlisteners.length = 0;
  started = false;
}
