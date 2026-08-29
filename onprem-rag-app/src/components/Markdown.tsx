// Thin react-markdown + remark-gfm wrapper with Tailwind v4 token styling.
// Chart code fences (```chart and ```chart-json) are intercepted and rendered
// via AgentChart instead of the default <pre>/<code> elements.
// External links open in a new tab (rel="noopener noreferrer").
import { isValidElement } from 'react';
import type { ReactNode, ReactElement } from 'react';
import ReactMarkdown from 'react-markdown';
import remarkGfm from 'remark-gfm';
import { cn } from './ui';
import AgentChart from './chart/AgentChart';

// Both ```chart (legacy line grammar) and ```chart-json (new typed JSON
// contract) route to AgentChart. It parses either and renders NOTHING when the
// block has no plottable data, so a "no records" answer never grows an empty
// bar graph.
const CHART_LANGS = new Set(['language-chart', 'language-chart-json']);

/**
 * Recursively extract the raw text from a React node tree.
 * Used to pull the fence body out of the component children before handing it
 * to parseChart (which expects a plain string, not a ReactNode).
 */
function extractText(children: ReactNode): string {
  if (children === null || children === undefined) return '';
  if (typeof children === 'string') return children;
  if (typeof children === 'number' || typeof children === 'boolean') return String(children);
  if (Array.isArray(children)) {
    return (children as ReactNode[]).map(extractText).join('');
  }
  if (isValidElement(children)) {
    return extractText(
      (children as ReactElement<{ children?: ReactNode }>).props.children,
    );
  }
  return '';
}

interface MarkdownProps {
  content: string;
}

export default function Markdown({ content }: MarkdownProps) {
  return (
    <ReactMarkdown
      remarkPlugins={[remarkGfm]}
      components={{
        p: ({ children }) => (
          <p className="mb-3 last:mb-0 leading-relaxed">{children}</p>
        ),
        ul: ({ children }) => (
          <ul className="mb-3 list-disc pl-5 space-y-1">{children}</ul>
        ),
        ol: ({ children }) => (
          <ol className="mb-3 list-decimal pl-5 space-y-1">{children}</ol>
        ),
        li: ({ children }) => (
          <li className="leading-relaxed">{children}</li>
        ),
        h1: ({ children }) => (
          <h1 className="text-lg font-bold mt-4 mb-2">{children}</h1>
        ),
        h2: ({ children }) => (
          <h2 className="text-base font-semibold mt-3 mb-2">{children}</h2>
        ),
        h3: ({ children }) => (
          <h3 className="text-sm font-semibold mt-3 mb-1">{children}</h3>
        ),
        code: ({ children, className }) => {
          // Intercept chart code fences — render as AgentChart instead of <code>.
          if (className && CHART_LANGS.has(className)) {
            return <AgentChart data={extractText(children as ReactNode).trim()} />;
          }
          const isBlock = !!className;
          if (isBlock) {
            return (
              <code className={cn('text-xs font-mono', className)}>
                {children}
              </code>
            );
          }
          return (
            <code className="text-xs font-mono bg-elevated px-1 py-0.5 rounded-sm">
              {children}
            </code>
          );
        },
        pre: ({ children }) => {
          // Peek at the child <code> element's className. react-markdown wraps
          // fenced code as <pre><code className="language-*">…</code></pre>.
          if (isValidElement<{ className?: string; children?: ReactNode }>(children)) {
            const { className, children: codeChildren } = children.props;
            if (className && CHART_LANGS.has(className)) {
              return <AgentChart data={extractText(codeChildren).trim()} />;
            }
          }
          return (
            <pre className="mb-3 overflow-x-auto rounded-md bg-elevated p-3 text-xs font-mono">
              {children}
            </pre>
          );
        },
        table: ({ children }) => (
          <div className="mb-3 overflow-x-auto">
            <table className="w-full text-xs border-collapse">{children}</table>
          </div>
        ),
        th: ({ children }) => (
          <th className="border border-border px-2 py-1 font-semibold text-left bg-elevated">
            {children}
          </th>
        ),
        td: ({ children }) => (
          <td className="border border-border px-2 py-1">{children}</td>
        ),
        a: ({ href, children }) => (
          <a
            href={href}
            target="_blank"
            rel="noopener noreferrer"
            className="text-accent underline underline-offset-2 hover:opacity-80"
          >
            {children}
          </a>
        ),
        strong: ({ children }) => (
          <strong className="font-semibold">{children}</strong>
        ),
        blockquote: ({ children }) => (
          <blockquote className="border-l-2 border-border pl-3 text-fg-muted italic mb-3">
            {children}
          </blockquote>
        ),
      }}
    >
      {content}
    </ReactMarkdown>
  );
}
