// Agent message bubble — handles three display modes (same discriminant as
// chat/MessageBubble.tsx) but with agent-specific extensions:
//   1. Routed-kind badge sourced from the registry (no hardcoded labels).
//   2. Mode badge, deterministic lightning icon, focus_used chip.
//   3. Structured path: chart from specToChart → ChartView, narration, pipeline disclosure.
//   4. Semantic path: Markdown narration + Citations.
//   5. ProvenanceStrip after each completed answer.
//   6. SuggestionChips below the last assistant message.
//   7. Clarify block (question + option buttons that submit the option as the next prompt).
//   8. AgentActivityStrip while answer is empty and no rows have arrived yet.
import { useState } from 'react';
import { Copy, Check, ChevronDown, ChevronUp, Zap, ArrowRight } from 'lucide-react';
import type { StoredMessage } from '../../lib/bridge';
import type { AgentPending } from '../../stores/agents';
import { useAgentRegistry } from '../../stores/agentRegistry';
import Markdown from '../../components/Markdown';
import Citations from '../chat/Citations';
import SqlResultTable from '../chat/SqlResultTable';
import { Badge } from '../../components/ui';
import { ChartView } from '../../components/chart/AgentChart';
import { specToChart } from '../../components/chart/specToChart';
import AgentActivityStrip from './AgentActivityStrip';
import ProvenanceStrip from '../../components/answer/ProvenanceStrip';
import SuggestionChips from '../../components/answer/SuggestionChips';

/** Callback shape shared by the suggestion chips and the clarify option buttons. */
type SubmitFn = (
  text: string,
  opts?: { suggestionSpec?: unknown; switchKind?: string },
) => void;

type MessageBubbleProps =
  | {
      kind: 'persisted';
      message: StoredMessage;
      isLast?: boolean;
      onSuggestionSubmit?: SubmitFn;
      /** Switch the active tab to `kind`, carrying the conversation with it. */
      onSwitchAgent?: (kind: string) => void;
    }
  | { kind: 'optimistic-user'; text: string }
  | {
      kind: 'optimistic-assistant';
      pending: AgentPending;
      isLast?: boolean;
      onSuggestionSubmit?: SubmitFn;
      onSwitchAgent?: (kind: string) => void;
    };

