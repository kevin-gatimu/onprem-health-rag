// SuggestionChips — up to 4 follow-up chips shown under the last assistant bubble.
// Shared between the agents and chat features.
//
// Clicking a chip is a normal turn: the text is submitted as the next user message
// and the optional `suggestionSpec` is forwarded to the backend so it can skip
// re-planning. "switch" suggestions carry an arrow icon and change the active agent.
import { ArrowRight, Search, ZoomIn, ZoomOut, BarChart2, HelpCircle } from 'lucide-react';
import type { Suggestion } from '../../lib/bridge';

interface Props {
  suggestions: Suggestion[];
  onSubmit: (text: string, opts?: { suggestionSpec?: unknown; switchKind?: string }) => void;
}

const KIND_ICON: Record<string, React.ReactNode> = {
  drill:   <ZoomIn size={12} aria-hidden="true" />,
  widen:   <ZoomOut size={12} aria-hidden="true" />,
  compare: <BarChart2 size={12} aria-hidden="true" />,
  switch:  <ArrowRight size={12} aria-hidden="true" />,
  explain: <HelpCircle size={12} aria-hidden="true" />,
};

export default function SuggestionChips({ suggestions, onSubmit }: Props) {
  if (!suggestions.length) return null;
  // Only show the first 4 chips.
  const chips = suggestions.slice(0, 4);

  return (
    <div className="flex flex-wrap gap-1.5 mt-2">
      {chips.map((s, i) => (
        <button
          key={i}
          onClick={() =>
            onSubmit(s.text, {
              suggestionSpec: s.spec,
              switchKind: s.kind === 'switch' ? s.agent : undefined,
            })
          }
          className="flex items-center gap-1.5 px-2.5 py-1.5 rounded-full border border-border text-xs text-fg-muted hover:border-accent hover:text-fg bg-surface hover:bg-elevated transition-colors min-h-[36px]"
          title={s.kind === 'switch' && s.agent ? `Open in ${s.agent}` : s.kind}
        >
          {KIND_ICON[s.kind] ?? <Search size={12} aria-hidden="true" />}
          <span>{s.text}</span>
        </button>
      ))}
    </div>
  );
}
