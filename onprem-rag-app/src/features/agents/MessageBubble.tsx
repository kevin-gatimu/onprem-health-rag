// Agent message bubble — handles three display modes (same discriminant as
// chat/MessageBubble.tsx) but with agent-specific extensions:
//   1. Routed-kind badge (health_query / trends / …)
//   2. Structured path: chart from specToChart → ChartView, narration, pipeline
//      disclosure (mirrors the Citations collapsible pattern).
//   3. Semantic path: Markdown narration + Citations component (reused from chat).
//   4. AgentActivityStrip while answer is empty and no rows have arrived yet.
import { useState } from 'react';
import { Copy, Check, ChevronDown, ChevronUp } from 'lucide-react';
import type { StoredMessage, AgentKind } from '../../lib/bridge';
import type { AgentPending } from '../../stores/agents';
import Markdown from '../../components/Markdown';
import Citations from '../chat/Citations';
import { Badge } from '../../components/ui';
import { ChartView } from '../../components/chart/AgentChart';
import { specToChart } from '../../components/chart/specToChart';
import AgentActivityStrip from './AgentActivityStrip';

// Human-readable label for each routed kind.
const KIND_LABELS: Partial<Record<AgentKind, string>> = {
  health_query:   'Health Query',
  trends:         'Trends',
  patient_lookup: 'Patient Lookup',
  summarize:      'Summarize',
  chat:           'Chat',
};

type MessageBubbleProps =
  | { kind: 'persisted'; message: StoredMessage }
  | { kind: 'optimistic-user'; text: string }
  | { kind: 'optimistic-assistant'; pending: AgentPending };

export default function MessageBubble(props: MessageBubbleProps) {
  const [copied, setCopied] = useState(false);
  const [pipelineOpen, setPipelineOpen] = useState(false);

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
  // After the early returns above, props is one of:
  //   { kind: 'persisted', message: StoredMessage (role==='assistant') }
  //   { kind: 'optimistic-assistant', pending: AgentPending }

  const pendingRun = props.kind === 'optimistic-assistant' ? props.pending : null;
  const isDone = props.kind === 'persisted'
    || props.pending.phase === 'done'
    || props.pending.phase === 'stopped';

  const content =
    props.kind === 'persisted'
      ? props.message.content
      : props.pending.answer;

  const citations =
    props.kind === 'persisted'
      ? (props.message.citations ?? [])
      : props.pending.citations;

  // Routed kind badge — from persisted agent_kind field or live routedKind.
  const agentKind: AgentKind | undefined =
    props.kind === 'persisted'
      ? (props.message.agent_kind as AgentKind | undefined)
      : (props.pending.routedKind ?? undefined);
  const kindLabel = agentKind ? KIND_LABELS[agentKind] : undefined;

  // ── Structured result (chart path) ──────────────────────────────────────────
  // spec: from persisted StructuredResult.spec, or cast from pending.spec (unknown).
  // rows: AggRow[] from either source; null/undefined means no chart.
  const resolvedSpec =
    props.kind === 'persisted'
      ? props.message.structured?.spec
      : (pendingRun?.spec != null
          ? pendingRun.spec as Record<string, unknown>
          : undefined);

  const resolvedRows =
    props.kind === 'persisted'
      ? props.message.structured?.rows
      : (pendingRun?.rows ?? undefined);

  const resolvedPipeline: unknown[] | undefined =
    props.kind === 'persisted'
      ? props.message.structured?.pipeline
      : (pendingRun?.pipeline ?? undefined);

  // Build a render-ready ParsedChart; null when there is nothing to plot.
  const chart =
    resolvedSpec != null && resolvedRows != null
      ? specToChart(resolvedSpec, resolvedRows)
      : null;

  // Activity strip shows while no answer AND no rows have arrived yet.
  const hasStructured = resolvedRows != null;

  return (
    <div className="flex flex-col gap-1 max-w-[90%] px-1">

      {/* Routed-kind badge */}
      {kindLabel && (
        <div className="mb-0.5">
          <Badge variant="info">{kindLabel}</Badge>
        </div>
      )}

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
    </div>
  );
}
