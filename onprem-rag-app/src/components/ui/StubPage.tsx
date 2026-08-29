import type { ReactNode } from 'react';

export interface StubPageProps {
  title: string;
  description?: string;
  icon?: ReactNode;
}

export function StubPage({ icon, title, description }: StubPageProps) {
  return (
    <div className="flex flex-col items-center justify-center h-full min-h-[400px] text-center gap-4 text-fg-muted">
      {icon && (
        <div className="w-[72px] h-[72px] rounded-xl bg-accent-subtle text-accent flex items-center justify-center">
          {icon}
        </div>
      )}
      <h1 className="text-xl font-semibold text-fg">{title}</h1>
      {description && (
        <p className="text-sm text-fg-muted max-w-[400px] leading-relaxed">{description}</p>
      )}
      <span className="px-3 py-1 bg-accent-subtle border border-accent/25 rounded-full text-xs font-medium text-accent">
        Coming in next phase
      </span>
    </div>
  );
}
