import type { ButtonHTMLAttributes, ReactNode } from 'react';
import { cn } from './cn';

export interface ButtonProps extends ButtonHTMLAttributes<HTMLButtonElement> {
  variant?: 'primary' | 'secondary' | 'ghost' | 'danger';
  size?: 'sm' | 'md' | 'lg';
  loading?: boolean;
  full?: boolean;
  leftIcon?: ReactNode;
  rightIcon?: ReactNode;
}

const variantClasses: Record<NonNullable<ButtonProps['variant']>, string> = {
  primary:
    'bg-accent text-accent-fg hover:bg-accent-hover',
  secondary:
    'bg-elevated text-fg border border-border hover:border-border-strong hover:bg-elevated',
  ghost:
    'bg-transparent text-fg-muted hover:bg-elevated hover:text-fg',
  danger:
    'bg-danger-subtle text-danger border border-[rgba(239,68,68,0.3)] hover:bg-[rgba(239,68,68,0.2)]',
};

const sizeClasses: Record<NonNullable<ButtonProps['size']>, string> = {
  sm: 'px-3 py-1 text-xs rounded-sm',
  md: 'px-4 py-2 text-sm rounded-md',
  lg: 'px-6 py-3 text-base rounded-md',
};

function Spinner() {
  return (
    <svg
      className="animate-spin"
      width="14"
      height="14"
      viewBox="0 0 14 14"
      fill="none"
      aria-hidden="true"
    >
      <circle
        cx="7"
        cy="7"
        r="5"
        stroke="currentColor"
        strokeOpacity="0.25"
        strokeWidth="2"
      />
      <path
        d="M12 7a5 5 0 0 0-5-5"
        stroke="currentColor"
        strokeWidth="2"
        strokeLinecap="round"
      />
    </svg>
  );
}

export default function Button({
  variant = 'primary',
  size = 'md',
  loading = false,
  full = false,
  leftIcon,
  rightIcon,
  children,
  className,
  disabled,
  ...props
}: ButtonProps) {
  return (
    <button
      className={cn(
        'inline-flex items-center justify-center gap-2 font-medium whitespace-nowrap cursor-pointer',
        'border border-transparent',
        'transition-colors duration-150',
        'active:scale-[0.98]',
        'disabled:opacity-45 disabled:cursor-not-allowed',
        variantClasses[variant],
        sizeClasses[size],
        full && 'w-full',
        loading && 'pointer-events-none',
        className,
      )}
      disabled={disabled || loading}
      {...props}
    >
      {loading ? <Spinner /> : leftIcon}
      {children}
      {!loading && rightIcon}
    </button>
  );
}
