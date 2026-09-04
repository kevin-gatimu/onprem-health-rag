// Models — model management screen (Stage 8, Layer 3b) of the mobile-first rebuild.
// Mobile-first: single column at 360 px; sections stack; the variant grid inside the
// manage-variants disclosure scales up (md:2 / xl:3). Tap targets ≥ 44 px. Dark theme only.
//
// Shape: ONE shared local LLM now drives every generative role (chat, health_query,
// trends, summarize, lookup, fast, classify, extractor, verifier) plus nl2sql — there
// is no per-role model choice left. The page renders:
//   - SharedLlmCard (the "chat" role)  → picks + downloads/loads/applies the one LLM,
//     with live progress when the chosen variant isn't cached yet.
//   - a collapsible "Manage downloaded variants" admin grid (VariantCard, fed by the
//     chat role's variants) for unloading/deleting/inspecting what's on disk.
//   - RoleSection for the remaining NON-managed (fastembed) roles: embeddings, reranker.
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
import { RefreshCw, Cpu, AlertTriangle, ChevronDown, ChevronRight } from 'lucide-react';
import { getModelRoles, getSetupStatus, registerEps } from '../../lib/bridge';
import { toast } from '../../stores/ui';
import { useSession } from '../../stores/session';
import { Button, Badge, cn } from '../../components/ui';
import { PageContainer } from '../../components/layout/PageContainer';
import ServiceConsole from '../../components/ServiceConsole';
import RoleSection from './RoleSection';
import SharedLlmCard from './SharedLlmCard';
import VariantCard from './VariantCard';
import { useState } from 'react';

export default function Models() {
  const queryClient = useQueryClient();

  // Gate mutating controls on the REAL role, not the previewRole (see header note).
  const isAdmin = useSession((s) => s.user?.role) === 'admin';

  const [reregistering, setReregistering] = useState(false);
  const [manageOpen, setManageOpen] = useState(false);


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
  const sharedLlmRole = roles.find((role) => role.role === 'chat');
  // Every other managed role is now served by the shared LLM — only fastembed
  // (non-managed) roles still get their own section.
  const remainingRoles = roles.filter((role) => !role.managed);
  const manageableVariants = (sharedLlmRole?.variants ?? []).filter(
    (variant) => variant.cached || variant.loaded,
  );


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
              className="min-h-11 w-full sm:w-auto"
            >
              Re-register EPs
            </Button>
          )}
          <Button
            variant="secondary"
            leftIcon={<RefreshCw size={16} className={isFetching ? 'animate-spin' : undefined} />}
            onClick={refreshAll}
            className="min-h-11 w-full sm:w-auto"
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
        <span className="shrink-0">
          {foundryReady ? <Cpu size={18} className="text-fg-muted" /> : <AlertTriangle size={18} className="text-warning" />}
        </span>
        <div className="flex flex-col min-w-0 flex-1">
          <span className="text-sm font-semibold text-fg">Foundry Local core</span>
          <span className="wrap-break-word text-xs text-fg-muted">
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
          {sharedLlmRole && (
            <SharedLlmCard
              role={sharedLlmRole}
              isAdmin={isAdmin}
              executionProviders={statusQuery.data?.execution_providers ?? []}
              onChanged={refreshAll}
            />
          )}

          {/* ── Manage downloaded variants (admin only) ─────────────────────── */}
          {/* Downloading a NEW variant happens via the Core LLM card above; this
              disclosure is only for managing what's already on disk (unload/delete/
              inspect), so it's filtered to cached-or-loaded and collapsed by default
              to keep the page compact. */}
          {isAdmin && sharedLlmRole && manageableVariants.length > 0 && (
            <div className="flex flex-col gap-3">
              <button
                type="button"
                onClick={() => setManageOpen((v) => !v)}
                className="flex min-h-11 items-center gap-2 self-start rounded-md px-1 text-sm font-medium text-fg-muted hover:text-fg transition-colors"
              >
                {manageOpen ? <ChevronDown size={16} /> : <ChevronRight size={16} />}
                Manage downloaded variants
              </button>
              {manageOpen && (
                <div className="grid gap-3 md:grid-cols-2 xl:grid-cols-3">
                  {manageableVariants.map((variant) => (
                    <VariantCard
                      key={variant.id}
                      variant={variant}
                      role="chat"
                      isDefault={sharedLlmRole.override_variant === variant.id}
                      isAdmin={isAdmin}
                      onChanged={refreshAll}
                    />
                  ))}
                </div>
              )}
            </div>
          )}

          {remainingRoles.map((role) => (
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
