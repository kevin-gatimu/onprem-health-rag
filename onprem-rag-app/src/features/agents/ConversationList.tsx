// Conversation list for agent conversations — used in both the desktop rail and
// the mobile drawer. Mirrors chat/ConversationList.tsx exactly, with one addition:
// the `selectedKind` prop is needed to build the correct TanStack query key for
// cache invalidation after rename/delete.
import { useState } from 'react';
import { Plus, Pencil, Trash2, Check, X } from 'lucide-react';
import { useQueryClient } from '@tanstack/react-query';
import type { Conversation, AgentKind } from '../../lib/bridge';
import { deleteConversation, renameConversation } from '../../lib/bridge';
import { Button, cn } from '../../components/ui';
import { truncateTitle, fmtWhen } from '../chat/utils';

interface ConversationListProps {
  conversations: Conversation[];
  activeConvId: string | null;
  /** Used for cache invalidation — must match the query key in the parent shell. */
  selectedKind: AgentKind;
  busyConversationIds: Set<string>;
  onSelect: (id: string) => void;
  onNewChat: () => void;
  /** Called when the currently-active conversation is deleted. */
  onActiveDeleted: () => void;
  /** Optional: close the mobile drawer after an action. */
  onClose?: () => void;
}

export default function ConversationList({
  conversations,
  activeConvId,
  selectedKind,
  busyConversationIds,
  onSelect,
  onNewChat,
  onActiveDeleted,
  onClose,
}: ConversationListProps) {
  const queryClient = useQueryClient();
  const [renamingId, setRenamingId] = useState<string | null>(null);
  const [renameValue, setRenameValue] = useState('');

  async function handleDelete(conv: Conversation) {
    if (!window.confirm(`Delete "${conv.title}"?`)) return;
    await deleteConversation(conv.id);
    await queryClient.invalidateQueries({ queryKey: ['agent-conversations', selectedKind] });
    if (conv.id === activeConvId) {
      onActiveDeleted();
    }
  }

  function startRename(conv: Conversation) {
    setRenamingId(conv.id);
    setRenameValue(conv.title);
  }

  async function commitRename(id: string) {
    const trimmed = renameValue.trim();
    if (trimmed) {
      await renameConversation(id, trimmed);
      await queryClient.invalidateQueries({ queryKey: ['agent-conversations', selectedKind] });
    }
    setRenamingId(null);
  }

  function cancelRename() {
    setRenamingId(null);
  }

  return (
    <div className="flex flex-col gap-1">
      <Button
        variant="secondary"
        full
        leftIcon={<Plus size={14} aria-hidden="true" />}
        onClick={() => {
          onNewChat();
          onClose?.();
        }}
        className="min-h-[44px] mb-1"
      >
        New Chat
      </Button>

      {conversations.length === 0 && (
        <p className="text-xs text-fg-muted px-2 py-3 text-center">
          No conversations yet
        </p>
      )}

      <ul className="flex flex-col gap-0.5" role="listbox" aria-label="Conversations">
        {conversations.map((conv) => (
          <li key={conv.id} role="option" aria-selected={conv.id === activeConvId}>
            {renamingId === conv.id ? (
              // Inline rename row
              <div className="flex items-center gap-1 px-2 py-1.5">
                <input
                  autoFocus
                  value={renameValue}
                  onChange={(e) => setRenameValue(e.target.value)}
                  onKeyDown={(e) => {
                    if (e.key === 'Enter') void commitRename(conv.id);
                    if (e.key === 'Escape') cancelRename();
                  }}
                  className="flex-1 text-sm bg-elevated border border-accent/60 rounded px-2 py-1 text-fg focus:outline-none focus:ring-1 focus:ring-accent/40"
                  aria-label="Rename conversation"
                />
                <button
                  onClick={() => void commitRename(conv.id)}
                  className="p-1 text-success hover:text-success min-h-[32px] min-w-[32px] flex items-center justify-center"
                  aria-label="Confirm rename"
                >
                  <Check size={13} aria-hidden="true" />
                </button>
                <button
                  onClick={cancelRename}
                  className="p-1 text-fg-muted hover:text-fg min-h-[32px] min-w-[32px] flex items-center justify-center"
                  aria-label="Cancel rename"
                >
                  <X size={13} aria-hidden="true" />
                </button>
              </div>
            ) : (
              // Conversation row
              <button
                className={cn(
                  'group w-full text-left px-2 py-2 rounded-md text-sm transition-colors',
                  'flex flex-col gap-0.5 min-h-[44px]',
                  conv.id === activeConvId
                    ? 'bg-accent-subtle text-fg'
                    : 'text-fg hover:bg-elevated',
                )}
                onClick={() => {
                  onSelect(conv.id);
                  onClose?.();
                }}
              >
                <div className="flex items-center justify-between w-full gap-1">
                  <span className="font-medium truncate flex-1 min-w-0">
                    {truncateTitle(conv.title)}
                  </span>
                  {/* Action buttons — revealed on hover */}
                  <div
                    className="flex items-center gap-0.5 flex-shrink-0 opacity-0 group-hover:opacity-100 transition-opacity"
                    onClick={(e) => e.stopPropagation()}
                  >
                    <button
                      onClick={(e) => { e.stopPropagation(); startRename(conv); }}
                      disabled={busyConversationIds.has(conv.id)}
                      className="p-1 text-fg-muted hover:text-fg min-h-[32px] min-w-[32px] flex items-center justify-center disabled:opacity-40 disabled:cursor-not-allowed"
                      aria-label="Rename conversation"
                      title={busyConversationIds.has(conv.id) ? 'Wait for active responses before renaming' : undefined}
                      tabIndex={-1}
                    >
                      <Pencil size={12} aria-hidden="true" />
                    </button>
                    <button
                      onClick={(e) => { e.stopPropagation(); void handleDelete(conv); }}
                      disabled={busyConversationIds.has(conv.id)}
                      className="p-1 text-fg-muted hover:text-danger min-h-[32px] min-w-[32px] flex items-center justify-center disabled:opacity-40 disabled:cursor-not-allowed"
                      aria-label="Delete conversation"
                      title={busyConversationIds.has(conv.id) ? 'Wait for active responses before deleting' : undefined}
                      tabIndex={-1}
                    >
                      <Trash2 size={12} aria-hidden="true" />
                    </button>
                  </div>
                </div>
                <span className="text-xs text-fg-subtle">{fmtWhen(conv.updated_at)}</span>
              </button>
            )}
          </li>
        ))}
      </ul>
    </div>
  );
}
