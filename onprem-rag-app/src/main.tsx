import React, { useEffect } from "react";
import ReactDOM from "react-dom/client";
import { QueryClientProvider } from "@tanstack/react-query";
import App from "./App";
import { queryClient } from "./lib/queryClient";
import { initBridgeEvents } from "./lib/bridgeEvents";
import { setSessionExpiredHandler } from "./lib/bridge";
import ErrorBoundary from "./components/ErrorBoundary";
import { ToastContainer } from "./components/ui";
import { useUi, toast } from "./stores/ui";
import { useSession } from "./stores/session";
import "./styles/theme.css";

/**
 * The one and only ToastContainer mount (the reference mounted it twice). Reads
 * the toast queue and dismiss action straight from the ui store.
 */
function Toasts() {
  const toasts = useUi((s) => s.toasts);
  const dismiss = useUi((s) => s.dismissToast);
  return <ToastContainer toasts={toasts} onDismiss={dismiss} />;
}

/** Registers the global Tauri event listeners once, at app boot. */
function BridgeEvents() {
  useEffect(() => {
    setSessionExpiredHandler(() => {
      const { authStatus, clearUser } = useSession.getState();
      const wasAuthed = authStatus === "authenticated";
      clearUser();
      // Drop the React Query cache — it holds persisted chat messages (PHI);
      // a forced logout must not leave transcripts around for the next user.
      queryClient.clear();
      // Only toast when we were actually signed in. Concurrent 401s dedupe
      // naturally: clearUser() flips authStatus before the next handler runs, so
      // the second and later handlers see a non-authenticated status and stay quiet.
      if (wasAuthed) toast.warning("Your session expired. Please sign in again.");
    });
    // These listeners live for the application lifetime. StrictMode may replay this
    // effect, so cleaning them up here races their asynchronous registration.
    void initBridgeEvents();
  }, []);
  return null;
}

ReactDOM.createRoot(document.getElementById("root") as HTMLElement).render(
  <React.StrictMode>
    <QueryClientProvider client={queryClient}>
      <BridgeEvents />
      <ErrorBoundary>
        <App />
      </ErrorBoundary>
      <Toasts />
    </QueryClientProvider>
  </React.StrictMode>,
);
