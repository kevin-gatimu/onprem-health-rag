import type { ReactNode } from 'react';
import { useEffect, useRef } from 'react';
import { createPortal } from 'react-dom';
import { X } from 'lucide-react';
import { cn } from './cn';

export interface ModalProps {
  open: boolean;
  onClose: () => void;
  title?: ReactNode;
  size?: 'sm' | 'md' | 'lg';
  children: ReactNode;
  footer?: ReactNode;
}

const sizeClasses: Record<NonNullable<ModalProps['size']>, string> = {
  sm: 'md:max-w-[360px]',
  md: 'md:max-w-[480px]',
  lg: 'md:max-w-[640px]',
};

/** Returns all focusable elements within a container. */
function getFocusable(el: HTMLElement): HTMLElement[] {
  return Array.from(
    el.querySelectorAll<HTMLElement>(
      'a[href], button:not([disabled]), input:not([disabled]), select:not([disabled]), textarea:not([disabled]), [tabindex]:not([tabindex="-1"])',
    ),
  );
}

export default function Modal({
  open,
  onClose,
  title,
  size = 'md',
  children,
  footer,
}: ModalProps) {
  const dialogRef = useRef<HTMLDivElement>(null);
  const previouslyFocused = useRef<Element | null>(null);

  /* ── Escape to close ── */
  useEffect(() => {
    if (!open) return;
    const handler = (e: KeyboardEvent) => {
      if (e.key === 'Escape') onClose();
    };
    window.addEventListener('keydown', handler);
    return () => window.removeEventListener('keydown', handler);
  }, [open, onClose]);

  /* ── Focus trap ── */
  useEffect(() => {
    if (!open) return;

    // Remember what had focus before we opened
    previouslyFocused.current = document.activeElement;

    // Focus first focusable element inside the dialog
    const frame = requestAnimationFrame(() => {
      if (!dialogRef.current) return;
      const focusable = getFocusable(dialogRef.current);
      if (focusable.length > 0) focusable[0].focus();
    });

    const trapTab = (e: KeyboardEvent) => {
      if (e.key !== 'Tab' || !dialogRef.current) return;
      const focusable = getFocusable(dialogRef.current);
      if (focusable.length === 0) { e.preventDefault(); return; }
      const first = focusable[0];
      const last = focusable[focusable.length - 1];
      if (e.shiftKey) {
        if (document.activeElement === first) { e.preventDefault(); last.focus(); }
      } else {
        if (document.activeElement === last) { e.preventDefault(); first.focus(); }
      }
    };
    window.addEventListener('keydown', trapTab);

    return () => {
      cancelAnimationFrame(frame);
      window.removeEventListener('keydown', trapTab);
      // Restore focus on close
      if (previouslyFocused.current instanceof HTMLElement) {
        previouslyFocused.current.focus();
      }
    };
  }, [open]);

  if (!open) return null;

  return createPortal(
    <div
      className={cn(
        'fixed inset-0 z-50 flex',
        /* Mobile: bottom-sheet alignment */
        'items-end justify-center',
        /* Desktop: centered */
        'md:items-center md:justify-center md:p-4',
        'bg-[var(--color-overlay)] backdrop-blur-[2px]',
        'animate-[fadeIn_150ms_ease_forwards]',
      )}
      onClick={(e) => { if (e.target === e.currentTarget) onClose(); }}
      aria-modal="true"
      role="dialog"
    >
      <div
        ref={dialogRef}
        className={cn(
          'relative flex flex-col bg-surface border border-border shadow-lg',
          'w-full max-h-[calc(100dvh-2rem)]',
          /* Mobile: bottom-sheet with rounded top corners, full width */
          'rounded-t-xl',
          /* Desktop: centered card with all corners rounded */
          'md:rounded-xl',
          sizeClasses[size],
          'animate-[slideUp_150ms_ease_forwards]',
        )}
      >
        {/* Header */}
        <div className="flex items-center justify-between px-6 py-4 border-b border-border flex-shrink-0">
          {title && (
            <h2 className="text-base font-semibold text-fg">{title}</h2>
          )}
          <button
            className="flex items-center justify-center w-7 h-7 rounded-sm text-fg-muted hover:bg-elevated hover:text-fg transition-colors duration-150 ml-auto"
            onClick={onClose}
            aria-label="Close"
          >
            <X size={16} />
          </button>
        </div>

        {/* Body */}
        <div className="p-6 overflow-y-auto flex-1">{children}</div>

        {/* Footer */}
        {footer && (
          <div className="flex items-center justify-end gap-3 px-6 py-4 border-t border-border flex-shrink-0">
            {footer}
          </div>
        )}
      </div>

      <style>{`
        @keyframes fadeIn { from { opacity: 0; } to { opacity: 1; } }
        @keyframes slideUp { from { opacity: 0; transform: translateY(12px); } to { opacity: 1; transform: translateY(0); } }
      `}</style>
    </div>,
    document.body,
  );
}
