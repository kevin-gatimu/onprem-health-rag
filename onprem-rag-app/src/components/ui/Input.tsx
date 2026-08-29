import { useState } from 'react';
import type { InputHTMLAttributes, ReactNode } from 'react';
import { Eye, EyeOff } from 'lucide-react';
import { cn } from './cn';

export interface InputProps extends InputHTMLAttributes<HTMLInputElement> {
  label?: string;
  hint?: string;
  error?: string;
  icon?: ReactNode;
  showToggle?: boolean;
}

export default function Input({
  label,
  hint,
  error,
  icon,
  showToggle,
  className,
  id,
  type,
  ...props
}: InputProps) {
  const [visible, setVisible] = useState(false);
  const inputId = id ?? label?.toLowerCase().replace(/\s+/g, '-');
  const inputType =
    showToggle && type === 'password' ? (visible ? 'text' : 'password') : type;

  return (
    <div className="flex flex-col gap-1">
      {label && (
        <label
          className="text-sm font-medium text-fg-muted"
          htmlFor={inputId}
        >
          {label}
        </label>
      )}
      <div className="relative flex items-center">
        {icon && (
          <span className="absolute left-3 text-fg-subtle pointer-events-none flex">
            {icon}
          </span>
        )}
        <input
          id={inputId}
          type={inputType}
          className={cn(
            'w-full py-2 px-3 bg-surface border border-border rounded-md',
            'text-sm text-fg font-[inherit]',
            'transition-colors duration-150',
            'placeholder:text-fg-subtle',
            'focus:border-accent focus:ring-2 focus:ring-accent/20 focus:outline-none',
            'disabled:opacity-50 disabled:cursor-not-allowed',
            !!icon && 'pl-[calc(0.75rem+18px+0.5rem)]',
            !!showToggle && 'pr-[calc(0.75rem+20px+0.5rem)]',
            error && 'border-danger focus:border-danger focus:ring-danger/20',
            className,
          )}
          {...props}
        />
        {showToggle && (
          <button
            type="button"
            className="absolute right-3 flex items-center justify-center bg-none border-none p-0 cursor-pointer text-fg-subtle hover:text-fg transition-colors duration-150"
            onClick={() => setVisible((v) => !v)}
            aria-label={visible ? 'Hide password' : 'Show password'}
            tabIndex={-1}
          >
            {visible ? <EyeOff size={14} /> : <Eye size={14} />}
          </button>
        )}
      </div>
      {error && (
        <span className="text-xs text-danger">{error}</span>
      )}
      {!error && hint && (
        <span className="text-xs text-fg-subtle">{hint}</span>
      )}
    </div>
  );
}
