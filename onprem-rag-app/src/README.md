# `onprem-rag-app/src` — frontend architecture

Feature-based React structure. Keep cross-cutting code thin and shared; keep each
screen's code inside its own `features/<name>/` folder.

```
src/
  main.tsx              Entry. Mounts providers (QueryClient), boots bridge events.
  App.tsx               Top gate: Connect-to-server → Login → AppShell, by session state.
  app/
    navigation.ts       NAV metadata — the sidebar/rail/bottom-bar items, grouped by section.
    routes.tsx          Route → lazy feature page registry (React.lazy + Suspense).
  components/
    ui/                 Design-system primitives: Button, Input, Select, Badge, Modal,
                        Toast, Card, StatCard, EmptyState, StubPage, EcgLogo. No feature logic.
    layout/             App chrome: AppShell + Sidebar / IconRail / BottomBar / TopBar / NavSheet.
  features/<name>/      One self-contained folder per screen. A page entry (index.tsx) plus
                        that feature's local components / hooks / query keys. Added per build stage.
  stores/               Global Zustand stores: session, ui (nav stack + toasts), stream (SSE buffers).
  lib/                  Cross-cutting: bridge.ts (typed Tauri commands), bridgeEvents.ts (single
                        listener registry), queryClient.ts, permissions.ts, types.ts.
  styles/theme.css      Tailwind v4 @theme design tokens (dark-only) + base layer.
  hooks/                Shared hooks used by more than one feature.
```

## Rules

- **Server state → TanStack Query** (`lib/queryClient.ts`); **client state → Zustand** (`stores/`).
- The JWT never enters the web layer — it lives in the Rust bridge (`src-tauri`). The app calls
  `lib/bridge.ts` wrappers, which `invoke` bridge commands.
- **One listener per bridge event**, registered in `lib/bridgeEvents.ts` at boot. Never scatter
  `listen`/`unlisten` across components. Hot streams (tokens, logs) batch on `requestAnimationFrame`.
- Navigation is a **stack in the `ui` store** (`navStack`), not a URL router — Android's hardware
  back maps to `back()`. Route access is gated by `lib/permissions.ts` (`canAccess`).
- A feature imports from `components/ui`, `stores`, and `lib`; features do **not** import each other.
