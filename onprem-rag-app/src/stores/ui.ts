// UI store — client-only view state: navigation, sidebar, transient overlays,
// toasts, and the admin "preview as role" switch.
//
// Navigation is a stack, not a URL router: the app renders whichever route is on
// top. This lets Android's hardware back button map directly to `back()`, and
// costs no router dependency. `sidebarCollapsed` is persisted (a pure UI pref);
// nothing else here survives a reload.
import { create } from "zustand";
import { persist, subscribeWithSelector } from "zustand/middleware";
import type { Role, Route } from "../lib/types";

export interface Toast {
  id: string;
  type: "success" | "error" | "warning" | "info";
  message: string;
  duration?: number; // ms; 0/undefined handled by the caller
}

interface UiState {
  navStack: Route[];
  sidebarCollapsed: boolean;
  /** Generic named overlay slot for mobile sheets/drawers (e.g. "nav", "conversations"). */
  activeSheet: string | null;
  toasts: Toast[];
  /** Admin-only: preview the app as another role. null = use the real role. */
  previewRole: Role | null;

  // Navigation
  navigate: (route: Route) => void;
  back: () => void;
  resetTo: (route: Route) => void;

  // Sidebar
  toggleSidebar: () => void;
  setSidebarCollapsed: (collapsed: boolean) => void;

  // Overlays
  openSheet: (name: string) => void;
  closeSheet: () => void;

  // Toasts
  pushToast: (t: Omit<Toast, "id">) => string;
  dismissToast: (id: string) => void;

  // Role preview
  setPreviewRole: (role: Role | null) => void;
}

let toastCounter = 0;

export const useUi = create<UiState>()(
  subscribeWithSelector(
    persist(
      (set, get) => ({
        navStack: ["/"],
        sidebarCollapsed: false,
        activeSheet: null,
        toasts: [],
        previewRole: null,

        navigate: (route) =>
          set((s) => {
            const top = s.navStack[s.navStack.length - 1];
            if (top === route) return s; // replace-if-same: no dupe push
            return { navStack: [...s.navStack, route], activeSheet: null };
          }),
        back: () =>
          set((s) =>
            s.navStack.length > 1
              ? { navStack: s.navStack.slice(0, -1) }
              : s,
          ),
        resetTo: (route) => set({ navStack: [route], activeSheet: null }),

        toggleSidebar: () => set((s) => ({ sidebarCollapsed: !s.sidebarCollapsed })),
        setSidebarCollapsed: (sidebarCollapsed) => set({ sidebarCollapsed }),

        openSheet: (activeSheet) => set({ activeSheet }),
        closeSheet: () => set({ activeSheet: null }),

        pushToast: (t) => {
          const id = `toast-${++toastCounter}`;
          set((s) => ({ toasts: [...s.toasts, { ...t, id }] }));
          const duration = t.duration ?? (t.type === "error" ? 6000 : 4000);
          if (duration > 0) {
            setTimeout(() => get().dismissToast(id), duration);
          }
          return id;
        },
        dismissToast: (id) =>
          set((s) => ({ toasts: s.toasts.filter((x) => x.id !== id) })),

        setPreviewRole: (previewRole) => set({ previewRole }),
      }),
      {
        name: "onprem-ui",
        // Persist only the sidebar preference; nav/toasts/overlays are ephemeral.
        partialize: (s) => ({ sidebarCollapsed: s.sidebarCollapsed }),
      },
    ),
  ),
);

/** The route currently on top of the stack — what the shell should render. */
export const currentRoute = (s: UiState): Route => s.navStack[s.navStack.length - 1];

/** Convenience toast API mirroring the reference's `toast.success(...)` ergonomics. */
export const toast = {
  success: (message: string, duration?: number) =>
    useUi.getState().pushToast({ type: "success", message, duration }),
  error: (message: string, duration?: number) =>
    useUi.getState().pushToast({ type: "error", message, duration }),
  warning: (message: string, duration?: number) =>
    useUi.getState().pushToast({ type: "warning", message, duration }),
  info: (message: string, duration?: number) =>
    useUi.getState().pushToast({ type: "info", message, duration }),
};
