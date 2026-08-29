import { Component } from 'react';
import type { ErrorInfo, ReactNode } from 'react';
import { AlertTriangle } from 'lucide-react';
import { Button } from './ui';

interface ErrorBoundaryProps {
  children: ReactNode;
}

interface ErrorBoundaryState {
  hasError: boolean;
  error: Error | null;
}

/**
 * App-wide error boundary. Without one, a render-time throw anywhere in the tree
 * unmounts everything and leaves a blank white screen — especially bad on Android
 * where there is no devtools console to inspect. Catches the throw, logs it, and
 * offers a full reload (the only safe recovery for a corrupted render tree).
 *
 * This is intentionally the ONLY global error surface: TanStack Query calls and
 * mutations already toast their own failures per-call, so a global query/mutation
 * error handler would double-report. This boundary covers render throws only.
 */
export default class ErrorBoundary extends Component<ErrorBoundaryProps, ErrorBoundaryState> {
  state: ErrorBoundaryState = { hasError: false, error: null };

  static getDerivedStateFromError(error: Error): ErrorBoundaryState {
    return { hasError: true, error };
  }

  componentDidCatch(error: Error, info: ErrorInfo) {
    // No remote logging on-prem — the console is the sink (dev tools / adb logcat).
    console.error('Unhandled render error:', error, info.componentStack);
  }

  render() {
    if (!this.state.hasError) return this.props.children;

    return (
      <div className="flex min-h-screen flex-col items-center justify-center gap-4 bg-base p-6 text-center">
        <div className="flex h-[72px] w-[72px] items-center justify-center rounded-xl bg-danger-subtle text-danger">
          <AlertTriangle size={32} />
        </div>
        <h1 className="text-xl font-semibold text-fg">Something went wrong</h1>
        <p className="max-w-[420px] text-sm leading-relaxed text-fg-muted">
          The app hit an unexpected error and can&apos;t continue rendering. Reloading usually
          clears it. If it keeps happening, note what you were doing and report it.
        </p>
        {this.state.error?.message && (
          <pre className="max-w-[420px] overflow-x-auto rounded-md border border-border bg-surface px-3 py-2 text-left text-xs text-fg-subtle">
            {this.state.error.message}
          </pre>
        )}
        <Button variant="primary" onClick={() => window.location.reload()}>
          Reload
        </Button>
      </div>
    );
  }
}
