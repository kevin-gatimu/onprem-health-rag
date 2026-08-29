// AI Agents screen — Stage 7. Mobile-first, ships as desktop (Tauri) + Android.
//
// Layout:
//   mobile  (< md):  Kind-selector tabs + TopBar (drawer button / title / info) +
//                    message list + composer. Conversation drawer and data panel
//                    open as Modals.
//   md+:             Kind-selector tabs + left conversation rail (260 px) + message pane.
//   xl+:             Same as md but with a third context column (220 px) on the right.
//
// Agent kinds: auto / health_query / trends / patient_lookup / summarize.
// Switching kinds resets the active conversation and clears pending state (each
// kind maintains its own conversation partition on the server).
//
// Send flow mirrors chat/index.tsx exactly, using the agent() bridge call.
import { useState, useCallback } from 'react';
import { useQuery, useQueryClient } from '@tanstack/react-query';
import { PanelLeftOpen, Layers, Bot, Info } from 'lucide-react';
import {
  listAgentConversations,
  createConversation,
  getMessages,
  agent,
  getStats,
  getIngestHistory,
} from '../../lib/bridge';
import type { AgentKind } from '../../lib/bridge';
import { useAgents } from '../../stores/agents';
import { toast } from '../../stores/ui';
import { Button, Modal } from '../../components/ui';
import ConversationList from './ConversationList';
import MessageList from './MessageList';
import Composer from './Composer';

// ── Kind selector config ─────────────────────────────────────────────────────

interface KindTab {
  kind: AgentKind;
  label: string;
}

// No 'chat' tab — 'chat' is only a routed *result* the server emits, never a
// user-selectable input.
const KIND_TABS: KindTab[] = [
  { kind: 'auto',           label: 'Auto' },
  { kind: 'health_query',   label: 'Health Query' },
  { kind: 'trends',         label: 'Trends' },
  { kind: 'patient_lookup', label: 'Patient Lookup' },
  { kind: 'summarize',      label: 'Summarize' },
];

const KIND_BLURBS: Record<string, string> = {
  auto:           'Automatically selects the best agent for your question.',
  health_query:   'Runs structured queries against health record data and visualises the results.',
  trends:         'Detects patterns and trends across patient records over time.',
  patient_lookup: 'Finds and summarises records for a specific patient.',
  summarize:      'Produces a concise narrative summary of selected records.',
};

// ── Component ────────────────────────────────────────────────────────────────

