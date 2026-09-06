// Hospital Agents screen — Plan 07. Mobile-first, ships as desktop (Tauri) + Android.
//
// Layout:
//   mobile  (< md):  Kind-selector tabs + TopBar (drawer button / title / info) +
//                    message list + composer. Conversation drawer and scope panel
//                    open as Modals.
//   md+:             Kind-selector tabs + left conversation rail (260 px) + message pane.
//   xl+:             Same as md but with a third Scope panel column (220 px) on the right.
//
// Agent tabs come from the server roster (GET /agents via useAgentRegistry).
// Falls back to legacy kinds when the server hasn't implemented plan 05 yet.
// No service-line slug or label is hardcoded in this file — everything is registry-driven.
//
// Tier 1 agents are shown directly; Tier 2/3 collapse into a "More ▾" menu.
// Unusable agents appear greyed in "More" with a tooltip.
//
// Mode toggle (Ask / Trends / Handover) appears on the right of the tab bar for
// agents that support more than one mode.
import { useState, useCallback, useEffect, useRef } from 'react';
import { useQuery } from '@tanstack/react-query';
import { PanelLeftOpen, Layers, ChevronDown, Bot, AlertTriangle, Info } from 'lucide-react';
import {
  listAgentConversations,
  getMessages,
} from '../../lib/bridge';
import type { AgentKind, AgentMode } from '../../lib/bridge';
import {
  retryAgentRun,
  sendQueuedAgentNow,
  stopAgentRun,
  submitAgentPrompt,
} from '../../lib/conversationRuntime';
import { useAgents } from '../../stores/agents';
import { useAgentRegistry, ASK_KIND } from '../../stores/agentRegistry';
import { toast, useUi } from '../../stores/ui';
import { Button, Modal, EmptyState } from '../../components/ui';
import ConversationList from './ConversationList';
import MessageList from './MessageList';
import Composer from './Composer';
import ScopePanel from './ScopePanel';

// ── Component ────────────────────────────────────────────────────────────────

