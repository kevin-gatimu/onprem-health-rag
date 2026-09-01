import { agent, cancelRun, chat, createConversation } from './bridge';
import type { AgentKind, RetrievalOpts } from './bridge';
import { queryClient } from './queryClient';
import { useAgents, type QueuedAgentPrompt } from '../stores/agents';
import { useChat, type QueuedChatPrompt } from '../stores/chat';

const activeChatRuns = new Map<string, Set<string>>();
const activeAgentRuns = new Map<string, Set<string>>();
let creatingChat: Promise<string> | null = null;
const creatingAgents = new Map<AgentKind, Promise<string>>();
let runtimeEpoch = 0;

function addActive(registry: Map<string, Set<string>>, conversationId: string, runId: string) {
  const ids = registry.get(conversationId) ?? new Set<string>();
  ids.add(runId);
  registry.set(conversationId, ids);
}

function removeActive(registry: Map<string, Set<string>>, conversationId: string, runId: string) {
  const ids = registry.get(conversationId);
  if (!ids) return;
  ids.delete(runId);
  if (ids.size === 0) registry.delete(conversationId);
}

async function ensureChatConversation(conversationId: string | null): Promise<string> {
  if (conversationId) return conversationId;
  const current = useChat.getState().activeConversationId;
  if (current) return current;
  if (!creatingChat) {
    const epoch = runtimeEpoch;
    const creating = createConversation()
      .then((conversation) => {
        if (epoch === runtimeEpoch) {
          useChat.getState().setActiveConversation(conversation.id);
          void queryClient.invalidateQueries({ queryKey: ['conversations'] });
        }
        return conversation.id;
      })
      .finally(() => {
        if (creatingChat === creating) creatingChat = null;
      });
    creatingChat = creating;
  }
  return creatingChat;
}

async function ensureAgentConversation(
  conversationId: string | null,
  kind: AgentKind,
): Promise<string> {
  if (conversationId) return conversationId;
  const state = useAgents.getState();
  if (state.selectedKind === kind && state.activeConversationId) return state.activeConversationId;
  const existing = creatingAgents.get(kind);
  if (existing) return existing;

  const epoch = runtimeEpoch;
  const creating = createConversation(undefined, kind)
    .then((conversation) => {
      if (epoch === runtimeEpoch) {
        const latest = useAgents.getState();
        if (latest.selectedKind === kind && latest.activeConversationId === null) {
          latest.setActiveConversation(conversation.id);
        }
        void queryClient.invalidateQueries({ queryKey: ['agent-conversations', kind] });
      }
      return conversation.id;
    })
    .finally(() => {
      if (creatingAgents.get(kind) === creating) creatingAgents.delete(kind);
    });
  creatingAgents.set(kind, creating);
  return creating;
}

function chatIsBusy(conversationId: string): boolean {
  return (activeChatRuns.get(conversationId)?.size ?? 0) > 0;
}

function agentIsBusy(conversationId: string): boolean {
  return (activeAgentRuns.get(conversationId)?.size ?? 0) > 0;
}

async function launchChat(prompt: QueuedChatPrompt): Promise<void> {
  const runId = crypto.randomUUID();
  addActive(activeChatRuns, prompt.conversationId, runId);
  useChat.getState().startRun(runId, prompt.conversationId, prompt.user, prompt.opts);

  let succeeded = false;
  try {
    await chat(prompt.user, [], prompt.opts, prompt.conversationId, runId);
    succeeded = true;
    await queryClient.invalidateQueries({ queryKey: ['messages', prompt.conversationId] });
    void queryClient.invalidateQueries({ queryKey: ['conversations'] });
  } catch (error) {
    if (useChat.getState().runs[runId]?.phase !== 'stopped') {
      useChat.getState().setError(runId, String(error));
    }
  } finally {
    removeActive(activeChatRuns, prompt.conversationId, runId);
    if (succeeded) useChat.getState().removeRun(runId);
    startNextChat(prompt.conversationId);
  }
}

function startNextChat(conversationId: string) {
  if (chatIsBusy(conversationId)) return;
  const next = useChat.getState().queues[conversationId]?.[0];
  if (!next) return;
  useChat.getState().removeQueued(conversationId, next.id);
  void launchChat(next);
}

export async function submitChatPrompt(
  conversationId: string | null,
  user: string,
  opts: RetrievalOpts,
  sendImmediately: boolean,
): Promise<string> {
  const epoch = runtimeEpoch;
  const resolvedId = await ensureChatConversation(conversationId);
  if (epoch !== runtimeEpoch) return resolvedId;
  const prompt: QueuedChatPrompt = {
    id: crypto.randomUUID(),
    conversationId: resolvedId,
    user,
    opts: { ...opts },
    queuedAt: Date.now(),
  };

  if (chatIsBusy(resolvedId) && !sendImmediately) {
    useChat.getState().enqueue(prompt);
  } else {
    void launchChat(prompt);
  }
  return resolvedId;
}

