// Agent composer — stays interactive while responses stream. Enter queues a
// follow-up; Ctrl/Cmd+Enter bypasses the active conversation's queue.
import { useRef, useCallback } from 'react';
import { ListPlus, Send, Zap } from 'lucide-react';

interface ComposerProps {
  text: string;
  onTextChange: (text: string) => void;
  onSend: (text: string, sendImmediately: boolean) => void;
  busy: boolean;
}

export default function Composer({ text, onTextChange, onSend, busy }: ComposerProps) {
  const textareaRef = useRef<HTMLTextAreaElement>(null);

  const send = useCallback((sendImmediately = false) => {
    const trimmed = text.trim();
    if (!trimmed) return;
    onSend(trimmed, sendImmediately);
    onTextChange('');
    if (textareaRef.current) {
      textareaRef.current.style.height = 'auto';
    }
  }, [text, onSend, onTextChange]);

  function handleKeyDown(e: React.KeyboardEvent<HTMLTextAreaElement>) {
    if (e.key === 'Enter' && !e.shiftKey) {
      e.preventDefault();
      send(e.ctrlKey || e.metaKey);
    }
  }

  function handleInput() {
    const el = textareaRef.current;
    if (!el) return;
    el.style.height = 'auto';
    // Max ~5 rows ≈ 120px; beyond that the textarea scrolls internally.
    el.style.height = `${Math.min(el.scrollHeight, 120)}px`;
  }

  const canSend = text.trim().length > 0;

  return (
    <div
      className="flex-shrink-0 border-t border-border bg-surface px-3 py-2"
      style={{ paddingBottom: 'calc(0.5rem + env(safe-area-inset-bottom))' }}
    >
      {busy && (
        <p className="max-w-3xl mx-auto mb-1 text-xs text-fg-muted">
          Enter queues this question. Ctrl/Cmd+Enter sends it immediately.
        </p>
      )}
      <div className="flex items-end gap-2 max-w-3xl mx-auto">
        <textarea
          ref={textareaRef}
          value={text}
          onChange={(e) => onTextChange(e.target.value)}
          onKeyDown={handleKeyDown}
          onInput={handleInput}
          placeholder="Ask your AI agent a question…"
          rows={1}
          className="flex-1 resize-none bg-elevated border border-border rounded-lg px-3 py-2.5 text-sm text-fg placeholder:text-fg-subtle focus:outline-none focus:ring-1 focus:ring-accent/40 focus:border-accent/60 overflow-y-auto min-h-[44px]"
          aria-label="Ask a question"
          aria-multiline="true"
        />

        {busy && (
          <button
            onClick={() => send(true)}
            disabled={!canSend}
            className="flex-shrink-0 flex items-center justify-center w-10 h-10 rounded-lg border border-accent text-accent hover:bg-accent-subtle transition-colors disabled:opacity-40 disabled:cursor-not-allowed min-h-[44px] min-w-[44px]"
            aria-label="Send immediately without waiting"
            title="Send immediately without waiting"
          >
            <Zap size={16} aria-hidden="true" />
          </button>
        )}
        <button
          onClick={() => send(false)}
          disabled={!canSend}
          className="flex-shrink-0 flex items-center justify-center w-10 h-10 rounded-lg bg-accent text-accent-fg hover:bg-accent-hover transition-colors disabled:opacity-40 disabled:cursor-not-allowed min-h-[44px] min-w-[44px]"
          aria-label={busy ? 'Queue message' : 'Send message'}
          title={busy ? 'Queue message' : 'Send message'}
        >
          {busy ? <ListPlus size={16} aria-hidden="true" /> : <Send size={16} aria-hidden="true" />}
        </button>
      </div>
    </div>
  );
}
