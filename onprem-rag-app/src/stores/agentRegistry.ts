// Agent registry store — holds the server's hospital-agent roster and provides
// usability / tier / kind lookups to the UI. Loaded once after login; refreshed
// when a schema binding rebuild completes.
//
// The roster comes from `GET /agents`, projected by the Rust bridge (see
// `list_agents` in `src-tauri/src/commands.rs`). No service-line slug or label is
// written in the web layer — everything below is either server data or the single
// synthetic "Ask" entry, which is a UI affordance over the router's `auto` kind and
// not a service line.
//
// Falls back to LEGACY_AGENTS only when `GET /agents` 404s (an older server).
import { create } from 'zustand';
import { listAgents, ENDPOINT_NOT_FOUND } from '../lib/bridge';
import type { AgentInfo, AgentMode } from '../lib/bridge';

/**
 * The agent kind for the "Ask" tab.
 *
 * VERIFIED: `AgentKind::parse` in `onprem-rag-server/src/agents/kind.rs` L86-95
 * maps `"ask"` (and the older `"auto"`) to `AgentKind::Ask`, then every
 * `ServiceLine::from_slug` match to `AgentKind::Line(..)`, then the legacy names
 * through `legacy_kind`. `"ask"` is the canonical slug (`AgentKind::slug`, L48),
 * so that is what this sends.
 */
export const ASK_KIND = 'ask';

/**
 * Synthetic first tab. `GET /agents` returns only the 13 service lines; the router's
 * own auto-select has no roster entry, so the Ask tab is built here.
 *
 * It is always usable: `POST /agents/auto` needs no schema binding — it falls back to
 * the semantic (DocumentDB) path when no source is bound.
 */
const ASK_AGENT: AgentInfo = {
  kind: ASK_KIND,
  label: 'Ask',
  blurb: 'Routes your question to whichever agent can answer it.',
  tier: 1,
  usable: true,
  // Ask supports every mode: `AgentRequest.mode`
  // (`onprem-rag-server/src/agents/routes.rs` L81-83) is orthogonal to the kind in
  // the URL, and the server honours it for Ask as for any line (L148-151, fed into
  // `persona::system_prompt` at L272).
  modes: ['ask', 'trends', 'handover'] satisfies AgentMode[],
  sources: [],
  example_questions: [],
  concepts: [],
};

/**
 * Roster for servers without `GET /agents` at all. These are the legacy kinds
 * `parse_kind` accepts, so they remain sendable. `modes` is `['ask']` throughout
 * because no server honours the `mode` body field yet.
 */
const LEGACY_AGENTS: AgentInfo[] = [
  ASK_AGENT,
  {
    kind: 'health_query',
    label: 'Health Query',
    blurb: 'Runs structured queries against health record data and visualises results.',
    tier: 1,
    usable: true,
    modes: ['ask'] satisfies AgentMode[],
    sources: [],
    example_questions: [],
    concepts: [],
  },
  {
    kind: 'trends',
    label: 'Trends',
    blurb: 'Detects patterns and trends across patient records over time.',
    tier: 1,
    usable: true,
    modes: ['ask'] satisfies AgentMode[],
    sources: [],
    example_questions: [],
    concepts: [],
  },
  {
    kind: 'patient_lookup',
    label: 'Patient Lookup',
    blurb: 'Finds and summarises records for a specific patient.',
    tier: 1,
    usable: true,
    modes: ['ask'] satisfies AgentMode[],
    sources: [],
    example_questions: [],
    concepts: [],
  },
  {
    kind: 'summarize',
    label: 'Summarize',
    blurb: 'Produces a concise narrative summary of selected records.',
    tier: 1,
    usable: true,
    modes: ['ask'] satisfies AgentMode[],
    sources: [],
    example_questions: [],
    concepts: [],
  },
];

interface AgentRegistryState {
  /** Ask first, then the server's service lines in the server's (tier) order. */
  agents: AgentInfo[];
  /** True once `load()` has resolved (success or legacy fallback; NOT on real errors). */
  loaded: boolean;
  /**
   * Where the current `agents` list came from.
   * - `'server'`  — fetched from a live `GET /agents` response.
   * - `'legacy'`  — `GET /agents` returned 404; using LEGACY_AGENTS.
   * - `'none'`    — not loaded yet, or a real error occurred.
   */
  source: 'server' | 'legacy' | 'none';
  /**
   * Non-null when the last `load()` failed with a real error (anything other than a
   * 404 on `GET /agents`). `loaded` stays `false` so the next `load()` retries.
   */
  error: string | null;
  /**
   * Fetch the roster. Idempotent — subsequent calls are no-ops unless `force` is
   * true (used after a binding rebuild in Settings).
   */
  load(force?: boolean): Promise<void>;
  byKind(kind: string): AgentInfo | undefined;
  /** Label for a kind, falling back to the raw kind for kinds not in the roster. */
  labelFor(kind: string | null | undefined): string | undefined;
  /** All agents the current deployment's sources can actually answer. */
  usable(): AgentInfo[];
  /** All agents at a specific display tier. */
  tier(n: 1 | 2 | 3): AgentInfo[];
  /** True when `kind` is not in the roster — i.e. an old persisted conversation. */
  isLegacyKind(kind: string | null | undefined): boolean;
}

export const useAgentRegistry = create<AgentRegistryState>((set, get) => ({
  agents: [],
  loaded: false,
  source: 'none',
  error: null,

  async load(force = false) {
    if (get().loaded && !force) return;
    try {
      const serverAgents = await listAgents();
      // Ask is prepended, never merged: if a future server does ship an `auto`
      // roster entry, its own entry wins and no duplicate tab appears.
      const hasAsk = serverAgents.some((a) => a.kind === ASK_KIND);
      const agents = hasAsk ? serverAgents : [ASK_AGENT, ...serverAgents];
      set({ agents, loaded: true, source: 'server', error: null });
    } catch (e) {
      if (typeof e === 'string' && e === ENDPOINT_NOT_FOUND) {
        // Expected on an older server: `GET /agents` does not exist. Fall back to
        // the static legacy set so the UI remains functional.
        set({ agents: LEGACY_AGENTS, loaded: true, source: 'legacy', error: null });
      } else {
        // Real failure (500, auth error, network outage, malformed response).
        // Do NOT set loaded:true — the session must be able to retry.
        // Do NOT replace agents with fabricated data.
        const msg = e instanceof Error ? e.message : String(e);
        set({ agents: [], loaded: false, source: 'none', error: msg });
      }
    }
  },

  byKind(kind) {
    return get().agents.find((a) => a.kind === kind);
  },

  labelFor(kind) {
    if (!kind) return undefined;
    return get().byKind(kind)?.label ?? kind;
  },

  usable() {
    return get().agents.filter((a) => a.usable);
  },

  tier(n) {
    return get().agents.filter((a) => a.tier === n);
  },

  isLegacyKind(kind) {
    if (!kind) return false;
    const { agents, loaded } = get();
    // Before the roster loads nothing can be judged legacy.
    if (!loaded) return false;
    return !agents.some((a) => a.kind === kind);
  },
}));
