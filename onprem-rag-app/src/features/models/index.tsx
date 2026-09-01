// Models — model management screen (Stage 8, Layer 3b) of the mobile-first rebuild.
// Mobile-first: single column at 360 px; role sections stack; the variant grids
// inside them scale up (md:2 / xl:3). Tap targets ≥ 44 px. Dark theme only.
//
// Two queries back the page:
//   ['model-roles']   → getModelRoles(): the REAL role→variant manifest (catalog).
//                       Variants already carry live cached/loaded/current flags.
//   ['setup-status']  → getSetupStatus(): drives the Foundry ready/down banner. This
//                       reuses the exact key Settings polls, so the cache is shared.
//
// Governing reshape (plans/11): our Foundry is an in-process native core, so there is
// NO start/stop/restart lifecycle. The only admin lifecycle-ish action is
// "Re-register execution providers" (re-runs EP discovery). When the core is down we
// show a clear banner but KEEP the role manifest visible — roles come from config even
// when the core is unavailable (their variants may just be empty).
//
// Gating rule: every mutating control is gated on the user's REAL role (useSession →
// 'admin'), never the admin "Preview as" role.
import { useQuery, useQueryClient } from '@tanstack/react-query';
import { RefreshCw, Cpu, AlertTriangle } from 'lucide-react';
import { getModelRoles, getSetupStatus, registerEps } from '../../lib/bridge';
import { toast } from '../../stores/ui';
import { useSession } from '../../stores/session';
import { Button, Badge, cn } from '../../components/ui';
import { PageContainer } from '../../components/layout/PageContainer';
import ServiceConsole from '../../components/ServiceConsole';
import RoleSection from './RoleSection';
import { useState } from 'react';

export default function Models() {
  const queryClient = useQueryClient();

  // Gate mutating controls on the REAL role, not the previewRole (see header note).
  const isAdmin = useSession((s) => s.user?.role) === 'admin';

  const [reregistering, setReregistering] = useState(false);

  const rolesQuery = useQuery({
    queryKey: ['model-roles'],
    queryFn: getModelRoles,
    refetchInterval: 12_000, // keep cached/loaded flags live (loads can happen elsewhere)
    staleTime: 30_000,
  });

  // Shares the key + queryFn Settings uses so both screens read one cached status.
  const statusQuery = useQuery({
    queryKey: ['setup-status'],
    queryFn: getSetupStatus,
    refetchInterval: 12_000, // keep foundry readiness live without a manual refresh
    staleTime: 30_000,
  });

  const foundryReady = statusQuery.data?.foundry_ready ?? false;
  const isFetching = rolesQuery.isFetching || statusQuery.isFetching;

  // Refresh + post-mutation invalidation both touch BOTH keys — a download/load
  // changes variant flags (model-roles) and readiness/model lists (setup-status).
  function refreshAll() {
    queryClient.invalidateQueries({ queryKey: ['model-roles'] });
    queryClient.invalidateQueries({ queryKey: ['setup-status'] });
  }

  async function handleReregister() {
    setReregistering(true);
    try {
      const result = await registerEps();
      if (result.success) {
        toast.success(`Execution providers re-registered (${result.registered.length} active).`);
      } else {
        // Partial/failed registration still returns — surface it as an error toast.
        toast.error(`EP registration reported failures: ${result.failed.join(', ') || result.status}`);
      }
      refreshAll();
    } catch (err) {
      toast.error(String(err));
    } finally {
      setReregistering(false);
    }
  }

  const roles = rolesQuery.data ?? [];

  return (
    <PageContainer variant="board">
    <div className="flex flex-col gap-5">

      {/* ── Header + actions ────────────────────────────────────────────────── */}
      <div className="flex flex-col gap-3 sm:flex-row sm:items-start sm:justify-between">
        <div>
          <h1 className="text-xl font-bold text-fg">Models</h1>
          <p className="text-sm text-fg-muted mt-0.5">
            Download, load, and route the local models powering chat, embeddings, and reranking.
          </p>
        </div>
        <div className="flex flex-col gap-2 sm:flex-row sm:shrink-0">
          {/* NO start/stop/restart — the only lifecycle action is re-running EP discovery. */}
          {isAdmin && (
            <Button
              variant="secondary"
              leftIcon={<RefreshCw size={16} />}
              loading={reregistering}
              onClick={handleReregister}
              className="w-full sm:w-auto min-h-[44px]"
            >
              Re-register EPs
            </Button>
          )}
          <Button
            variant="secondary"
            leftIcon={<RefreshCw size={16} className={isFetching ? 'animate-spin' : undefined} />}
            onClick={refreshAll}
            className="w-full sm:w-auto min-h-[44px]"
          >
            Refresh
          </Button>
        </div>
      </div>

      {/* ── Foundry status bar ──────────────────────────────────────────────── */}
      <div className={cn(
        'flex items-center gap-3 rounded-lg border px-4 py-3',
        foundryReady ? 'border-border bg-surface' : 'border-warning/25 bg-warning-subtle',
      )}>
        <span className="flex-shrink-0">
          {foundryReady ? <Cpu size={18} className="text-fg-muted" /> : <AlertTriangle size={18} className="text-warning" />}
        </span>
        <div className="flex flex-col min-w-0 flex-1">
          <span className="text-sm font-semibold text-fg">Foundry Local core</span>
          <span className="text-xs text-fg-muted break-words">
            {foundryReady
              ? 'In-process (native SDK) — ready.'
              : 'Foundry Local core unavailable. Roles below come from config; variant actions are limited until it recovers.'}
          </span>
        </div>
        <Badge variant={foundryReady ? 'success' : 'error'} dot>
          {foundryReady ? 'ready' : 'down'}
        </Badge>
      </div>

      {/* ── Role sections ───────────────────────────────────────────────────── */}
      {rolesQuery.isLoading ? (
        <p className="text-sm text-fg-muted">Loading model roles…</p>
      ) : (
        <div className="flex flex-col gap-4">
          {roles.map((role) => (
            <RoleSection
              key={role.role}
              role={role}
              isAdmin={isAdmin}
              onChanged={refreshAll}
            />
          ))}
        </div>
      )}

      {/* ── Activity ────────────────────────────────────────────────────────── */}
      <ServiceConsole />
    </div>
    </PageContainer>
  );
}