export default function Agents() {
  const [drawerOpen, setDrawerOpen] = useState(false);
  const [panelOpen, setPanelOpen] = useState(false);
  const [moreOpen, setMoreOpen] = useState(false);
  // Coverage strip for Ask — default collapsed so it doesn't push the chat area.
  const [coverageOpen, setCoverageOpen] = useState(false);

  // Ref wrapping the More button + its menu so the pointerdown dismissal can check
  // whether the click was inside the compound control.
  const moreRef = useRef<HTMLDivElement>(null);
  // Ref on the trigger button so Escape can return focus to it.
  const moreButtonRef = useRef<HTMLButtonElement>(null);

  const activeConvId = useAgents((state) => state.activeConversationId);
  const selectedKind = useAgents((state) => state.selectedKind);
  const allRuns = useAgents((state) => state.runs);
  const queues = useAgents((state) => state.queues);
  const modes = useAgents((state) => state.modes);

  // Draft key encodes both kind and conv so switching tabs preserves composer text.
  const draftKey = `${selectedKind}:${activeConvId ?? '__new__'}`;
  const draft = useAgents((state) => state.drafts[draftKey] ?? '');

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

  // Active mode. Keyed like the draft, so a mode can be chosen BEFORE the first
  // message exists — `activeConversationId` is null until the server mints the
  // conversation, and a toggle that silently does nothing until you have already
  // sent a turn is the wrong affordance.
  const modeKey = draftKey;
  const activeMode: AgentMode = modes[modeKey] ?? 'ask';

  // ── Registry ─────────────────────────────────────────────────────────────────
  const registry = useAgentRegistry();
  useEffect(() => {
    // Load once after mount; the store is idempotent so this is safe on re-mount.
    void registry.load();
  }, []);

  const usableAgents = registry.usable();
  const tier1 = usableAgents.filter((a) => a.tier === 1);
  const tierMore = usableAgents.filter((a) => a.tier !== 1);
  // Unusable agents for the "More" menu (greyed, no route).
  const unusableAgents = registry.agents.filter((a) => !a.usable);

  // All service lines excluding Ask — used for the coverage strip count.
  const departments = registry.agents.filter((a) => a.kind !== ASK_KIND);

  const currentAgent = registry.byKind(selectedKind);
  const registrySource = registry.source;
  const registryError = registry.error;

  // Navigation helper — used for the "Open Connections" link in the notice below.
  const navigate = useUi((s) => s.navigate);

  // ── More-menu dismissal — active only while the menu is open ─────────────────
  useEffect(() => {
    if (!moreOpen) return;

    function onPointerDown(e: PointerEvent) {
      if (moreRef.current && !moreRef.current.contains(e.target as Node)) {
        setMoreOpen(false);
      }
    }

    function onKeyDown(e: KeyboardEvent) {
      if (e.key === 'Escape') {
        setMoreOpen(false);
        moreButtonRef.current?.focus();
      }
    }

    document.addEventListener('pointerdown', onPointerDown);
    document.addEventListener('keydown', onKeyDown);
    return () => {
      document.removeEventListener('pointerdown', onPointerDown);
      document.removeEventListener('keydown', onKeyDown);
    };
  }, [moreOpen]);

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

  // ── Handlers ─────────────────────────────────────────────────────────────────
  // Every turn carries the conversation's current mode (plan 07 §4: "Mode is sent
  // with each prompt and shown as a badge on the assistant bubble"). The bridge
  // forwards it as the `mode` body field; see the finding on `mode` in the report —
  // the current server ignores it, which is why the toggle only appears for agents
  // whose roster entry advertises more than one mode.
  const handleSend = useCallback((text: string, sendImmediately: boolean) => {
    void submitAgentPrompt(activeConvId, selectedKind, text, sendImmediately, {
      mode: activeMode,
    })
      .then((resolvedId) => {
        // Carry the pre-send mode onto the conversation the server just minted,
        // so the toggle does not snap back to Ask after the first turn. The key
        // must match the reader's `${kind}:${convId}` form, not the bare id.
        if (!activeConvId) {
          useAgents.getState().setMode(`${selectedKind}:${resolvedId}`, activeMode);
        }
      })
      .catch((error) => {
        toast.error(`Failed to start conversation: ${String(error)}`);
      });
  }, [activeConvId, selectedKind, activeMode]);

  /**
   * A suggestion chip or clarify option click. Plan 07 §5.4: this is a NORMAL
   * turn — the chip's text becomes the user message, never a hidden action.
   * A `switch` suggestion also moves the tab before sending, so the answer lands
   * in the line that can serve it.
   */
  const handleSuggestionSubmit = useCallback(
    (text: string, opts?: { suggestionSpec?: unknown; switchKind?: string }) => {
      const targetKind = opts?.switchKind ?? selectedKind;
      if (opts?.switchKind && opts.switchKind !== selectedKind) {
        useAgents.getState().setSelectedKind(opts.switchKind);
      }
      void submitAgentPrompt(activeConvId, targetKind, text, true, {
        mode: activeMode,
        suggestionSpec: opts?.suggestionSpec,
      })
        .then((resolvedId) => {
          if (!activeConvId) {
            useAgents.getState().setMode(`${targetKind}:${resolvedId}`, activeMode);
          }
        })
        .catch((error) => {
          toast.error(`Failed to send: ${String(error)}`);
        });
    },
    [activeConvId, selectedKind, activeMode],
  );

  /**
   * "Open in {Line}" — switch tab and carry the conversation across.
   *
   * CLIENT-SIDE ONLY. Plan 07 §4 asks for the conversation's `agent_kind` to be
   * changed server-side via `PATCH /conversations/<id>`, but that route accepts
   * only `{ title }` (`onprem-rag-server/src/routes/conversations.rs`), and this
   * workstream must not edit the server crate. So the tab moves and the same
   * conversation stays open, but the stored `agent_kind` is unchanged: after a
   * reload the conversation reappears under its original tab. Reported as a
   * blocked deliverable rather than worked around with a delete-and-recreate,
   * which would lose the message history.
   */
  const handleSwitchAgent = useCallback(
    (kind: string) => {
      if (kind === selectedKind) return;
      const conv = activeConvId;
      useAgents.getState().setSelectedKind(kind);
      if (conv) useAgents.getState().setActiveConversation(conv);
    },
    [selectedKind, activeConvId],
  );

  const handlePickExample = useCallback(
    (question: string) => {
      useAgents.getState().setDraft(draftKey, question);
      setPanelOpen(false);
    },
    [draftKey],
  );

  function handleNewChat() {
    useAgents.getState().setActiveConversation(null);
    setDrawerOpen(false);
  }

  function handleSelectConv(id: string) {
    useAgents.getState().setActiveConversation(id);
    setDrawerOpen(false);
  }

  function handleActiveDeleted() {
    useAgents.getState().setActiveConversation(null);
  }

  function handleKindChange(kind: AgentKind) {
    if (kind !== selectedKind) useAgents.getState().setSelectedKind(kind);
    setMoreOpen(false);
  }

  function handleModeChange(mode: AgentMode) {
    useAgents.getState().setMode(modeKey, mode);
  }

  // ── Mode toggle — only shown when the current agent supports multiple modes ──
  const agentModes = currentAgent?.modes ?? ['ask'];
  const modeToggle = agentModes.length > 1 ? (
    <div
      className="flex items-center gap-0.5 shrink-0 rounded-md border border-border bg-base p-0.5"
      role="group"
      aria-label="Mode"
    >
      {(['ask', 'trends', 'handover'] as AgentMode[])
        .filter((m) => agentModes.includes(m))
        .map((m) => (
          <button
            key={m}
            onClick={() => handleModeChange(m)}
            aria-pressed={activeMode === m}
            className={[
              'px-2.5 py-1 rounded text-xs font-medium transition-colors capitalize min-h-[32px]',
              activeMode === m
                ? 'bg-elevated text-fg'
                : 'text-fg-muted hover:text-fg',
            ].join(' ')}
          >
            {m}
          </button>
        ))}
    </div>
  ) : null;

  // ── Kind selector ─────────────────────────────────────────────────────────────
  //
  // The More button and its menu live OUTSIDE the overflow-x-auto tablist.
  // Per CSS spec, overflow-x:auto forces overflow-y to a non-visible value, so
  // an absolute child positioned below the scroll box would be clipped — z-50
  // cannot escape a clipping ancestor. Moving More to a sibling div (no overflow
  // on the parent flex row) lets the absolute menu render freely.
  //
  // Ancestor chain of the menu: relative shrink-0 div → flex items-center gap-2
  // (no overflow) → flex flex-col h-full root (no overflow). Safe for absolute.
  const moreCount = tierMore.length + unusableAgents.length;
  const kindSelector = (
    <div className="flex-shrink-0 flex items-center pb-3 gap-2">
      {/* Scrollable tablist — contains ONLY role="tab" buttons, no interactive
          non-tab elements (More is a menu trigger, not a tab). */}
      <div
        className="flex items-center gap-1 overflow-x-auto min-w-0 flex-1"
        role="tablist"
        aria-label="Agent"
      >
        {tier1.map((tab) => (
          <button
            key={tab.kind}
            role="tab"
            aria-selected={selectedKind === tab.kind}
            onClick={() => handleKindChange(tab.kind)}
            className={[
              'shrink-0 px-3 py-2 rounded-md text-sm font-medium transition-colors whitespace-nowrap min-h-[44px]',
              selectedKind === tab.kind
                ? 'bg-accent-subtle text-fg'
                : 'text-fg-muted hover:bg-elevated hover:text-fg',
            ].join(' ')}
          >
            {tab.label}
          </button>
        ))}
      </div>

      {/* More menu — sibling to the tablist so overflow-x-auto never clips it */}
      {moreCount > 0 && (
        <div ref={moreRef} className="relative shrink-0">
          <button
            ref={moreButtonRef}
            onClick={() => setMoreOpen((v) => !v)}
            aria-haspopup="menu"
            aria-expanded={moreOpen}
            className={[
              'flex items-center gap-1 px-3 py-2 rounded-md text-sm font-medium transition-colors whitespace-nowrap min-h-[44px]',
              tierMore.some((a) => a.kind === selectedKind)
                ? 'bg-accent-subtle text-fg'
                : 'text-fg-muted hover:bg-elevated hover:text-fg',
            ].join(' ')}
          >
            More
            {/* Subdued count — not a loud badge, just a muted numeral */}
            <span className="text-fg-subtle font-normal text-xs" aria-hidden="true">
              {moreCount}
            </span>
            <ChevronDown size={13} aria-hidden="true" />
          </button>

          {moreOpen && (
            <div
              role="menu"
              className="absolute top-full left-0 mt-1 w-64 bg-elevated border border-border rounded-lg shadow-md z-50 py-1 max-h-[70vh] overflow-y-auto"
            >
              {/* Usable tier 2/3 departments */}
              {tierMore.length > 0 && (
                <>
                  <div
                    className="px-3 pt-1.5 pb-1 text-xs font-semibold text-fg-subtle uppercase tracking-wide select-none"
                    aria-hidden="true"
                  >
                    Departments
                  </div>
                  {tierMore.map((tab) => (
                    <button
                      key={tab.kind}
                      role="menuitem"
                      onClick={() => handleKindChange(tab.kind)}
                      className={[
                        'w-full text-left px-3 py-2 text-sm transition-colors min-h-[32px]',
                        selectedKind === tab.kind
                          ? 'bg-base text-fg'
                          : 'text-fg-muted hover:bg-base hover:text-fg',
                      ].join(' ')}
                    >
                      {tab.label}
                    </button>
                  ))}
                </>
              )}

              {/* Unusable — one group label + one shared explanation, not per-row */}
              {unusableAgents.length > 0 && (
                <>
                  <div
                    className={[
                      'px-3 pb-1 text-xs font-semibold text-fg-subtle uppercase tracking-wide select-none',
                      tierMore.length > 0 ? 'pt-3 border-t border-border mt-1' : 'pt-1.5',
                    ].join(' ')}
                    aria-hidden="true"
                  >
                    Unavailable
                  </div>
                  <p className="px-3 pb-1.5 text-xs text-fg-subtle leading-relaxed">
                    No bound tables in the connected sources.
                  </p>
                  {unusableAgents.map((tab) => (
                    <div
                      key={tab.kind}
                      role="menuitem"
                      aria-disabled="true"
                      title="No bound tables in connected sources"
                      className="w-full px-3 py-2 text-sm text-fg-subtle opacity-50 cursor-not-allowed min-h-[32px]"
                    >
                      {tab.label}
                    </div>
                  ))}
                </>
              )}
            </div>
          )}
        </div>
      )}

      {/* Mode toggle — sibling to the tablist, pinned right, never scrolls away */}
      {modeToggle}
    </div>
  );

  // ── Active conversation title for the mobile header ──────────────────────────
  const activeTitle = activeConvId
    ? (conversations.find((c) => c.id === activeConvId)?.title ?? 'Agent Chat')
    : 'New Chat';

  // ── Scope panel content (plan 07 §4 — extracted to ScopePanel.tsx) ──────────
  const scopePanel = currentAgent ? (
    <ScopePanel agent={currentAgent} onPickExample={handlePickExample} />
  ) : null;

  // ── Render ───────────────────────────────────────────────────────────────────
  return (
    // No onClick here for moreOpen — dismissal is handled by the pointerdown
    // effect above which reads current state and cleans up properly.
    <div className="flex flex-col h-full">

      {/* Agent kind selector — horizontal-scroll strip (mobile) / pill tabs (md+) */}
      {kindSelector}

      {/* Ask coverage strip — visible when Ask is the selected agent.
          Shows the full roster of departments Ask can route to, collapsible.
          Default collapsed to avoid pushing the chat area down in sparse deployments. */}
      {selectedKind === ASK_KIND && departments.length > 0 && (
        <div className="flex-shrink-0 mb-2">
          <button
            onClick={() => setCoverageOpen((v) => !v)}
            className="flex items-center gap-1.5 w-full text-left text-xs text-fg-muted hover:text-fg transition-colors py-0.5"
            aria-expanded={coverageOpen}
          >
            <ChevronDown
              size={12}
              aria-hidden="true"
              className={['transition-transform', coverageOpen ? '' : '-rotate-90'].join(' ')}
            />
            Ask routes to{' '}
            <span className="font-medium text-fg">
              {departments.length}
            </span>{' '}
            {departments.length === 1 ? 'department' : 'departments'}
          </button>
          {coverageOpen && (
            <div className="flex flex-wrap gap-1 mt-1.5 pl-1">
              {departments.map((dept) =>
                dept.usable ? (
                  <button
                    key={dept.kind}
                    onClick={() => handleKindChange(dept.kind)}
                    className="px-2 py-0.5 rounded-full text-xs border border-border bg-elevated text-fg-muted hover:text-fg transition-colors"
                  >
                    {dept.label}
                  </button>
                ) : (
                  <span
                    key={dept.kind}
                    title="No bound tables in connected sources"
                    className="px-2 py-0.5 rounded-full text-xs border border-border bg-elevated text-fg-subtle opacity-50 cursor-default"
                  >
                    {dept.label}
                  </span>
                ),
              )}
            </div>
          )}
        </div>
      )}

      {/* "Only Ask is usable" notice — shown when the registry loaded successfully
          but no schema binding has been built for the connected sources yet.
          The binding is what maps source tables to service lines; without it the
          source_usable_lines map is empty and all department agents show usable:false.
          Not shown when the registry errored (that has its own EmptyState) or when
          legacy mode is active (legacy agents are always marked usable). */}
      {registryError === null && registry.loaded && usableAgents.length === 1 && (
        <div
          className="flex items-center gap-2 px-3 py-2 mb-2 rounded-md border border-border bg-elevated text-xs text-fg-muted flex-shrink-0"
          role="status"
        >
          <Info size={13} aria-hidden="true" />
          <span>
            Department agents are unavailable — no schema binding has been built for the
            connected source(s). Ask still works and routes across all departments. An admin
            can build the binding from{' '}
            <button
              onClick={() => navigate('/settings')}
              className="underline hover:text-fg transition-colors"
            >
              Settings
            </button>
            {' '}(Data Binding section).
          </span>
        </div>
      )}

      {/* Legacy notice — shown when the server has no agent registry endpoint.
          Non-blocking: the app is fully usable with the built-in default list. */}
      {registrySource === 'legacy' && (
        <div
          className="flex items-center gap-2 px-3 py-2 mb-2 rounded-md border border-border bg-elevated text-xs text-fg-muted flex-shrink-0"
          role="status"
        >
          <Info size={13} aria-hidden="true" />
          <span>Agent list is a built-in default — this server does not provide an agent registry.</span>
        </div>
      )}

      {/* Error state — registry fetch failed; feature is not usable until resolved */}
      {registryError !== null && registrySource === 'none' ? (
        <div className="flex-1 min-h-0 flex items-center justify-center">
          <EmptyState
            icon={<AlertTriangle size={32} />}
            title="Agent roster could not be loaded"
            description={registryError}
            action={
              <Button variant="secondary" onClick={() => void registry.load(true)}>
                Retry
              </Button>
            }
          />
        </div>
      ) : (
      <>

      {/* Mobile top row: drawer button + active title + scope panel toggle */}
      <div className="md:hidden flex items-center gap-2 pb-3 flex-shrink-0">
        <Button
          variant="secondary"
          size="sm"
          leftIcon={<PanelLeftOpen size={15} aria-hidden="true" />}
          onClick={() => setDrawerOpen(true)}
          className="min-h-[44px]"
        >
          Agents
        </Button>
        <h2 className="text-sm font-semibold text-fg flex-1 truncate text-center">
          {activeTitle}
        </h2>
        {currentAgent && (
          <button
            onClick={() => setPanelOpen(true)}
            className="flex items-center justify-center w-9 h-9 text-fg-muted hover:text-fg hover:bg-elevated rounded-md min-h-[44px]"
            aria-label="Agent scope"
          >
            <Bot size={16} aria-hidden="true" />
          </button>
        )}
      </div>

      {/* Main layout: single column (mobile) → two column (md+) → three column (xl+) */}
      <div className={`flex-1 min-h-0 flex flex-col md:grid md:grid-cols-[260px_minmax(0,1fr)] md:grid-rows-[minmax(0,1fr)] ${panelOpen ? 'xl:grid-cols-[260px_minmax(0,1fr)_220px]' : ''} rounded-lg border border-border overflow-hidden`}>

        {/* Left rail — desktop only; conversation list */}
        <aside className="hidden md:flex md:flex-col min-h-0 overflow-hidden border-r border-border bg-surface">
          <div className="flex items-center gap-2 px-3 py-2.5 border-b border-border text-xs font-semibold text-fg-muted uppercase tracking-wide flex-shrink-0">
            <Layers size={13} aria-hidden="true" />
            Conversations
          </div>
          <div className="flex-1 overflow-y-auto p-2">
            <ConversationList
              conversations={conversations}
              activeConvId={activeConvId}
              selectedKind={selectedKind}
              busyConversationIds={busyConversationIds}
              onSelect={handleSelectConv}
              onNewChat={handleNewChat}
              onActiveDeleted={handleActiveDeleted}
            />
          </div>
        </aside>

        {/* Right pane: message list + composer */}
        <div className="flex-1 min-h-0 overflow-hidden flex flex-col bg-base">
          <MessageList
            persisted={messages}
            runs={runs}
            queued={queued}
            activeConvId={activeConvId}
            onSendQueuedNow={(promptId) => {
              if (activeConvId) sendQueuedAgentNow(activeConvId, promptId);
            }}
            onRemoveQueued={(promptId) => {
              if (activeConvId) useAgents.getState().removeQueued(activeConvId, promptId);
            }}
            onRetry={retryAgentRun}
            onStop={stopAgentRun}
            onSuggestionSubmit={handleSuggestionSubmit}
            onSwitchAgent={handleSwitchAgent}
            exampleQuestions={currentAgent?.example_questions ?? []}
            onPickExample={handlePickExample}
          />
          <Composer
            text={draft}
            onTextChange={(text) => useAgents.getState().setDraft(draftKey, text)}
            onSend={handleSend}
            busy={busy}
          />
        </div>

        {/* Scope panel — xl+ third column */}
        {panelOpen && currentAgent && (
          <aside
            id="agent-scope-panel"
            className="hidden xl:flex xl:flex-col min-h-0 border-l border-border bg-surface overflow-y-auto p-3 gap-3"
          >
            {scopePanel}
          </aside>
        )}
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
          busyConversationIds={busyConversationIds}
          onSelect={handleSelectConv}
          onNewChat={handleNewChat}
          onActiveDeleted={handleActiveDeleted}
          onClose={() => setDrawerOpen(false)}
        />
      </Modal>

      {/* Mobile scope panel */}
      <Modal
        open={panelOpen}
        onClose={() => setPanelOpen(false)}
        title={currentAgent?.label ?? 'Agent'}
        size="sm"
      >
        <div className="flex flex-col gap-3 py-2">
          {scopePanel}
        </div>
      </Modal>

      </>
      )}
    </div>
  );
}
