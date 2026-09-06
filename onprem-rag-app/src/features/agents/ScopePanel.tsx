// ScopePanel — plan 07 §4. The right-hand context column (xl+) and the mobile
// "Agent scope" modal body.
//
// Everything here is registry data (`AgentInfo` from `GET /agents`, projected by
// the bridge's `list_agents`). No service-line slug, label or blurb is written in
// this file — see plan 07 §8's acceptance rule.
//
// `sources[].tables` is the real per-source table scope the bridge reads from
// `GET /sources/<id>/binding`, so "Data this agent can see" names the tables the
// server would actually query — not a guess.
import { Bot, Database, Sparkles } from 'lucide-react';
import type { AgentInfo } from '../../lib/bridge';

interface ScopePanelProps {
  agent: AgentInfo;
  /** Called when an example question chip is clicked (fills the composer). */
  onPickExample: (question: string) => void;
}

export default function ScopePanel({ agent, onPickExample }: ScopePanelProps) {
  const hasScope = agent.sources.some((source) => source.tables.length > 0);

  return (
    <>
      <h3 className="text-xs font-semibold text-fg-muted uppercase tracking-wide flex items-center gap-1.5 flex-shrink-0">
        <Bot size={12} aria-hidden="true" />
        {agent.label}
      </h3>

      <p className="text-xs text-fg-muted leading-relaxed">{agent.blurb}</p>

      {/* Data scope — table chips grouped by source. */}
      <div>
        <p className="text-xs font-medium text-fg mb-1 flex items-center gap-1.5">
          <Database size={11} aria-hidden="true" />
          Data this agent can see
        </p>
        {hasScope ? (
          agent.sources.map((source) => (
            <div key={source.source_id} className="mb-1.5">
              <p className="text-xs text-fg-subtle font-mono truncate" title={source.source_id}>
                {source.source_id}
              </p>
              <div className="flex flex-wrap gap-1 mt-0.5">
                {source.tables.map((table) => (
                  <span
                    key={table}
                    className="px-1.5 py-0.5 rounded bg-elevated text-xs text-fg-muted font-mono"
                  >
                    {table}
                  </span>
                ))}
              </div>
            </div>
          ))
        ) : (
          <p className="text-xs text-fg-subtle">
            No bound tables in connected sources.
          </p>
        )}
      </div>

      {/* Concepts this line owns — verified registry field, useful when the scope
          is empty because it explains WHAT the binder was looking for. */}
      {agent.concepts.length > 0 && (
        <div>
          <p className="text-xs font-medium text-fg mb-1">Concepts</p>
          <div className="flex flex-wrap gap-1">
            {agent.concepts.map((concept) => (
              <span
                key={concept}
                className="px-1.5 py-0.5 rounded bg-elevated text-xs text-fg-subtle font-mono"
              >
                {concept}
              </span>
            ))}
          </div>
        </div>
      )}

      {agent.example_questions.length > 0 && (
        <div>
          <p className="text-xs font-medium text-fg mb-1 flex items-center gap-1.5">
            <Sparkles size={11} aria-hidden="true" />
            Example questions
          </p>
          <div className="flex flex-col gap-1">
            {agent.example_questions.map((question) => (
              <button
                key={question}
                onClick={() => onPickExample(question)}
                className="text-left text-xs text-fg-muted hover:text-fg px-2 py-1.5 rounded hover:bg-elevated transition-colors min-h-[36px]"
              >
                {question}
              </button>
            ))}
          </div>
        </div>
      )}
    </>
  );
}
