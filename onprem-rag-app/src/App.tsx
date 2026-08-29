// Application root gate: bootstrap → Connect → Login → AppShell.
//
// On mount the bootstrap effect asks the bridge what it already knows:
//   1. The persisted server URL (from the Rust managed state).
//   2. If a URL exists, probe /health immediately so a returning user
//      lands on Login rather than the Connect screen.
//   3. Whether it still holds a valid JWT (isAuthenticated → me).
//
// Until all of that resolves we show a minimal loader.  The gate then
// renders one of three screens depending on the session state:
//
//   authStatus === 'unknown'        → loader (startup, haven't checked yet)
//   authStatus !== 'authenticated' && !connected → <Connect/>  (no server yet)
//   authStatus !== 'authenticated' && connected  → <Login/>
//   authStatus === 'authenticated'              → <AppShell/>
import { useEffect } from 'react';
import { AppShell } from './components/layout/AppShell';
import { useSession } from './stores/session';
import { getServerUrl, isAuthenticated, me, health } from './lib/bridge';
import { Connect, Login } from './features/auth';

function useBootstrap() {
  const setServerUrl = useSession((s) => s.setServerUrl);
  const setHealth    = useSession((s) => s.setHealth);
  const setUser      = useSession((s) => s.setUser);

  useEffect(() => {
    let cancelled = false;

    (async () => {
      // Step 1: load the persisted server URL into the session store mirror.
      let url = '';
      try {
        url = await getServerUrl();
        if (!cancelled && url) setServerUrl(url);
      } catch {
        // No URL persisted yet — the user will go through the Connect screen.
      }

      // Step 2: if a URL is known, probe /health so a returning user lands on
      // Login (connected = true) rather than the Connect screen.
      if (url) {
        try {
          const h = await health();
          if (!cancelled) setHealth(h);
        } catch {
          // Server unreachable — connected stays false; user must re-connect.
        }
      }

      // Step 3: check for a stored JWT and rehydrate the user identity.
      try {
        const authed = await isAuthenticated();
        if (cancelled) return;
        if (authed) {
          const user = await me();
          if (!cancelled) setUser(user);
        } else {
          if (!cancelled) setUser(null);
        }
      } catch {
        if (!cancelled) setUser(null);
      }
    })();

    return () => {
      cancelled = true;
    };
  }, [setServerUrl, setHealth, setUser]);
}

export default function App() {
  useBootstrap();

  const authStatus = useSession((s) => s.authStatus);
  const connected  = useSession((s) => s.connected);

  if (authStatus === 'unknown') {
    return (
      <div className="flex min-h-full items-center justify-center bg-base">
        <p className="text-sm text-fg-muted">Loading…</p>
      </div>
    );
  }

  if (authStatus === 'authenticated') return <AppShell />;
  if (!connected)                     return <Connect />;
  return <Login />;
}
