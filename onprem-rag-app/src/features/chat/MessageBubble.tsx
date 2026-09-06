// A single chat message bubble — handles three display modes:
//   'persisted'           — a stored StoredMessage (user or assistant)
//   'optimistic-user'     — the in-flight user question (before server persists)
//   'optimistic-assistant'— the in-flight assistant answer (live-streaming)
//
// Plan 07 §4 ("features/chat/*"): Ask on /chat gets the SAME provenance strip,
// suggestion chips, clarify block and department badge as the agents screen. The
// three components are imported from `src/components/answer/` and the registry —
// nothing here is a second copy of the agents implementation.
import { useState } from 'react';
import { Copy, Check, Zap } from 'lucide-react';
import type { StoredMessage } from '../../lib/bridge';
import type { PendingRun } from '../../stores/chat';
import { useAgentRegistry } from '../../stores/agentRegistry';
import Markdown from '../../components/Markdown';
import Citations from './Citations';
import ActivityStrip from './ActivityStrip';
import VerifyBadge from './VerifyBadge';
import SqlResultTable from './SqlResultTable';
import { Badge } from '../../components/ui';
import ProvenanceStrip from '../../components/answer/ProvenanceStrip';
import SuggestionChips from '../../components/answer/SuggestionChips';

type MessageBubbleProps =
  | {
      kind: 'persisted';
      message: StoredMessage;
      isLast?: boolean;
      onSuggestionSubmit?: (text: string, opts?: { suggestionSpec?: unknown }) => void;
    }
  | { kind: 'optimistic-user'; text: string }
  | {
      kind: 'optimistic-assistant';
      pending: PendingRun;
      isLast?: boolean;
      onSuggestionSubmit?: (text: string, opts?: { suggestionSpec?: unknown }) => void;
    };

export default function MessageBubble(props: MessageBubbleProps) {
  const [copied, setCopied] = useState(false);
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
  const content =
    props.kind === 'persisted'
      ? props.message.content
      : props.pending.answer;

  const citations =
    props.kind === 'persisted'
      ? (props.message.citations ?? [])
      : props.pending.citations;

  const pendingRun = props.kind === 'optimistic-assistant' ? props.pending : null;
  const sqlResult = props.kind === 'persisted' ? props.message.sql_result : pendingRun?.sqlResult;
  const verification = props.kind === 'persisted' ? props.message.verify : pendingRun?.verify;
  const isDone = props.kind === 'persisted'
    || props.pending.phase === 'done'
    || props.pending.phase === 'stopped';

  // ── Department badge ────────────────────────────────────────────────────────
  // `/chat` is the one route that emits the FULL routed decision, so
  // `routed.service_line` is the verified source of the department here
  // (`RouteDecision::to_sse_json`, onprem-rag-server/src/router/mod.rs L196-203).
  // The label is looked up in the registry — never spelled out in this file.
  const serviceLine =
    props.kind === 'persisted'
      ? (props.message.provenance?.service_line ?? props.message.agent_kind ?? null)
      : (pendingRun?.routed?.service_line ?? null);
  const departmentLabel = serviceLine ? registry.labelFor(serviceLine) : undefined;
  const deterministic = pendingRun?.routed?.deterministic ?? false;

  const provenance =
    props.kind === 'persisted' ? props.message.provenance : (pendingRun?.provenance ?? undefined);
  const suggestions =
    props.kind === 'persisted'
      ? (props.message.suggestions ?? [])
      : (pendingRun?.suggestions ?? []);
  const focusUsed =
    props.kind === 'persisted'
      ? (props.message.focus_used ?? [])
      : (pendingRun?.focusUsed ?? []);
  const clarify =
    props.kind === 'persisted' ? (props.message.clarify ?? null) : (pendingRun?.clarify ?? null);
  const isLast = props.isLast ?? false;
  const onSuggestionSubmit = props.onSuggestionSubmit;

  return (
    <div className="flex flex-col gap-1 max-w-[90%] px-1">
      {/* Department badge + deterministic marker + focus chip */}
      {(departmentLabel || deterministic || focusUsed.length > 0) && (
        <div className="flex flex-wrap items-center gap-1.5 mb-0.5">
          {departmentLabel && <Badge variant="info">{departmentLabel}</Badge>}
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
        </div>
      )}

      {/* Activity strip: visible while no tokens have arrived yet */}
      {pendingRun !== null && !isDone && content === '' && (
        <ActivityStrip pending={pendingRun} />
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

      {/* Streamed / persisted answer text */}
      {content && (
        <div className="text-sm text-fg prose-sm">
          {isDone
            ? <Markdown content={content} />
            : <div className="whitespace-pre-wrap break-words">{content}</div>}
        </div>
      )}

      {/* Clarify block — options submit as a normal turn (plan 07 §5.5). */}
      {clarify && (
        <div className="mt-1 p-3 rounded-lg border border-border bg-surface">
          <p className="text-sm font-medium text-fg mb-2">{clarify.question}</p>
          <div className="flex flex-wrap gap-1.5">
            {clarify.options.map((option) => (
              <button
                key={option}
                onClick={() => onSuggestionSubmit?.(option)}
                className="px-2.5 py-1.5 rounded-full border border-border text-xs text-fg-muted hover:border-accent hover:text-fg bg-elevated transition-colors min-h-[36px]"
              >
                {option}
              </button>
            ))}
          </div>
        </div>
      )}

      {sqlResult && <SqlResultTable result={sqlResult} />}

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

      {/* Faithfulness verdict (plan 25), retained after the message is persisted. */}
      {verification && <VerifyBadge report={verification} />}

      {/* Citations */}
      {citations.length > 0 && <Citations citations={citations} />}

      {/* Provenance disclosure — shared with the agents screen. */}
      {isDone && provenance && <ProvenanceStrip provenance={provenance} />}

      {/* Follow-up suggestions — last assistant message only. */}
      {isDone && isLast && suggestions.length > 0 && onSuggestionSubmit && (
        <SuggestionChips suggestions={suggestions} onSubmit={onSuggestionSubmit} />
      )}
    </div>
  );
}
