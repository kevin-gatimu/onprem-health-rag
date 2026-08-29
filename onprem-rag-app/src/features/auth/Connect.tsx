/**
 * Connect — first-run / server-change screen.
 *
 * Lets the user point the app at an on-prem RAG server before logging in.
 * Probes /health, stores the result in the session store (which flips
 * `connected` true and advances the gate to Login), and shows an inline
 * danger banner if the server is unreachable.
 *
 * A warning toast is shown (but connect still succeeds) when the server
 * is reachable but DocumentDB reports "down" — the user may be connecting
 * before DocumentDB finishes booting.
 *
 * The JWT never touches this component — we only call bridge wrappers.
 */
import { useState, type FormEvent } from 'react';
import { Server, AlertCircle } from 'lucide-react';
import { setServerUrl as bridgeSetServerUrl, health } from '../../lib/bridge';
import { useSession } from '../../stores/session';
import { toast } from '../../stores/ui';
import { Button, Input, EcgLogo } from '../../components/ui';

export default function Connect() {
  const storedUrl  = useSession((s) => s.serverUrl);
  const setHealth  = useSession((s) => s.setHealth);

  // Pre-fill from persisted URL, fall back to localhost default.
  const [url,     setUrl]     = useState(storedUrl || 'http://localhost:8000');
  const [probing, setProbing] = useState(false);
  const [error,   setError]   = useState<string | null>(null);

  async function handleSubmit(e: FormEvent) {
    e.preventDefault();
    const trimmed = url.trim();
    if (!trimmed || probing) return;

    setProbing(true);
    setError(null);
    try {
      // Persist the URL in the bridge's Rust state before probing.
      await bridgeSetServerUrl(trimmed);
      // Sync the mirror into the session store so the Login screen displays it.
      useSession.getState().setServerUrl(trimmed);

      const h = await health();
      // Even if DocumentDB is down we allow connecting — the user can still
      // browse settings. A warning makes the situation visible without blocking.
      if (h.documentdb === 'down') {
        toast.warning('Server reachable but DocumentDB is down');
      }
      setHealth(h);   // flips connected = true → gate advances to Login
    } catch (err) {
      const msg = `Could not reach server: ${String(err)}`;
      setError(msg);
      toast.error(msg);
    } finally {
      setProbing(false);
    }
  }

  return (
    <div className="flex min-h-full w-full items-center justify-center bg-base px-4 py-8">
      <div className="w-full max-w-sm space-y-6">
        {/* Header */}
        <div className="flex flex-col items-center gap-3 text-center">
          <EcgLogo size={40} className="text-accent" />
          <div className="space-y-1">
            <h1 className="text-xl font-semibold text-fg">Connect to server</h1>
            <p className="text-sm text-fg-muted">
              Enter the address of your on-prem RAG server.
            </p>
          </div>
        </div>

        {/* Connect card */}
        <div className="rounded-xl border border-border bg-surface shadow-md px-6 py-8 space-y-5">
          <form onSubmit={handleSubmit} noValidate className="space-y-4">
            <Input
              label="Server URL"
              icon={<Server size={16} />}
              type="url"
              placeholder="http://localhost:8000"
              value={url}
              onChange={(e) => setUrl(e.target.value)}
              disabled={probing}
              autoFocus
              autoComplete="url"
            />

            {/* Hint: DocumentDB status visible after a successful connect */}
            <p className="text-xs text-fg-subtle">
              After connecting, DocumentDB status will be shown in the header.
            </p>

            {/* Error banner */}
            {error && (
              <div
                role="alert"
                className="flex items-start gap-2 px-3 py-2 rounded-md bg-danger-subtle border border-danger/30 text-danger text-sm"
              >
                <AlertCircle size={16} className="shrink-0 mt-0.5" />
                <span>{error}</span>
              </div>
            )}

            <Button
              type="submit"
              full
              loading={probing}
              disabled={!url.trim() || probing}
            >
              Connect
            </Button>
          </form>
        </div>
      </div>
    </div>
  );
}
