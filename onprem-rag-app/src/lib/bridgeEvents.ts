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
import { startLogStream, parseRoutedPayload, type LogLine, type IngestProgress, type ChatEvent, type Passage, type AgentKind, type AggRow, type ModelEvent, type Provenance, type RoutedEvent, type StageEvent, type Suggestion, type VerifyReport, type ClarifyPayload } from "./bridge";
import { useStream, createKeyedRafBuffer, createRafBuffer } from "../stores/stream";
import { useIngestion } from "../stores/ingestion";
import { notifyBackground } from "./notify";
import { toast } from "../stores/ui";
import { useChat } from "../stores/chat";
import { useAgents } from "../stores/agents";
import { useModels } from "../stores/models";

let started = false;
const unlisteners: UnlistenFn[] = [];

// HMR: without this, editing any module in this import chain re-runs the file and
// re-registers every listener on top of the old ones — each chat token then lands
// twice and streamed answers read "HelloHello!! I I'm'm…" until the DB refetch.
if (import.meta.hot) {
  import.meta.hot.dispose(() => {
    for (const un of unlisteners) un();
    unlisteners.length = 0;
    started = false;
  });
}

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
  // Fails with "not logged in" before auth — AppShell retries on mount (post-login).
  startLogStream().catch(() => {
    /* retried from AppShell once authenticated */
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
      void notifyBackground("Ingestion finished", "The ingestion job has completed.");
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
      void notifyBackground("Ingestion failed", "The ingestion job reported an error.");
    }),
  );

  // ── Model download listeners (Stage 8) ───────────────────────────────────────
  // Fan `model://progress|status` into the models store keyed by variant_id so the
  // progress bar survives navigation. Terminal events (done/error) are handled by
  // the store's startDownload via the invoke promise — no listeners needed here.
  unlisteners.push(
    await listen<ModelEvent>("model://progress", (ev) => {
      const pct = parseFloat(ev.payload.data);
      if (!Number.isNaN(pct)) useModels.getState().applyProgress(ev.payload.variant_id, pct);
    }),
  );

  unlisteners.push(
    await listen<ModelEvent>("model://status", (ev) => {
      useModels.getState().applyStatus(ev.payload.variant_id, ev.payload.data);
    }),
  );

  // ── Chat stream listeners (Stage 6) ──────────────────────────────────────────
  // Registered once at boot; each event carries a run_id so stale-run events are
  // dropped (double-filtered: the push guard + the store's setter guard).

  // Each run gets its own frame batch so simultaneous streams cannot mix tokens.
  const tokenBuffer = createKeyedRafBuffer<string>((runId, batch) => {
    useChat.getState().appendAnswer(runId, batch);
  });

  unlisteners.push(
    await listen<ChatEvent>("chat://token", (ev) => {
      const { run_id, data } = ev.payload;
      if (useChat.getState().runs[run_id]) tokenBuffer.push(run_id, data);
    }),
  );

  // The `/chat` route emits the FULL routed decision object (verified:
  // `RouteDecision::to_sse_json()` in onprem-rag-server/src/rag/routes.rs), which is
  // what carries `service_line` and `deterministic`. The agent route, by contrast,
  // still emits a bare kind string — see the agent://routed listener below.
  unlisteners.push(
    await listen<ChatEvent>("chat://routed", (ev) => {
      const { run_id, data } = ev.payload;
      const parsed = parseRoutedPayload(data);
      if (parsed) useChat.getState().setRouted(run_id, parsed);
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
    await listen<ChatEvent>("chat://sql", (ev) => {
      const { run_id, data } = ev.payload;
      try {
        const metadata = JSON.parse(data) as { source_id: string; sql: string };
        useChat.getState().setSqlMetadata(run_id, metadata.source_id, metadata.sql);
      } catch {
        /* ignore malformed SQL metadata */
      }
    }),
  );

  unlisteners.push(
    await listen<ChatEvent>("chat://columns", (ev) => {
      const { run_id, data } = ev.payload;
      try {
        useChat.getState().setSqlColumns(run_id, JSON.parse(data) as string[]);
      } catch {
        /* ignore malformed SQL columns */
      }
    }),
  );

  unlisteners.push(
    await listen<ChatEvent>("chat://rows", (ev) => {
      const { run_id, data } = ev.payload;
      try {
        useChat.getState().setSqlRows(run_id, JSON.parse(data) as unknown[][]);
      } catch {
        /* ignore malformed SQL rows */
      }
    }),
  );

  unlisteners.push(
    await listen<ChatEvent>("chat://verify", (ev) => {
      const { run_id, data } = ev.payload;
      try {
        useChat.getState().setVerify(run_id, JSON.parse(data) as VerifyReport);
      } catch {
        /* ignore malformed verify payload — the answer stands unannotated */
      }
    }),
  );

  // Live pipeline steps, relayed from the server's per-run progress stream. These
  // arrive *while* the server is still working — before any citations or tokens —
  // which is the whole point: the strip shows the actual step instead of a spinner.
  unlisteners.push(
    await listen<ChatEvent>("chat://stage", (ev) => {
      const { run_id, data } = ev.payload;
      try {
        useChat.getState().pushStage(run_id, JSON.parse(data) as StageEvent);
      } catch {
        /* ignore malformed stage payload — the answer is unaffected */
      }
    }),
  );

  unlisteners.push(
    await listen<ChatEvent>("chat://error", (ev) => {
      const { run_id, data } = ev.payload;
      tokenBuffer.flushNow(run_id);
      useChat.getState().setError(run_id, data);
    }),
  );

  unlisteners.push(
    await listen<ChatEvent>("chat://done", (ev) => {
      const { run_id } = ev.payload;
      tokenBuffer.flushNow(run_id);
      useChat.getState().finish(run_id);
    }),
  );

  // ── Agent stream listeners (Stage 7) ─────────────────────────────────────────
  // Registered once at boot; each event carries a run_id so stale-run events are
  // dropped (double-filtered: the push guard + the store's setter guard).

  // Keep independent agent streams in separate frame batches.
  const agentTokenBuffer = createKeyedRafBuffer<string>((runId, batch) => {
    useAgents.getState().appendAnswer(runId, batch);
  });

  unlisteners.push(
    await listen<ChatEvent>("agent://token", (ev) => {
      const { run_id, data } = ev.payload;
      if (useAgents.getState().runs[run_id]) agentTokenBuffer.push(run_id, data);
    }),
  );

  unlisteners.push(
    await listen<ChatEvent>("agent://routed", (ev) => {
      const { run_id, data } = ev.payload;
      try {
        const parsed = JSON.parse(data);
        if (typeof parsed === "string") {
          // Legacy: server emits a bare kind string.
          useAgents.getState().setRouted(run_id, parsed as AgentKind);
        } else if (parsed && typeof parsed === "object" && "route" in parsed) {
          // Plan 07: full RoutedEvent object with backend, deterministic, focus_used, etc.
          useAgents.getState().setRoutedFull(run_id, parsed as RoutedEvent);
        } else {
          // Unexpected shape — extract kind best-effort.
          const kind = (parsed as Record<string, unknown>).route ?? parsed;
          useAgents.getState().setRouted(run_id, String(kind) as AgentKind);
        }
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

  // Plan 07 §3: an agent run can carry a live SQL result. SPEC-DERIVED — today's
  // agents route emits neither `sql` nor `columns`, so these two never fire; they
  // exist so the accumulator is complete when the source_sql backend reaches the
  // agents route.
  unlisteners.push(
    await listen<ChatEvent>("agent://sql", (ev) => {
      const { run_id, data } = ev.payload;
      try {
        const metadata = JSON.parse(data) as { source_id: string; sql: string };
        useAgents.getState().setSqlMetadata(run_id, metadata.source_id, metadata.sql);
      } catch {
        /* ignore malformed SQL metadata */
      }
    }),
  );

  unlisteners.push(
    await listen<ChatEvent>("agent://columns", (ev) => {
      const { run_id, data } = ev.payload;
      try {
        useAgents.getState().setSqlColumns(run_id, JSON.parse(data) as string[]);
      } catch {
        /* ignore malformed SQL columns */
      }
    }),
  );

  // `rows` is the one ambiguous agent event: the structured path sends chart rows
  // (`[{label, value}]`) while a SQL result would send positional rows
  // (`[[...], ...]`). They are told apart STRUCTURALLY — an array-of-arrays is a
  // SQL result — not by guessing which one the server meant. Today only the chart
  // shape is ever emitted (`onprem-rag-server/src/agents/routes.rs`).
  unlisteners.push(
    await listen<ChatEvent>("agent://rows", (ev) => {
      const { run_id, data } = ev.payload;
      try {
        const parsed = JSON.parse(data) as unknown;
        if (!Array.isArray(parsed)) return;
        if (parsed.length > 0 && Array.isArray(parsed[0])) {
          useAgents.getState().setSqlRows(run_id, parsed as unknown[][]);
        } else {
          useAgents.getState().setRows(run_id, parsed as AggRow[]);
        }
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

  // Live pipeline steps for agent runs; see the chat://stage listener above.
  unlisteners.push(
    await listen<ChatEvent>("agent://stage", (ev) => {
      const { run_id, data } = ev.payload;
      try {
        useAgents.getState().pushStage(run_id, JSON.parse(data) as StageEvent);
      } catch {
        /* ignore malformed stage payload */
      }
    }),
  );

  unlisteners.push(
    await listen<ChatEvent>("agent://error", (ev) => {
      const { run_id, data } = ev.payload;
      agentTokenBuffer.flushNow(run_id);
      useAgents.getState().setError(run_id, data);
    }),
  );

  unlisteners.push(
    await listen<ChatEvent>("agent://done", (ev) => {
      const { run_id } = ev.payload;
      agentTokenBuffer.flushNow(run_id);
      useAgents.getState().finish(run_id);
    }),
  );

  // ── Plan 06 / 07: provenance, suggestions, clarify (agent stream) ─────────────
  // These events only arrive when the server implements plan 06; until then they
  // are simply never emitted, and the listeners are no-ops.

  unlisteners.push(
    await listen<ChatEvent>("agent://provenance", (ev) => {
      const { run_id, data } = ev.payload;
      try {
        useAgents.getState().setProvenance(run_id, JSON.parse(data) as Provenance);
      } catch {
        /* ignore malformed provenance payload */
      }
    }),
  );

  unlisteners.push(
    await listen<ChatEvent>("agent://suggestions", (ev) => {
      const { run_id, data } = ev.payload;
      try {
        useAgents.getState().setSuggestions(run_id, JSON.parse(data) as Suggestion[]);
      } catch {
        /* ignore malformed suggestions payload */
      }
    }),
  );

  unlisteners.push(
    await listen<ChatEvent>("agent://clarify", (ev) => {
      const { run_id, data } = ev.payload;
      try {
        const payload = JSON.parse(data) as ClarifyPayload;
        useAgents.getState().setClarify(run_id, payload);
      } catch {
        /* ignore malformed clarify payload */
      }
    }),
  );

  // ── Plan 06 / 07: provenance, suggestions, clarify (chat stream) ─────────────
  // The Ask tab on /chat uses the same server executor and emits the same events
  // on the chat:// channel.

  unlisteners.push(
    await listen<ChatEvent>("chat://provenance", (ev) => {
      const { run_id, data } = ev.payload;
      try {
        useChat.getState().setProvenance(run_id, JSON.parse(data) as Provenance);
      } catch {
        /* ignore malformed provenance payload */
      }
    }),
  );

  unlisteners.push(
    await listen<ChatEvent>("chat://suggestions", (ev) => {
      const { run_id, data } = ev.payload;
      try {
        useChat.getState().setSuggestions(run_id, JSON.parse(data) as Suggestion[]);
      } catch {
        /* ignore malformed suggestions payload */
      }
    }),
  );

  unlisteners.push(
    await listen<ChatEvent>("chat://clarify", (ev) => {
      const { run_id, data } = ev.payload;
      try {
        const payload = JSON.parse(data) as ClarifyPayload;
        useChat.getState().setClarify(run_id, payload);
      } catch {
        /* ignore malformed clarify payload */
      }
    }),
  );
}

/** Tear down all listeners (used only on full teardown; normally lives for the session). */
export function disposeBridgeEvents(): void {
  unlisteners.forEach((fn) => fn());
  unlisteners.length = 0;
  started = false;
}
