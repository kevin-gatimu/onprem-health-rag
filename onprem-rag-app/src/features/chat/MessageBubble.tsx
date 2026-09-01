// A single chat message bubble — handles three display modes:
//   'persisted'           — a stored StoredMessage (user or assistant)
//   'optimistic-user'     — the in-flight user question (before server persists)
//   'optimistic-assistant'— the in-flight assistant answer (live-streaming)
import { useState } from 'react';
import { Copy, Check } from 'lucide-react';
import type { StoredMessage } from '../../lib/bridge';
import type { PendingRun } from '../../stores/chat';
import Markdown from '../../components/Markdown';
import Citations from './Citations';
import ActivityStrip from './ActivityStrip';
import VerifyBadge from './VerifyBadge';

type MessageBubbleProps =
  | { kind: 'persisted'; message: StoredMessage }
  | { kind: 'optimistic-user'; text: string }
  | { kind: 'optimistic-assistant'; pending: PendingRun };

export default function MessageBubble(props: MessageBubbleProps) {
  const [copied, setCopied] = useState(false);

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
  const isDone = props.kind === 'persisted'
    || props.pending.phase === 'done'
    || props.pending.phase === 'stopped';

  return (
    <div className="flex flex-col gap-1 max-w-[90%] px-1">
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

      {/* Faithfulness verdict (plan 25). Live-run only: the report rides on the
          pending run, not on the persisted message, so it disappears once the
          conversation is refetched from the DB. */}
      {pendingRun?.verify && <VerifyBadge report={pendingRun.verify} />}

      {/* Citations */}
      {citations.length > 0 && <Citations citations={citations} />}
    </div>
  );
}
