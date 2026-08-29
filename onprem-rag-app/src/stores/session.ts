// Session store — replaces the old React-context useSession provider.
//
// The JWT itself never lives here (or anywhere in the web layer): the Tauri
// bridge holds it in Rust managed state. This store tracks who is logged in,
// the auth phase, the configured server URL, and the last health probe — the
// things the UI renders and guards on.
import { create } from "zustand";
import type { User } from "../lib/types";
import type { HealthStatus } from "../lib/bridge";

/** Where we are in the connect → authenticate flow. */
export type AuthStatus =
  | "unknown" // haven't checked a stored session yet (startup)
  | "authenticated"
  | "unauthenticated";

interface SessionState {
  /** Server base URL currently configured in the bridge (mirrored for display). */
  serverUrl: string;
  /** Last successful /health probe; null until the Connect screen runs one. */
  health: HealthStatus | null;
  /** True once a /health probe has succeeded against `serverUrl`. */
  connected: boolean;
  user: User | null;
  authStatus: AuthStatus;
  error: string | null;

  setServerUrl: (url: string) => void;
  setHealth: (health: HealthStatus | null) => void;
  setUser: (user: User | null) => void;
  setError: (error: string | null) => void;
  /** Log out locally: drop the user but keep the server connection. */
  clearUser: () => void;
}

export const useSession = create<SessionState>((set) => ({
  serverUrl: "",
  health: null,
  connected: false,
  user: null,
  authStatus: "unknown",
  error: null,

  setServerUrl: (serverUrl) => set({ serverUrl }),
  setHealth: (health) => set({ health, connected: health !== null }),
  setUser: (user) =>
    set({
      user,
      authStatus: user ? "authenticated" : "unauthenticated",
      error: null,
    }),
  setError: (error) => set({ error }),
  clearUser: () => set({ user: null, authStatus: "unauthenticated", error: null }),
}));
