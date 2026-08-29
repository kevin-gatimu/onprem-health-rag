import type { ReactNode } from 'react';
import { cn } from './cn';

export interface EmptyStateProps {
  /** Icon displayed above the title. */
  icon?: ReactNode;
  title: string;
  description?: string;
  /** Optional action element (typically a Button). */
  action?: ReactNode;
  className?: string;
}

export default function EmptyState({ icon, title, description, action, className }: EmptyStateProps) {
  return (
    <div
      className={cn(
        'flex flex-col items-center justify-center text-center gap-3 py-12 px-4',
        className,
      )}
    >
      {icon && (
        <span className="text-fg-subtle mb-1">{icon}</span>
      )}
      <h3 className="text-base font-semibold text-fg">{title}</h3>
      {description && (
        <p className="text-sm text-fg-muted max-w-sm leading-relaxed">{description}</p>
      )}
      {action && <div className="mt-2">{action}</div>}
    </div>
  );
}
