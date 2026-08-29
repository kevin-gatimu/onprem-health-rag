/**
 * Login — two-panel branded sign-in screen, mobile-first.
 *
 * Layout:
 *   mobile  (< md): right form panel fills the full viewport; brand panel hidden.
 *   desktop (≥ md): left brand panel + right form panel side-by-side.
 *
 * The JWT never touches this layer — we call the bridge wrapper, which stores
 * the token in Rust managed state and returns only the user identity.
 */
import { useState, type FormEvent } from 'react';
import { Mail, Lock, AlertCircle, WifiOff, Database, Brain, Shield } from 'lucide-react';
import { login } from '../../lib/bridge';
import { useSession } from '../../stores/session';
import { toast } from '../../stores/ui';
import { Button, Input, EcgLogo } from '../../components/ui';
import LoginBackground from './LoginBackground';

export default function Login() {
  const serverUrl = useSession((s) => s.serverUrl);
  const setHealth = useSession((s) => s.setHealth);

  const [identifier, setIdentifier] = useState('');
  const [password,   setPassword]   = useState('');
  const [loading,    setLoading]    = useState(false);
  const [error,      setError]      = useState<string | null>(null);

  const canSubmit = identifier.trim().length > 0 && password.length > 0 && !loading;

  async function handleSubmit(e: FormEvent) {
    e.preventDefault();
    if (!canSubmit) return;

    setLoading(true);
    setError(null);
    try {
      const user = await login(identifier.trim(), password);
      useSession.getState().setUser(user);
    } catch (err) {
      const msg = String(err);
      setError(msg);
      toast.error(msg);
    } finally {
      setLoading(false);
    }
  }

  // Return to Connect screen — clears the health probe so the gate re-evaluates.
  function handleChangeServer() {
    setHealth(null);
  }

  return (
    <div className="flex h-full w-full">
      {/* ── Left brand panel — hidden on mobile, shown at md ── */}
      <div className="hidden md:flex flex-col flex-1 relative bg-base overflow-hidden">
        {/* ECG animated background — absolutely positioned behind brand content */}
        <LoginBackground />

        {/* Brand content — sits above the background */}
        <div className="relative z-10 flex flex-col justify-center h-full px-10 py-12 gap-8">
          <div className="flex flex-col gap-4">
            <EcgLogo size={48} className="text-accent" />
            <h1 className="text-2xl font-bold text-fg leading-tight">
              Health Records Ingest
            </h1>
            <p className="text-sm text-fg-muted max-w-xs leading-relaxed">
              Secure, offline-first clinical data management and AI-powered
              decision support — built for Kenyan clinics.
            </p>
          </div>

          {/* Feature rows */}
          <div className="flex flex-col gap-3">
            {[
              { Icon: WifiOff,  text: '100% offline — no internet required' },
              { Icon: Database, text: 'MySQL, PostgreSQL, MSSQL & MongoDB' },
              { Icon: Brain,    text: 'Local AI via Microsoft Foundry Local' },
              { Icon: Shield,   text: 'Kenya Data Protection Act 2019 compliant' },
            ].map(({ Icon, text }) => (
              <div
                key={text}
                className="flex items-center gap-3 text-sm text-fg-muted min-h-[44px]"
              >
                <span className="flex items-center justify-center w-8 h-8 rounded-md bg-accent-subtle text-accent shrink-0">
                  <Icon size={16} />
                </span>
                {text}
              </div>
            ))}
          </div>

          {/* "All data stays on this device" badge */}
          <div className="inline-flex items-center gap-2 px-3 py-2 rounded-full bg-success-subtle border border-success/20 text-success text-xs font-medium w-fit">
            <span className="w-2 h-2 rounded-full bg-success shrink-0" />
            All data stays on this device
          </div>
        </div>
      </div>

      {/* ── Right form panel ── */}
      <div className="w-full md:w-[420px] lg:w-[460px] flex flex-col items-center justify-center bg-base px-4 py-8">
        <div className="w-full max-w-sm space-y-6">
          {/* Mobile-only: show logo + app name since the brand panel is hidden */}
          <div className="flex flex-col items-center gap-2 md:hidden">
            <EcgLogo size={32} className="text-accent" />
            <span className="text-base font-semibold text-fg">Health Records Ingest</span>
          </div>

          {/* Form card */}
          <div className="rounded-xl border border-border bg-surface shadow-md px-6 py-8 space-y-5">
            <div className="space-y-1">
              <h2 className="text-lg font-semibold text-fg">Sign in</h2>
              <p className="text-sm text-fg-muted">Enter your credentials to access the system.</p>
            </div>

            <form onSubmit={handleSubmit} noValidate className="space-y-4">
              <Input
                label="Email or username"
                icon={<Mail size={16} />}
                type="text"
                autoFocus
                autoComplete="username"
                value={identifier}
                onChange={(e) => setIdentifier(e.target.value)}
                disabled={loading}
              />

              <Input
                label="Password"
                icon={<Lock size={16} />}
                type="password"
                showToggle
                autoComplete="current-password"
                value={password}
                onChange={(e) => setPassword(e.target.value)}
                disabled={loading}
              />

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
                loading={loading}
                disabled={!canSubmit}
              >
                Sign in
              </Button>
            </form>

            <p className="text-xs text-fg-subtle text-center">
              Credentials are managed by your system administrator.
            </p>
          </div>

          {/* "Change server" footer — lets the user go back to the Connect screen */}
          <div className="flex flex-col items-center gap-1">
            <button
              type="button"
              onClick={handleChangeServer}
              className="text-xs text-fg-subtle hover:text-fg-muted transition-colors duration-150 min-h-[44px] px-2"
            >
              Change server
            </button>
            {serverUrl && (
              <span className="text-xs text-fg-disabled truncate max-w-xs">{serverUrl}</span>
            )}
          </div>
        </div>
      </div>
    </div>
  );
}
