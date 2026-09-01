// Renders the full conversation: persisted messages from the server query, then
// the optimistic pending pair (user + assistant) when a run is in flight.
// Auto-scrolls to the bottom on new content and on mount.
import { useEffect, useMemo, useRef } from 'react';
import { MessageSquare } from 'lucide-react';
import type { StoredMessage } from '../../lib/bridge';
import type { PendingRun } from '../../stores/chat';
import MessageBubble from './MessageBubble';

interface MessageListProps {
  persisted: StoredMessage[];
  pending: PendingRun | null;
  activeConvId: string | null;
}

export default function MessageList({ persisted, pending, activeConvId }: MessageListProps) {
  const listRef = useRef<HTMLDivElement>(null);

  const showPending = pending !== null && pending.conversationId === activeConvId;

  // Mid-run refetches (e.g. mobile keyboard blur → refetchOnWindowFocus) can already
  // contain the persisted copy of the in-flight user message — the server saves it at
  // request start. Drop that trailing duplicate or the bubble renders twice.
  const visible = useMemo(() => {
    if (!showPending || pending === null) return persisted;
    const last = persisted[persisted.length - 1];
    if (last && last.role === 'user' && last.content === pending.user) {
      return persisted.slice(0, -1);
    }
    return persisted;
  }, [persisted, showPending, pending]);

  const hasContent = visible.length > 0 || showPending;

  // Scroll to bottom whenever content changes: new persisted messages, new tokens,
  // or phase transitions (e.g. citations arrive while answer is still empty).
  useEffect(() => {
    if (listRef.current) {
      listRef.current.scrollTop = listRef.current.scrollHeight;
    }
  }, [visible.length, pending?.answer, pending?.phase]);

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
      className="flex-1 overflow-y-auto px-3 py-4 min-h-0"
      aria-label="Conversation messages"
    >
      {/* Centered cap: keeps message bubbles in a comfortable reading column on wide
          monitors while the scrollbar stays at the pane edge. */}
      <div className="flex flex-col gap-4 max-w-4xl 3xl:max-w-5xl mx-auto w-full">
        {visible.map((msg) => (
          <MessageBubble key={msg.id} kind="persisted" message={msg} />
        ))}

        {showPending && pending !== null && (
          <>
            <MessageBubble kind="optimistic-user" text={pending.user} />
            <MessageBubble kind="optimistic-assistant" pending={pending} />
          </>
        )}
      </div>
    </div>
  );
}
