// Renders persisted messages, concurrent optimistic runs, and queued follow-ups.
import { useEffect, useRef } from 'react';
import { MessageSquare, RotateCcw, Square, X, Zap } from 'lucide-react';
import type { StoredMessage } from '../../lib/bridge';
import type { PendingRun, QueuedChatPrompt } from '../../stores/chat';
import MessageBubble from './MessageBubble';

interface MessageListProps {
  persisted: StoredMessage[];
  runs: PendingRun[];
  queued: QueuedChatPrompt[];
  activeConvId: string | null;
  onSendQueuedNow: (promptId: string) => void;
  onRemoveQueued: (promptId: string) => void;
  onRetry: (runId: string) => void;
  onStop: (runId: string) => void;
}

export default function MessageList({
  persisted,
  runs,
  queued,
  activeConvId,
  onSendQueuedNow,
  onRemoveQueued,
  onRetry,
  onStop,
}: MessageListProps) {
  const listRef = useRef<HTMLDivElement>(null);
  const pinnedToBottom = useRef(true);
  const previousConversationId = useRef(activeConvId);
  const hasContent = persisted.length > 0 || runs.length > 0 || queued.length > 0;

  useEffect(() => {
    if (previousConversationId.current !== activeConvId) {
      previousConversationId.current = activeConvId;
      pinnedToBottom.current = true;
    }
    if (!pinnedToBottom.current) return;
    const frame = requestAnimationFrame(() => {
      if (listRef.current) listRef.current.scrollTop = listRef.current.scrollHeight;
    });
    return () => cancelAnimationFrame(frame);
  }, [activeConvId, persisted.length, runs, queued.length]);

  function handleScroll() {
    const list = listRef.current;
    if (list) {
      pinnedToBottom.current = list.scrollHeight - list.scrollTop - list.clientHeight < 48;
    }
  }

  if (!hasContent) {
    return (
      <div className="flex-1 flex flex-col items-center justify-center gap-3 py-16 text-center px-4">
        <MessageSquare size={40} className="text-fg-subtle" aria-hidden="true" />
        <div>
          <p className="font-medium text-fg">Ask a question</p>
          <p className="text-sm text-fg-muted mt-1 max-w-xs">
            Ask about your health records and get grounded answers with source citations.
          </p>
        </div>
      </div>
    );
  }

  return (
    <div
      ref={listRef}
      onScroll={handleScroll}
      className="flex-1 overflow-y-auto px-3 py-4 min-h-0"
      aria-label="Conversation messages"
      aria-live="polite"
      aria-busy={runs.some((run) => run.phase !== 'done' && run.phase !== 'stopped')}
    >
      {/* Centered cap: keeps message bubbles in a comfortable reading column on wide
          monitors while the scrollbar stays at the pane edge. */}
      <div className="flex flex-col gap-4 max-w-4xl 3xl:max-w-5xl mx-auto w-full">
        {visible.map((msg) => (
          <MessageBubble key={msg.id} kind="persisted" message={msg} />
        ))}

        {runs.map((run) => (
          <div key={run.runId} className="contents">
            <MessageBubble kind="optimistic-user" text={run.user} />
            <MessageBubble kind="optimistic-assistant" pending={run} />
            {run.phase !== 'done' && run.phase !== 'stopped' && (
              <button
                onClick={() => onStop(run.runId)}
                className="self-start ml-11 inline-flex items-center gap-1 text-xs text-fg-muted hover:text-fg"
              >
                <Square size={11} fill="currentColor" aria-hidden="true" />
                Stop generating
              </button>
            )}
            {(run.error || run.phase === 'stopped') && (
              <button
                onClick={() => onRetry(run.runId)}
                className="self-start ml-11 inline-flex items-center gap-1 text-xs text-accent hover:text-accent-hover"
              >
                <RotateCcw size={12} aria-hidden="true" />
                Retry
              </button>
            )}
          </div>
        ))}

        {queued.map((prompt, index) => (
          <div key={prompt.id} className="ml-auto max-w-[85%] rounded-lg border border-border bg-elevated px-3 py-2">
            <p className="text-sm text-fg">{prompt.user}</p>
            <div className="mt-1 flex items-center justify-end gap-2 text-xs text-fg-muted">
              <span>Queued · {index + 1}</span>
              <button
                onClick={() => onRemoveQueued(prompt.id)}
                className="inline-flex items-center gap-1 hover:text-fg"
                aria-label="Remove queued message"
              >
                <X size={12} aria-hidden="true" />
                Remove
              </button>
              <button
                onClick={() => onSendQueuedNow(prompt.id)}
                className="inline-flex items-center gap-1 text-accent hover:text-accent-hover"
              >
                <Zap size={12} aria-hidden="true" />
                Send now
              </button>
            </div>
          </div>
        ))}
      </div>
    </div>
  );
}
