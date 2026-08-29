// Chat composer — auto-grow textarea (max ~5 rows), Send button, retrieval
// settings toggle. Enter sends; Shift+Enter inserts a newline.
// Composer and Send are disabled while a run is streaming.
import { useRef, useState, useCallback } from 'react';
import { Send, SlidersHorizontal, Loader2 } from 'lucide-react';

interface ComposerProps {
  onSend: (text: string) => void;
  streaming: boolean;
  onOpenSettings: () => void;
}

export default function Composer({ onSend, streaming, onOpenSettings }: ComposerProps) {
  const [text, setText] = useState('');
  const textareaRef = useRef<HTMLTextAreaElement>(null);

  const send = useCallback(() => {
    const trimmed = text.trim();
    if (!trimmed || streaming) return;
    onSend(trimmed);
    setText('');
    // Reset height to a single row after send
    if (textareaRef.current) {
      textareaRef.current.style.height = 'auto';
    }
  }, [text, streaming, onSend]);

  function handleKeyDown(e: React.KeyboardEvent<HTMLTextAreaElement>) {
    if (e.key === 'Enter' && !e.shiftKey) {
      e.preventDefault();
      send();
    }
  }

  function handleInput() {
    const el = textareaRef.current;
    if (!el) return;
    el.style.height = 'auto';
    // Max ~5 rows ≈ 120px; beyond that the textarea scrolls internally
    el.style.height = `${Math.min(el.scrollHeight, 120)}px`;
  }

  const canSend = text.trim().length > 0 && !streaming;

  return (
    <div
      className="flex-shrink-0 border-t border-border bg-surface px-3 py-2"
      style={{ paddingBottom: 'calc(0.5rem + env(safe-area-inset-bottom))' }}
    >
      <div className="flex items-end gap-2 max-w-3xl mx-auto">
        {/* Retrieval settings toggle */}
        <button
          onClick={onOpenSettings}
          disabled={streaming}
          className="flex-shrink-0 flex items-center justify-center w-9 h-9 rounded-md text-fg-muted hover:bg-elevated hover:text-fg transition-colors disabled:opacity-40 min-h-[44px]"
          aria-label="Retrieval settings"
        >
          <SlidersHorizontal size={16} aria-hidden="true" />
        </button>

        {/* Auto-grow textarea */}
        <textarea
          ref={textareaRef}
          value={text}
          onChange={(e) => setText(e.target.value)}
          onKeyDown={handleKeyDown}
          onInput={handleInput}
          disabled={streaming}
          placeholder="Ask a question about your health records…"
          rows={1}
          className="flex-1 resize-none bg-elevated border border-border rounded-lg px-3 py-2.5 text-sm text-fg placeholder:text-fg-subtle focus:outline-none focus:ring-1 focus:ring-accent/40 focus:border-accent/60 disabled:opacity-40 overflow-y-auto min-h-[44px]"
          aria-label="Ask a question"
          aria-multiline="true"
        />

        {/* Send button */}
        <button
          onClick={send}
          disabled={!canSend}
          className="flex-shrink-0 flex items-center justify-center w-10 h-10 rounded-lg bg-accent text-accent-fg hover:bg-accent-hover transition-colors disabled:opacity-40 disabled:cursor-not-allowed min-h-[44px] min-w-[44px]"
          aria-label="Send message"
        >
          {streaming
            ? <Loader2 size={16} className="animate-spin" aria-hidden="true" />
            : <Send size={16} aria-hidden="true" />}
        </button>
      </div>
    </div>
  );
}
