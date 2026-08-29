import type { ReactNode } from 'react';
import { cn } from './cn';

export type BadgeVariant = 'success' | 'error' | 'warning' | 'info' | 'neutral';

export interface BadgeProps {
  variant?: BadgeVariant;
  dot?: boolean;
  children: ReactNode;
}

const variantClasses: Record<BadgeVariant, string> = {
  success: 'bg-success-subtle text-success',
  error:   'bg-danger-subtle text-danger',
  warning: 'bg-warning-subtle text-warning',
  info:    'bg-accent-subtle text-accent',
  neutral: 'bg-[rgba(100,116,139,0.12)] text-fg-muted',
};

export default function Badge({ variant = 'neutral', dot = false, children }: BadgeProps) {
  return (
    <span
      className={cn(
        'inline-flex items-center gap-1 px-2 py-[2px]',
        'rounded-full text-xs font-medium whitespace-nowrap',
        variantClasses[variant],
      )}
    >
      {dot && (
        <span
          className="w-[6px] h-[6px] rounded-full bg-current flex-shrink-0"
          aria-hidden="true"
        />
      )}
      {children}
    </span>
  );
}