export default function Agents() {
  const queryClient = useQueryClient();

  // Component-local view state — none needs cross-feature persistence.
  const [activeConvId, setActiveConvId] = useState<string | null>(null);
  const [drawerOpen, setDrawerOpen] = useState(false);
  const [panelOpen, setPanelOpen] = useState(false);
  const [selectedKind, setSelectedKind] = useState<AgentKind>('auto');

  // Derived streaming gate: lock UI while a run is in flight.
  const pending = useAgents((s) => s.pending);
  const streaming = pending !== null && pending.phase !== 'done';

  // ── Queries ─────────────────────────────────────────────────────────────────
  const { data: conversations = [] } = useQuery({
    queryKey: ['agent-conversations', selectedKind],
    queryFn: () => listAgentConversations(selectedKind),
    staleTime: 30_000,
  });

  const { data: messages = [] } = useQuery({
    queryKey: ['messages', activeConvId],
    queryFn: () => getMessages(activeConvId!),
    enabled: !!activeConvId,
    staleTime: 30_000,
  });

  // Context panel: stats + ingest history (secondary data, soft failure OK).
  const { data: stats } = useQuery({
    queryKey: ['stats'],
    queryFn: getStats,
    staleTime: 30_000,
  });

  const { data: ingestHistory = [] } = useQuery({
    queryKey: ['ingest-history'],
    queryFn: getIngestHistory,
    staleTime: 30_000,
  });

  const indexedTableCount = ingestHistory.reduce((acc, c) => acc + c.tables.length, 0);

  // ── Send flow ────────────────────────────────────────────────────────────────
  const handleSend = useCallback(async (text: string) => {
    const runId = crypto.randomUUID();

    // Ensure we have a conversation to attach messages to.
    let convId: string;
    if (activeConvId) {
      convId = activeConvId;
    } else {
      try {
        const c = await createConversation(undefined, selectedKind);
        convId = c.id;
        setActiveConvId(c.id);
        void queryClient.invalidateQueries({ queryKey: ['agent-conversations', selectedKind] });
      } catch (e) {
        toast.error(`Failed to start conversation: ${String(e)}`);
        return;
      }
    }

    // Optimistic: register the run before the bridge invoke so the activity strip
    // appears immediately and the user bubble is visible during the call.
    useAgents.getState().startRun(runId, convId, selectedKind, text);

    try {
      await agent(selectedKind, text, convId, runId);
      // Await the messages re-fetch BEFORE clearing pending so the optimistic
      // bubbles are replaced atomically (no duplicate-bubble flash).
      await queryClient.invalidateQueries({ queryKey: ['messages', convId] });
      void queryClient.invalidateQueries({ queryKey: ['agent-conversations', selectedKind] });
      useAgents.getState().clear();
    } catch (e) {
      useAgents.getState().setError(runId, String(e));
    }
  }, [activeConvId, selectedKind, queryClient]);

  // ── Navigation helpers ───────────────────────────────────────────────────────
  function handleNewChat() {
    setActiveConvId(null);
    useAgents.getState().clear();
    setDrawerOpen(false);
  }

  function handleSelectConv(id: string) {
    if (id === activeConvId) return;
    setActiveConvId(id);
    useAgents.getState().clear();
    setDrawerOpen(false);
  }

  function handleActiveDeleted() {
    setActiveConvId(null);
    useAgents.getState().clear();
  }

  // Switching agent kind resets active conversation — each kind is a separate partition.
  function handleKindChange(kind: AgentKind) {
    if (kind === selectedKind) return;
    setSelectedKind(kind);
    setActiveConvId(null);
    useAgents.getState().clear();
  }

  // ── Active conversation title for the mobile header ──────────────────────────
  const activeTitle = activeConvId
    ? (conversations.find((c) => c.id === activeConvId)?.title ?? 'Agent Chat')
    : 'New Chat';

  // ── Render ───────────────────────────────────────────────────────────────────
  return (
    <div className="flex flex-col h-full">

      {/* Agent kind selector — horizontal-scroll strip (mobile) / pill tabs (md+) */}
      <div
        className="flex-shrink-0 flex items-center gap-1 pb-3 overflow-x-auto"
        role="tablist"
        aria-label="Agent type"
      >
        {KIND_TABS.map((tab) => (
          <button
            key={tab.kind}
            role="tab"
            aria-selected={selectedKind === tab.kind}
            onClick={() => handleKindChange(tab.kind)}
            disabled={streaming}
            className={[
              'shrink-0 px-3 py-2 rounded-md text-sm font-medium transition-colors whitespace-nowrap min-h-[44px]',
              selectedKind === tab.kind
                ? 'bg-accent-subtle text-fg'
                : 'text-fg-muted hover:bg-elevated hover:text-fg',
              streaming ? 'opacity-50 cursor-not-allowed' : '',
            ].join(' ')}
          >
            {tab.label}
          </button>
        ))}
      </div>

      {/* Mobile top row: drawer button + active title + data overview toggle */}
      <div className="md:hidden flex items-center gap-2 pb-3 flex-shrink-0">
        <Button
          variant="secondary"
          size="sm"
          leftIcon={<PanelLeftOpen size={15} aria-hidden="true" />}
          onClick={() => setDrawerOpen(true)}
          disabled={streaming}
          className="min-h-[44px]"
        >
          Agents
        </Button>
        <h2 className="text-sm font-semibold text-fg flex-1 truncate text-center">
          {activeTitle}
        </h2>
        <button
          onClick={() => setPanelOpen(true)}
          className="flex items-center justify-center w-9 h-9 text-fg-muted hover:text-fg hover:bg-elevated rounded-md min-h-[44px]"
          aria-label="Data overview"
        >
          <Info size={16} aria-hidden="true" />
        </button>
      </div>

      {/* Main layout: single column (mobile) → two column (md+) → three column (xl+) */}
      <div className="flex-1 min-h-0 flex flex-col md:grid md:grid-cols-[260px_minmax(0,1fr)] xl:grid-cols-[260px_minmax(0,1fr)_220px] rounded-lg border border-border overflow-hidden">

        {/* Left rail — desktop only; conversation list */}
        <aside className="hidden md:flex md:flex-col border-r border-border bg-surface">
          <div className="flex items-center gap-2 px-3 py-2.5 border-b border-border text-xs font-semibold text-fg-muted uppercase tracking-wide flex-shrink-0">
            <Layers size={13} aria-hidden="true" />
            Conversations
          </div>
          <div className="flex-1 overflow-y-auto p-2">
            <ConversationList
              conversations={conversations}
              activeConvId={activeConvId}
              selectedKind={selectedKind}
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
          />
        </div>

        {/* Context panel — xl+ third column; hidden on smaller screens */}
        <aside className="hidden xl:flex xl:flex-col border-l border-border bg-surface overflow-y-auto p-3 gap-3">
          <h3 className="text-xs font-semibold text-fg-muted uppercase tracking-wide flex items-center gap-1.5 flex-shrink-0">
            <Bot size={12} aria-hidden="true" />
            Data Overview
          </h3>

          {/* Stat counts from GET /stats */}
          {stats && (
            <div className="flex flex-col gap-1.5 text-xs flex-shrink-0">
              <div className="flex justify-between items-center">
                <span className="text-fg-muted">Connections</span>
                <span className="font-medium text-fg">{stats.active_connections}</span>
              </div>
              <div className="flex justify-between items-center">
                <span className="text-fg-muted">Tables</span>
                <span className="font-medium text-fg">{stats.total_tables}</span>
              </div>
              <div className="flex justify-between items-center">
                <span className="text-fg-muted">Records</span>
                <span className="font-medium text-fg">{stats.total_records.toLocaleString()}</span>
              </div>
              {indexedTableCount > 0 && (
                <div className="flex justify-between items-center">
                  <span className="text-fg-muted">Indexed tables</span>
                  <span className="font-medium text-fg">{indexedTableCount}</span>
                </div>
              )}
            </div>
          )}

          {/* Per-agent blurb */}
          <div className="border-t border-border pt-2 flex-shrink-0">
            <p className="text-xs font-medium text-fg mb-1">
              {KIND_TABS.find((t) => t.kind === selectedKind)?.label ?? 'Agent'}
            </p>
            <p className="text-xs text-fg-muted leading-relaxed">
              {KIND_BLURBS[selectedKind] ?? ''}
            </p>
          </div>
        </aside>
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
          selectedKind={selectedKind}
          streaming={streaming}
          onSelect={handleSelectConv}
          onNewChat={handleNewChat}
          onActiveDeleted={handleActiveDeleted}
          onClose={() => setDrawerOpen(false)}
        />
      </Modal>

      {/* Mobile data overview panel */}
      <Modal
        open={panelOpen}
        onClose={() => setPanelOpen(false)}
        title="Data Overview"
        size="sm"
      >
        <div className="flex flex-col gap-3 py-2">
          {stats && (
            <div className="flex flex-col gap-2 text-sm">
              <div className="flex justify-between">
                <span className="text-fg-muted">Active connections</span>
                <span className="font-medium text-fg">{stats.active_connections}</span>
              </div>
              <div className="flex justify-between">
                <span className="text-fg-muted">Tables</span>
                <span className="font-medium text-fg">{stats.total_tables}</span>
              </div>
              <div className="flex justify-between">
                <span className="text-fg-muted">Records</span>
                <span className="font-medium text-fg">{stats.total_records.toLocaleString()}</span>
              </div>
              {indexedTableCount > 0 && (
                <div className="flex justify-between">
                  <span className="text-fg-muted">Indexed tables</span>
                  <span className="font-medium text-fg">{indexedTableCount}</span>
                </div>
              )}
            </div>
          )}
          <div className="border-t border-border pt-3">
            <p className="text-sm font-medium text-fg mb-1">
              {KIND_TABS.find((t) => t.kind === selectedKind)?.label ?? 'Agent'}
            </p>
            <p className="text-sm text-fg-muted leading-relaxed">
              {KIND_BLURBS[selectedKind] ?? ''}
            </p>
          </div>
        </div>
      </Modal>
    </div>
  );
}
