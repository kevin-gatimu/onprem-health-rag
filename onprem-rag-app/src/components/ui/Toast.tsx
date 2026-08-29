import { CheckCircle, XCircle, AlertTriangle, Info, X } from 'lucide-react';
import { cn } from './cn';

export type ToastType = 'success' | 'error' | 'warning' | 'info';

export interface Toast {
  id: string;
  type: ToastType;
  message: string;
  duration?: number;
}

export interface ToastItemProps {
  toast: Toast;
  onDismiss: (id: string) => void;
}

export interface ToastContainerProps {
  toasts: Toast[];
  onDismiss: (id: string) => void;
}

const ICONS: Record<ToastType, React.ReactElement> = {
  success: <CheckCircle size={16} />,
  error:   <XCircle size={16} />,
  warning: <AlertTriangle size={16} />,
  info:    <Info size={16} />,
};

const borderClasses: Record<ToastType, string> = {
  success: 'border-l-4 border-l-success',
  error:   'border-l-4 border-l-danger',
  warning: 'border-l-4 border-l-warning',
  info:    'border-l-4 border-l-accent',
};

const iconClasses: Record<ToastType, string> = {
  success: 'text-success',
  error:   'text-danger',
  warning: 'text-warning',
  info:    'text-accent',
};

export function ToastItem({ toast, onDismiss }: ToastItemProps) {
  return (
    <div
      role="alert"
      className={cn(
        'flex items-start gap-3 px-4 py-3',
        'bg-elevated border border-border rounded-md shadow-lg',
        'pointer-events-auto',
        'animate-[toastIn_200ms_ease_forwards]',
        borderClasses[toast.type],
      )}
    >
      <span className={cn('flex-shrink-0 mt-[1px]', iconClasses[toast.type])}>
        {ICONS[toast.type]}
      </span>
      <span className="flex-1 text-sm text-fg leading-[1.5]">{toast.message}</span>
      <button
        className="flex-shrink-0 mt-[1px] text-fg-subtle hover:text-fg transition-colors duration-150 cursor-pointer"
        onClick={() => onDismiss(toast.id)}
        aria-label="Dismiss"
      >
        <X size={14} />
      </button>
    </div>
  );
}

export function ToastContainer({ toasts, onDismiss }: ToastContainerProps) {
  if (toasts.length === 0) return null;

  return (
    <>
      <div
        className={cn(
          'fixed z-[60] flex flex-col gap-2 pointer-events-none',
          /* Mobile: full-width top strip respecting safe area */
          'top-[env(safe-area-inset-top,0px)] left-0 right-0 px-3 pt-3',
          /* Desktop: top-right corner */
          'md:top-4 md:right-4 md:left-auto md:w-auto md:max-w-[360px] md:px-0 md:pt-0',
        )}
        aria-live="polite"
        aria-label="Notifications"
      >
        {toasts.map((t) => (
          <ToastItem key={t.id} toast={t} onDismiss={onDismiss} />
        ))}
      </div>
      <style>{`
        @keyframes toastIn { from { opacity: 0; transform: translateY(-6px); } to { opacity: 1; transform: translateY(0); } }
      `}</style>
    </>
  );
}
