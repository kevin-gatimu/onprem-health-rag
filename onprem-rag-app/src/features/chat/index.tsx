// Chat screen — Stage 6. Mobile-first, ships as desktop (Tauri) + Android.
//
// Layout:
//   mobile  (< md):  TopBar + mobile "Chats" button → Modal drawer + message list + composer
//   md+:             Left conversation rail (260px) + message pane with sticky composer
//
// Send flow (see 09-chat-implementation.md decision #6/#7):
//   1. Create conversation if none (server auto-titles on first message).
//   2. startRun (optimistic state) → await chat() (server SSE → bridge events).
//   3. On success: await invalidateQueries(['messages']) then clear().
//   4. On error: setError (rendered inline in the bubble).
//
// Runs are conversation-scoped, so navigation and the composer remain usable while streaming.
import { useState, useCallback } from 'react';
import { useQuery } from '@tanstack/react-query';
import { Layers, PanelLeftOpen } from 'lucide-react';
import { listConversations, getMessages } from '../../lib/bridge';
import {
  retryChatRun,
  sendQueuedChatNow,
  stopChatRun,
  submitChatPrompt,
} from '../../lib/conversationRuntime';
import { useChat } from '../../stores/chat';
import { toast } from '../../stores/ui';
import { Button, Modal } from '../../components/ui';
import ConversationList from './ConversationList';
import MessageList from './MessageList';
import Composer from './Composer';

export default function Chat() {
  const [drawerOpen, setDrawerOpen] = useState(false);
  const activeConvId = useChat((state) => state.activeConversationId);
  const allRuns = useChat((state) => state.runs);
  const queues = useChat((state) => state.queues);
  const draftKey = activeConvId ?? '__new__';
  const draft = useChat((state) => state.drafts[draftKey] ?? '');
  const runs = Object.values(allRuns)
    .filter((run) => run.conversationId === activeConvId)
    .sort((a, b) => a.startedAt - b.startedAt);
  const queued = activeConvId ? (queues[activeConvId] ?? []) : [];
  const busyConversationIds = new Set(
    Object.values(allRuns)
      .filter((run) => run.phase !== 'done' && run.phase !== 'stopped')
      .map((run) => run.conversationId),
  );
  const busy = activeConvId !== null && busyConversationIds.has(activeConvId);

  // ── Queries ─────────────────────────────────────────────────────────────────
  const { data: conversations = [] } = useQuery({
    queryKey: ['conversations'],
    queryFn: listConversations,
    staleTime: 30_000,
  });

  const { data: messages = [] } = useQuery({
    queryKey: ['messages', activeConvId],
    queryFn: () => getMessages(activeConvId!),
    enabled: !!activeConvId,
    staleTime: 30_000,
  });

  const handleSend = useCallback((text: string, sendImmediately: boolean) => {
    // Retrieval policy is selected server-side from the routed query class.
    void submitChatPrompt(activeConvId, text, {}, sendImmediately).catch((error) => {
      toast.error(`Failed to start conversation: ${String(error)}`);
    });
  }, [activeConvId]);

  function handleNewChat() {
    useChat.getState().setActiveConversation(null);
    setDrawerOpen(false);
  }

  function handleSelectConv(id: string) {
    useChat.getState().setActiveConversation(id);
    setDrawerOpen(false);
  }

  function handleActiveDeleted() {
    useChat.getState().setActiveConversation(null);
  }

  // ── Active conversation title for the mobile header ──────────────────────────
  const activeTitle = activeConvId
    ? (conversations.find((c) => c.id === activeConvId)?.title ?? 'Chat')
    : 'New Chat';

  // ── Render ───────────────────────────────────────────────────────────────────
  return (
    <div className="flex flex-col h-full">
      {/* Mobile top row: Chats button + conversation title */}
      <div className="md:hidden flex items-center gap-2 pb-3 flex-shrink-0">
        <Button
          variant="secondary"
          size="sm"
          leftIcon={<PanelLeftOpen size={15} aria-hidden="true" />}
          onClick={() => setDrawerOpen(true)}
          className="min-h-[44px]"
        >
          Chats
        </Button>
        <h2 className="text-sm font-semibold text-fg flex-1 truncate text-center">
          {activeTitle}
        </h2>
        {/* Spacer balances the Chats button on the left */}
        <div className="w-[72px]" aria-hidden="true" />
      </div>

      {/* Main two-column (md+) / single-column (mobile) layout */}
      <div className="flex-1 min-h-0 flex flex-col md:grid md:grid-cols-[260px_minmax(0,1fr)] rounded-lg border border-border overflow-hidden">

        {/* Left rail — desktop only; mirrors ConnectionTree pattern from data-explorer */}
        <aside className="hidden md:flex md:flex-col border-r border-border bg-surface">
          <div className="flex items-center gap-2 px-3 py-2.5 border-b border-border text-xs font-semibold text-fg-muted uppercase tracking-wide flex-shrink-0">
            <Layers size={13} aria-hidden="true" />
            Conversations
          </div>
          <div className="flex-1 overflow-y-auto p-2">
            <ConversationList
              conversations={conversations}
              activeConvId={activeConvId}
           busyConversationIds={busyConversationIds}
              onSelect={handleSelectConv}
              onNewChat={handleNewChat}
              onActiveDeleted={handleActiveDeleted}
            />
          </div>
        </aside>

        {/* Right pane: message list + composer */}
        <div className="flex-1 min-h-0 flex flex-col bg-base">
          <MessageList
            persisted={messages}
            runs={runs}
            queued={queued}
            activeConvId={activeConvId}
            onSendQueuedNow={(promptId) => {
              if (activeConvId) sendQueuedChatNow(activeConvId, promptId);
            }}
            onRemoveQueued={(promptId) => {
              if (activeConvId) useChat.getState().removeQueued(activeConvId, promptId);
            }}
            onRetry={retryChatRun}
            onStop={stopChatRun}
          />
          <Composer
            text={draft}
            onTextChange={(text) => useChat.getState().setDraft(draftKey, text)}
            onSend={handleSend}
            busy={busy}
          />
        </div>
      </div>

      {/* Mobile conversation drawer */}
      <Modal
        open={drawerOpen}
        onClose={() => setDrawerOpen(false)}
        title="Conversations"
        size="md"
      >
        <ConversationList
          conversations={conversations}
          activeConvId={activeConvId}
          busyConversationIds={busyConversationIds}
          onSelect={handleSelectConv}
          onNewChat={handleNewChat}
          onActiveDeleted={handleActiveDeleted}
          onClose={() => setDrawerOpen(false)}
        />
      </Modal>
    </div>
  );
}
