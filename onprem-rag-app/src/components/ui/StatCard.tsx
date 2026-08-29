import type { ReactNode } from 'react';
import { cn } from './cn';

export interface StatCardProps {
  label: string;
  value: ReactNode;
  icon?: ReactNode;
  /** Trend/delta line rendered below the value — e.g. "+12% vs last week". */
  delta?: ReactNode;
  /** Optional accent color token class for the value, e.g. "text-success". */
  accentColor?: string;
  className?: string;
}

export default function StatCard({ label, value, icon, delta, accentColor, className }: StatCardProps) {
  return (
    <div className={cn('bg-surface border border-border rounded-lg p-4 flex flex-col gap-2', className)}>
      <div className="flex items-start justify-between gap-2">
        <span className="text-sm text-fg-muted">{label}</span>
        {icon && (
          <span className="text-fg-subtle flex-shrink-0">{icon}</span>
        )}
      </div>
      <div className={cn('text-2xl font-semibold text-fg leading-none', accentColor)}>
        {value}
      </div>
      {delta && (
        <div className="text-xs text-fg-muted">{delta}</div>
      )}
    </div>
  );
}
