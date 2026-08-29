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
// All controls lock while pending.phase !== 'done' (single in-flight run invariant).
import { useState, useCallback } from 'react';
import { useQuery, useQueryClient } from '@tanstack/react-query';
import { Layers, PanelLeftOpen } from 'lucide-react';
import {
  listConversations,
  createConversation,
  getMessages,
  chat,
} from '../../lib/bridge';
import type { RetrievalOpts } from '../../lib/bridge';
import { useChat } from '../../stores/chat';
import { toast } from '../../stores/ui';
import { Button, Modal } from '../../components/ui';
import ConversationList from './ConversationList';
import MessageList from './MessageList';
import Composer from './Composer';
import RetrievalSettings from './RetrievalSettings';

export default function Chat() {
  const queryClient = useQueryClient();

  // Component-local view state — none of this needs cross-feature persistence.
  const [activeConvId, setActiveConvId] = useState<string | null>(null);
  const [drawerOpen, setDrawerOpen] = useState(false);
  const [settingsOpen, setSettingsOpen] = useState(false);
  const [opts, setOpts] = useState<RetrievalOpts>({});

  // Derived streaming gate: lock UI while a run is in flight.
  const pending = useChat((s) => s.pending);
  const streaming = pending !== null && pending.phase !== 'done';

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

  // ── Send flow ────────────────────────────────────────────────────────────────
  const handleSend = useCallback(async (text: string) => {
    const runId = crypto.randomUUID();

    // Ensure we have a conversation to attach messages to.
    let convId: string;
    if (activeConvId) {
      convId = activeConvId;
    } else {
      try {
        const c = await createConversation();
        convId = c.id;
        setActiveConvId(c.id);
        void queryClient.invalidateQueries({ queryKey: ['conversations'] });
      } catch (e) {
        toast.error(`Failed to start conversation: ${String(e)}`);
        return;
      }
    }

    // Optimistic: register the run before the bridge invoke so the activity strip
    // appears immediately and the user bubble is visible during the call.
    useChat.getState().startRun(runId, convId, text);

    try {
      // Pass history: [] — the server loads history from DB when conversationId is set.
      await chat(text, [], opts, convId, runId);
      // Await the messages re-fetch BEFORE clearing pending so the optimistic
      // bubbles are replaced atomically (no duplicate-bubble flash).
      await queryClient.invalidateQueries({ queryKey: ['messages', convId] });
      void queryClient.invalidateQueries({ queryKey: ['conversations'] }); // refresh title/updated_at
      useChat.getState().clear();
    } catch (e) {
      useChat.getState().setError(runId, String(e));
    }
  }, [activeConvId, opts, queryClient]);

  // ── Navigation helpers ───────────────────────────────────────────────────────
  function handleNewChat() {
    setActiveConvId(null);
    useChat.getState().clear();
    setDrawerOpen(false);
  }

  function handleSelectConv(id: string) {
    if (id === activeConvId) return;
    setActiveConvId(id);
    useChat.getState().clear();
    setDrawerOpen(false);
  }

  function handleActiveDeleted() {
    setActiveConvId(null);
    useChat.getState().clear();
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
          disabled={streaming}
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
              streaming={streaming}
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
            pending={pending}
            activeConvId={activeConvId}
          />
          <Composer
            onSend={handleSend}
            streaming={streaming}
            onOpenSettings={() => setSettingsOpen(true)}
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
          streaming={streaming}
          onSelect={handleSelectConv}
          onNewChat={handleNewChat}
          onActiveDeleted={handleActiveDeleted}
          onClose={() => setDrawerOpen(false)}
        />
      </Modal>

      {/* Retrieval settings bottom-sheet */}
      <RetrievalSettings
        open={settingsOpen}
        onClose={() => setSettingsOpen(false)}
        opts={opts}
        onOptsChange={setOpts}
      />
    </div>
  );
}