export function sendQueuedChatNow(conversationId: string, promptId: string) {
  const prompt = useChat.getState().queues[conversationId]?.find((item) => item.id === promptId);
  if (!prompt) return;
  useChat.getState().removeQueued(conversationId, promptId);
  void launchChat(prompt);
}

export function stopChatRun(runId: string) {
  const run = useChat.getState().runs[runId];
  if (!run || run.phase === 'done' || run.phase === 'stopped') return;
  useChat.getState().markStopped(runId);
  void cancelRun(runId);
}

export function retryChatRun(runId: string) {
  const run = useChat.getState().runs[runId];
  if (!run || (!run.error && run.phase !== 'stopped')) return;
  useChat.getState().removeRun(runId);
  const prompt: QueuedChatPrompt = {
    id: crypto.randomUUID(),
    conversationId: run.conversationId,
    user: run.user,
    opts: run.opts,
    queuedAt: Date.now(),
  };
  if (chatIsBusy(run.conversationId)) {
    useChat.getState().enqueue(prompt);
  } else {
    void launchChat(prompt);
  }
}

async function launchAgent(prompt: QueuedAgentPrompt): Promise<void> {
  const runId = crypto.randomUUID();
  addActive(activeAgentRuns, prompt.conversationId, runId);
  useAgents.getState().startRun(runId, prompt.conversationId, prompt.selectedKind, prompt.user);

  let succeeded = false;
  try {
    await agent(prompt.selectedKind, prompt.user, prompt.conversationId, runId);
    succeeded = true;
    await queryClient.invalidateQueries({ queryKey: ['messages', prompt.conversationId] });
    void queryClient.invalidateQueries({
      queryKey: ['agent-conversations', prompt.selectedKind],
    });
  } catch (error) {
    if (useAgents.getState().runs[runId]?.phase !== 'stopped') {
      useAgents.getState().setError(runId, String(error));
    }
  } finally {
    removeActive(activeAgentRuns, prompt.conversationId, runId);
    if (succeeded) useAgents.getState().removeRun(runId);
    startNextAgent(prompt.conversationId);
  }
}

function startNextAgent(conversationId: string) {
  if (agentIsBusy(conversationId)) return;
  const next = useAgents.getState().queues[conversationId]?.[0];
  if (!next) return;
  useAgents.getState().removeQueued(conversationId, next.id);
  void launchAgent(next);
}

export async function submitAgentPrompt(
  conversationId: string | null,
  selectedKind: AgentKind,
  user: string,
  sendImmediately: boolean,
): Promise<string> {
  const epoch = runtimeEpoch;
  const resolvedId = await ensureAgentConversation(conversationId, selectedKind);
  if (epoch !== runtimeEpoch) return resolvedId;
  const prompt: QueuedAgentPrompt = {
    id: crypto.randomUUID(),
    conversationId: resolvedId,
    selectedKind,
    user,
    queuedAt: Date.now(),
  };

  if (agentIsBusy(resolvedId) && !sendImmediately) {
    useAgents.getState().enqueue(prompt);
  } else {
    void launchAgent(prompt);
  }
  return resolvedId;
}

export function sendQueuedAgentNow(conversationId: string, promptId: string) {
  const prompt = useAgents.getState().queues[conversationId]?.find((item) => item.id === promptId);
  if (!prompt) return;
  useAgents.getState().removeQueued(conversationId, promptId);
  void launchAgent(prompt);
}

export function stopAgentRun(runId: string) {
  const run = useAgents.getState().runs[runId];
  if (!run || run.phase === 'done' || run.phase === 'stopped') return;
  useAgents.getState().markStopped(runId);
  void cancelRun(runId);
}

export function retryAgentRun(runId: string) {
  const run = useAgents.getState().runs[runId];
  if (!run || (!run.error && run.phase !== 'stopped')) return;
  useAgents.getState().removeRun(runId);
  const prompt: QueuedAgentPrompt = {
    id: crypto.randomUUID(),
    conversationId: run.conversationId,
    selectedKind: run.selectedKind,
    user: run.user,
    queuedAt: Date.now(),
  };
  if (agentIsBusy(run.conversationId)) {
    useAgents.getState().enqueue(prompt);
  } else {
    void launchAgent(prompt);
  }
}

export function resetConversationRuntime() {
  runtimeEpoch += 1;
  activeChatRuns.clear();
  activeAgentRuns.clear();
  creatingChat = null;
  creatingAgents.clear();
  useChat.getState().reset();
  useAgents.getState().reset();
}
