import type { ReactNode, SelectHTMLAttributes } from 'react';
import { ChevronDown } from 'lucide-react';
import { cn } from './cn';

export interface SelectProps extends SelectHTMLAttributes<HTMLSelectElement> {
  label?: string;
  hint?: string;
  error?: string;
  options?: { value: string; label: string; disabled?: boolean }[];
  children?: ReactNode;
}

export default function Select({
  label,
  hint,
  error,
  options,
  children,
  className,
  id,
  ...props
}: SelectProps) {
  const selectId = id ?? label?.toLowerCase().replace(/\s+/g, '-');

  return (
    <div className="flex flex-col gap-1">
      {label && (
        <label className="text-sm font-medium text-fg-muted" htmlFor={selectId}>
          {label}
        </label>
      )}
      <div className="relative flex items-center">
        <select
          id={selectId}
          className={cn(
            'w-full py-2 pl-3 pr-8 bg-surface border border-border rounded-md',
            'text-sm text-fg font-[inherit]',
            'appearance-none cursor-pointer',
            'transition-colors duration-150',
            'focus:border-accent focus:ring-2 focus:ring-accent/20 focus:outline-none',
            'disabled:opacity-50 disabled:cursor-not-allowed',
            error && 'border-danger focus:ring-danger/20',
            className,
          )}
          {...props}
        >
          {options
            ? options.map((o) => (
                <option
                  key={o.value}
                  value={o.value}
                  disabled={o.disabled}
                  className="bg-surface text-fg disabled:text-fg-subtle"
                >
                  {o.label}
                </option>
              ))
            : children}
        </select>
        <span className="absolute right-3 pointer-events-none text-fg-subtle flex">
          <ChevronDown size={14} />
        </span>
      </div>
      {error && <span className="text-xs text-danger">{error}</span>}
      {!error && hint && <span className="text-xs text-fg-subtle">{hint}</span>}
    </div>
  );
}
