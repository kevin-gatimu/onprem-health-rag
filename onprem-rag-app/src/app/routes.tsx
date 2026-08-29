// Route → page-element registry. Each stage swaps a StubPage entry for the real
// feature page (React.lazy'd from features/<name>), so the shell only ever knows
// routes, never feature internals.
//
// Stage 2: Dashboard wired. Remaining routes are StubPage until their stage lands.
// The <Suspense> wrapper that makes lazy() work is in AppShell — do not add another.
import { createElement, lazy } from 'react';
import type { ComponentType } from 'react';
import { StubPage } from '../components/ui';
import { navItem } from './navigation';
import type { Route } from '../lib/types';

// Routes with real pages. The cast is safe: all pages are zero-prop React components.
const registry: Partial<Record<Route, ComponentType>> = {
  '/':            lazy(() => import('../features/dashboard'))    as ComponentType,
  '/connections': lazy(() => import('../features/connections'))  as ComponentType,
  '/ingest':      lazy(() => import('../features/ingest'))       as ComponentType,
  '/data':        lazy(() => import('../features/data-explorer')) as ComponentType,
  '/chat':        lazy(() => import('../features/chat'))         as ComponentType,
  '/agents':      lazy(() => import('../features/agents'))       as ComponentType,
  '/models':      lazy(() => import('../features/models'))       as ComponentType,
  '/settings':    lazy(() => import('../features/settings'))     as ComponentType,
  '/profile':     lazy(() => import('../features/profile'))      as ComponentType,
  '/admin':       lazy(() => import('../features/admin'))        as ComponentType,
  '/audit':       lazy(() => import('../features/audit'))        as ComponentType,
  '/analytics':   lazy(() => import('../features/analytics'))    as ComponentType,
  '/alerts':      lazy(() => import('../features/alerts'))       as ComponentType,
};

/** Render the page element for a route. */
export function renderRoute(route: Route) {
  const Page = registry[route];
  if (Page) return <Page />;

  // Fallback: placeholder while the feature screen is still being built.
  const item = navItem(route);
  const icon = item ? createElement(item.icon, { size: 32 }) : undefined;
  return (
    <StubPage
      title={item?.label ?? route}
      description="This screen is being rebuilt — coming in the next phase."
      icon={icon}
    />
  );
}