export default function MessageBubble(props: MessageBubbleProps) {
  const [copied, setCopied] = useState(false);
  const [pipelineOpen, setPipelineOpen] = useState(false);
  const registry = useAgentRegistry();

  function handleCopy(text: string) {
    navigator.clipboard.writeText(text).then(() => {
      setCopied(true);
      setTimeout(() => setCopied(false), 2000);
    }).catch(() => {
      /* clipboard write failed — silently ignore */
    });
  }

  // ── User bubble ──────────────────────────────────────────────────────────────
  if (props.kind === 'optimistic-user') {
    return (
      <div className="flex justify-end px-1">
        <div className="max-w-[80%] rounded-2xl rounded-tr-sm px-4 py-2.5 bg-accent-subtle text-fg text-sm leading-relaxed">
          {props.text}
        </div>
      </div>
    );
  }

  if (props.kind === 'persisted' && props.message.role === 'user') {
    return (
      <div className="flex justify-end px-1">
        <div className="max-w-[80%] rounded-2xl rounded-tr-sm px-4 py-2.5 bg-accent-subtle text-fg text-sm leading-relaxed">
          {props.message.content}
        </div>
      </div>
    );
  }

  // ── Assistant bubble ─────────────────────────────────────────────────────────
  const pendingRun = props.kind === 'optimistic-assistant' ? props.pending : null;
  const isDone = props.kind === 'persisted'
    || pendingRun?.phase === 'done'
    || pendingRun?.phase === 'stopped';

  const content =
    props.kind === 'persisted'
      ? props.message.content
      : (pendingRun?.answer ?? '');

  const citations =
    props.kind === 'persisted'
      ? (props.message.citations ?? [])
      : (pendingRun?.citations ?? []);

  // Routed kind from persisted field or live routedKind — looked up in registry.
  const agentKindStr: string | undefined =
    props.kind === 'persisted'
      ? props.message.agent_kind
      : (pendingRun?.routedKind ?? undefined);
  // Use registry for the label; fall back to the kind string itself.
  const agentLabel = agentKindStr
    ? (registry.byKind(agentKindStr)?.label ?? agentKindStr)
    : undefined;

  // Mode badge. For a live run this is the mode the turn was actually SENT with
  // (`AgentPending.opts`, captured at startRun), not the conversation's current
  // mode — switching the toggle mid-answer must not relabel an in-flight bubble.
  const mode =
    props.kind === 'persisted'
      ? props.message.mode
      : pendingRun?.opts?.mode;

  // Deterministic indicator (from routed payload)
  const deterministic = pendingRun?.routed?.deterministic ?? false;

  // Focus entities used (from routed payload or persisted field)
  const focusUsed =
    props.kind === 'persisted'
      ? (props.message.focus_used ?? [])
      : (pendingRun?.focusUsed ?? []);

  // ── Structured result (chart path) ──────────────────────────────────────────
  const resolvedSpec =
    props.kind === 'persisted'
      ? props.message.structured?.spec
      : (pendingRun?.spec != null ? pendingRun.spec as Record<string, unknown> : undefined);

  const resolvedRows =
    props.kind === 'persisted'
      ? props.message.structured?.rows
      : (pendingRun?.rows ?? undefined);

  const resolvedPipeline: unknown[] | undefined =
    props.kind === 'persisted'
      ? props.message.structured?.pipeline
      : (pendingRun?.pipeline ?? undefined);

  const chart =
    resolvedSpec != null && resolvedRows != null
      ? specToChart(resolvedSpec, resolvedRows)
      : null;

  const hasStructured = resolvedRows != null;

  // ── Provenance (plan 06) ──────────────────────────────────────────────────────
  const provenance =
    props.kind === 'persisted'
      ? props.message.provenance
      : (pendingRun?.provenance ?? undefined);

  // ── Suggestions (plan 06) ────────────────────────────────────────────────────
  const suggestions =
    props.kind === 'persisted'
      ? (props.message.suggestions ?? [])
      : (pendingRun?.suggestions ?? []);

  // At this point props.kind is 'persisted' | 'optimistic-assistant'; both have isLast.
  const isLast = props.isLast ?? false;

  // ── Clarify block (plan 06) ──────────────────────────────────────────────────
  // `StoredMessage.clarify` is already a `ClarifyPayload` ({question, slot,
  // options}) — an earlier version rebuilt it with `slot` in the `question`
  // position, which rendered "dimension" where the question belonged.
  const clarify =
    props.kind === 'persisted'
      ? (props.message.clarify ?? null)
      : (pendingRun?.clarify ?? null);

  // Both assistant variants can submit: a persisted clarify block from a reloaded
  // conversation must stay clickable, not just a live one.
  const onSuggestionSubmit = props.onSuggestionSubmit;
  const onSwitchAgent = props.onSwitchAgent;

  // ── SQL result (plan 07 §4: "SqlResultTable (reuse from chat)") ─────────────
  const sqlResult =
    props.kind === 'persisted'
      ? props.message.sql_result
      : (pendingRun?.sqlResult ?? undefined);

  // ── Legacy badge (plan 07 §6) ───────────────────────────────────────────────
  // A persisted conversation whose `agent_kind` is not in the roster predates the
  // service-line rename. Only judged once the roster has loaded.
  const isLegacy = registry.isLegacyKind(agentKindStr);

  // ── "Open in {Line}" (plan 07 §4) ───────────────────────────────────────────
  // Offered when the router resolved a service line different from the tab the
  // user is on. Client-side only — see the note on `onSwitchAgent` in index.tsx.
  const routedLine =
    props.kind === 'optimistic-assistant' ? (pendingRun?.routed?.service_line ?? null) : null;
  const routedLineLabel = routedLine ? registry.labelFor(routedLine) : undefined;
  const canOpenInLine =
    routedLine != null
    && routedLineLabel != null
    && onSwitchAgent != null
    && registry.byKind(routedLine) != null
    && routedLine !== (pendingRun?.selectedKind ?? null);

  return (
    <div className="flex flex-col gap-1 max-w-[90%] px-1">

      {/* Header: routed-kind badge + mode + deterministic icon + focus chip */}
      <div className="flex flex-wrap items-center gap-1.5 mb-0.5">
        {agentLabel && (
          <Badge variant="info">{agentLabel}</Badge>
        )}
        {mode && mode !== 'ask' && (
          <Badge variant="neutral">{mode.charAt(0).toUpperCase() + mode.slice(1)}</Badge>
        )}
        {deterministic && (
          <span title="Deterministic SQL" className="text-fg-muted" aria-label="Deterministic">
            <Zap size={12} aria-hidden="true" />
          </span>
        )}
        {focusUsed.length > 0 && (
          <span className="text-xs text-fg-subtle px-1.5 py-0.5 rounded bg-elevated">
            using: {focusUsed.join(' · ')}
          </span>
        )}
        {isLegacy && (
          <span title="Answered by an agent this server no longer lists">
            <Badge variant="neutral">Legacy</Badge>
          </span>
        )}
        {canOpenInLine && (
          <button
            onClick={() => onSwitchAgent(routedLine)}
            className="inline-flex items-center gap-1 text-xs text-accent hover:text-accent-hover px-1.5 py-0.5 rounded hover:bg-elevated transition-colors"
          >
            <ArrowRight size={11} aria-hidden="true" />
            Open in {routedLineLabel}
          </button>
        )}
      </div>

      {/* Activity strip: visible while no tokens and no rows */}
      {pendingRun !== null && !isDone && content === '' && !hasStructured && (
        <AgentActivityStrip phase={pendingRun.phase} steps={pendingRun.stages} />
      )}

      {pendingRun?.phase === 'stopped' && (
        <p className="text-xs text-fg-muted">Generation stopped</p>
      )}

      {/* Error display */}
      {pendingRun?.error && (
        <p className="text-sm text-danger bg-danger-subtle px-3 py-2 rounded-lg">
          {pendingRun.error}
        </p>
      )}

      {/* Chart — rendered above the narration for structured results */}
      {chart && (
        <div className="mb-1">
          <ChartView chart={chart} />
        </div>
      )}

      {/* Live SQL result — the same table the chat feature renders, not a copy. */}
      {sqlResult && <SqlResultTable result={sqlResult} />}

      {/* Pipeline disclosure — mirrors Citations collapsible pattern */}
      {resolvedPipeline && resolvedPipeline.length > 0 && (
        <div className="mt-0.5 border-t border-border pt-1">
          <button
            onClick={() => setPipelineOpen((v) => !v)}
            className="flex items-center gap-1.5 text-xs text-fg-muted hover:text-fg transition-colors min-h-[44px] py-2"
            aria-expanded={pipelineOpen}
          >
            {pipelineOpen
              ? <ChevronUp size={13} aria-hidden="true" />
              : <ChevronDown size={13} aria-hidden="true" />}
            <span>Query</span>
          </button>
          {pipelineOpen && (
            <pre className="mt-1 text-xs font-mono bg-elevated rounded-md p-3 overflow-x-auto text-fg-muted whitespace-pre-wrap">
              {JSON.stringify(resolvedPipeline, null, 2)}
            </pre>
          )}
        </div>
      )}

      {/* Clarify block (plan 06) */}
      {clarify && (
        <div className="mt-1 p-3 rounded-lg border border-border bg-surface">
          <p className="text-sm font-medium text-fg mb-2">{clarify.question}</p>
          <div className="flex flex-wrap gap-1.5">
            {clarify.options.map((opt, i) => (
              <button
                key={i}
                onClick={() => onSuggestionSubmit?.(opt)}
                className="px-2.5 py-1.5 rounded-full border border-border text-xs text-fg-muted hover:border-accent hover:text-fg bg-elevated transition-colors"
              >
                {opt}
              </button>
            ))}
          </div>
        </div>
      )}

      {/* Streamed / persisted answer text */}
      {content && (
        <div className="text-sm text-fg prose-sm">
          {isDone
            ? <Markdown content={content} />
            : <div className="whitespace-pre-wrap break-words">{content}</div>}
        </div>
      )}

      {/* Copy button (appears once there is content) */}
      {content && (
        <button
          onClick={() => handleCopy(content)}
          className="flex items-center gap-1.5 text-xs text-fg-subtle hover:text-fg-muted transition-colors self-start py-1"
          aria-label={copied ? 'Copied' : 'Copy response'}
        >
          {copied
            ? <Check size={12} aria-hidden="true" />
            : <Copy size={12} aria-hidden="true" />}
          <span>{copied ? 'Copied' : 'Copy'}</span>
        </button>
      )}

      {/* Citations — reused from chat, not duplicated */}
      {citations.length > 0 && <Citations citations={citations} />}

      {/* Provenance strip (plan 06) — shown when done */}
      {isDone && provenance && <ProvenanceStrip provenance={provenance} />}

      {/* Suggestion chips (plan 06) — last assistant message only, live or persisted */}
      {isDone && isLast && suggestions.length > 0 && onSuggestionSubmit && (
        <SuggestionChips suggestions={suggestions} onSubmit={onSuggestionSubmit} />
      )}
    </div>
  );
}
