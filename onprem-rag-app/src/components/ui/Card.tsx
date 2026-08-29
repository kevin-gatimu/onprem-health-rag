import type { ReactNode } from 'react';
import { cn } from './cn';

export interface CardProps {
  children: ReactNode;
  title?: ReactNode;
  actions?: ReactNode;
  /** Additional className forwarded to the outer container. */
  className?: string;
  /** Override inner padding (defaults to p-4). */
  padding?: string;
}

export default function Card({ children, title, actions, className, padding = 'p-4' }: CardProps) {
  const hasHeader = title !== undefined || actions !== undefined;

  return (
    <div
      className={cn(
        'bg-surface border border-border rounded-lg',
        className,
      )}
    >
      {hasHeader && (
        <div className="flex items-center justify-between px-4 py-3 border-b border-border">
          {title && <h3 className="text-sm font-semibold text-fg">{title}</h3>}
          {actions && <div className="flex items-center gap-2">{actions}</div>}
        </div>
      )}
      <div className={padding}>{children}</div>
    </div>
  );
}
