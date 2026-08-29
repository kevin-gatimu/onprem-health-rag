// Settings — System Setup screen (Stage 8, Layer 3a) of the mobile-first UI rebuild.
// Mobile-first: designed at 360 px (single column), then md: grids scale up. Tap
// targets ≥ 44 px. Dark theme only.
//
// One `GET /setup-status` call (polled every 8 s) powers the whole page: hardware,
// per-service health, Foundry readiness, the active/loaded/cached model lists, and
// execution providers. The page is degraded-safe — when Foundry's native core is
// down the model lists come back empty and the Foundry service reads "error".
//
// Governing reshape (see plans/11): our Foundry is an in-process native core, so
// there is NO start/stop/restart lifecycle and NO Docker lifecycle. The only admin
// lifecycle-ish action is "Re-register execution providers" (re-runs EP discovery).
//
// Gating rule: every MUTATING control is gated on the user's REAL role
// (`useSession` → 'admin'), never the admin "Preview as" role — previewing as a
// non-admin must not expose a real admin the ability to actually mutate, and
// (conversely) preview state must not hide controls the real admin should keep.
import { useMemo, useState } from 'react';
import type { ReactNode } from 'react';
import {
  Cpu, Zap, RefreshCw, Server, HardDrive, Boxes, ExternalLink,
} from 'lucide-react';
import { useQuery, useQueryClient } from '@tanstack/react-query';
import {
  getSetupStatus, registerEps, selectModel, unloadModel,
} from '../../lib/bridge';
import type { ServiceStatus } from '../../lib/bridge';
import { toast, useUi } from '../../stores/ui';
import { useSession } from '../../stores/session';
import { Button, Select, Badge, Card, EmptyState, cn } from '../../components/ui';
import { PageContainer } from '../../components/layout/PageContainer';
import ServiceConsole from '../../components/ServiceConsole';

// ── Helpers ─────────────────────────────────────────────────────────────────────

/** Colour token for a service status dot / text. */
function statusTone(status: string): { dot: string; badge: 'success' | 'error' | 'neutral' } {
  switch (status) {
    case 'ok':    return { dot: 'bg-success', badge: 'success' };
    case 'error': return { dot: 'bg-danger',  badge: 'error' };
    default:      return { dot: 'bg-fg-subtle', badge: 'neutral' }; // "unknown"
  }
}

// A compact labelled value row used inside the Hardware card. Kept local (not a new
// primitive) — long model ids stay small and wrap instead of overflowing at 360 px.
function InfoRow({ icon, label, value }: { icon: ReactNode; label: string; value: ReactNode }) {
  return (
    <div className="flex items-start gap-3">
      <span className="flex-shrink-0 flex items-center justify-center w-9 h-9 rounded-md bg-accent-subtle text-accent">
        {icon}
      </span>
      <div className="flex flex-col min-w-0">
        <span className="text-xs text-fg-muted">{label}</span>
        <span className="text-sm font-medium text-fg break-words">{value}</span>
      </div>
    </div>
  );
}

// ── ServiceCard sub-component ─────────────────────────────────────────────────

function ServiceCard({ service }: { service: ServiceStatus }) {
  const tone = statusTone(service.status);
  return (
    <div className="rounded-lg border border-border bg-surface p-4 flex flex-col gap-2">
      <div className="flex items-center gap-2">
        <span className={cn('w-2 h-2 rounded-full flex-shrink-0', tone.dot)} aria-hidden="true" />
        <span className="text-sm font-semibold text-fg truncate">{service.name}</span>
      </div>
      {service.detail && (
        <span className="text-xs text-fg-muted break-words">{service.detail}</span>
      )}
    </div>
  );
}

// ── Settings ──────────────────────────────────────────────────────────────────

export default function Settings() {
  const queryClient = useQueryClient();
  const navigate = useUi((s) => s.navigate);

  // Gate mutating controls on the REAL role, not the previewRole (see header note).
  const isAdmin = useSession((s) => s.user?.role) === 'admin';

  // Per-action loading flags. Kept as local state (not useMutation) because these
  // touch several controls and read cleaner as plain async handlers here.
  const [reregistering, setReregistering] = useState(false);
  const [switching, setSwitching]         = useState(false);
  const [unloading, setUnloading]         = useState(false);

  const { data: status, isLoading, isFetching } = useQuery({
    queryKey: ['setup-status'],
    queryFn: getSetupStatus,
    refetchInterval: 8000, // keep readiness/model lists live without a manual refresh
    staleTime: 30_000,
  });

  function refreshStatus() {
    queryClient.invalidateQueries({ queryKey: ['setup-status'] });
  }

  // Deduped union of cached + loaded model ids for the switch dropdown, with a flag
  // marking which are already resident (so the label can hint "(loaded)").
  const modelOptions = useMemo(() => {
    if (!status) return [] as { id: string; loaded: boolean }[];
    const loaded = new Set(status.loaded_models);
    const seen = new Set<string>();
    const out: { id: string; loaded: boolean }[] = [];
    for (const id of [...status.cached_models, ...status.loaded_models]) {
      if (seen.has(id)) continue;
      seen.add(id);
      out.push({ id, loaded: loaded.has(id) });
    }
    return out;
  }, [status]);

  // ── Actions (admin only) ────────────────────────────────────────────────────

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
      refreshStatus();
    } catch (err) {
      toast.error(String(err));
    } finally {
      setReregistering(false);
    }
  }

  async function handleSwitchModel(id: string) {
    if (!id) return;
    setSwitching(true);
    try {
      // select_model downloads-if-needed → loads → sets current, so it can take a while.
      const resolved = await selectModel(id);
      toast.success(`Active chat model set to ${resolved}.`);
      refreshStatus();
    } catch (err) {
      toast.error(String(err));
    } finally {
      setSwitching(false);
    }
  }

  async function handleUnloadAll() {
    if (!status || status.loaded_models.length === 0) return;
    setUnloading(true);
    try {
      // Server-side unload is per-variant; "Unload all" is a client fan-out (idempotent).
      await Promise.all(status.loaded_models.map((id) => unloadModel(id)));
      toast.success('All models unloaded.');
      refreshStatus();
    } catch (err) {
      toast.error(String(err));
    } finally {
      setUnloading(false);
    }
  }

  // ── Derived display values ────────────────────────────────────────────────────

  const gpuLabel = status
    ? status.gpu.has_gpu
      ? (status.gpu.gpu_name ?? 'GPU detected')
      : 'CPU-only mode'
    : '—';
  const activeModel = status && status.active_chat_model ? status.active_chat_model : 'none loaded';
  // DocumentDB guidance appears only when its service line reports an error.
  const docDbErrored = status?.services.some((s) => s.name === 'DocumentDB' && s.status === 'error');

  return (
    <PageContainer variant="flow">
    <div className="flex flex-col gap-5">

      {/* ── Header + Refresh ─────────────────────────────────────────────────── */}
      <div className="flex flex-col gap-3 sm:flex-row sm:items-start sm:justify-between">
        <div>
          <h1 className="text-xl font-bold text-fg">System Setup</h1>
          <p className="text-sm text-fg-muted mt-0.5">
            Hardware, local AI services, and the active chat model — all on-premises.
          </p>
        </div>
        <div className="sm:shrink-0">
          <Button
            variant="secondary"
            leftIcon={<RefreshCw size={16} className={isFetching ? 'animate-spin' : undefined} />}
            onClick={refreshStatus}
            className="w-full sm:w-auto min-h-[44px]"
          >
            Refresh
          </Button>
        </div>
      </div>

      {isLoading ? (
        <p className="text-sm text-fg-muted">Loading system status…</p>
      ) : (
        <>
          {/* ── Hardware ───────────────────────────────────────────────────────── */}
          <Card title="Hardware">
            <div className="grid gap-4 md:grid-cols-2">
              <InfoRow
                icon={status?.gpu.has_gpu ? <Zap size={18} /> : <Cpu size={18} />}
                label="Accelerator"
                value={gpuLabel}
              />
              <InfoRow
                icon={<Boxes size={18} />}
                label="Active chat model"
                value={activeModel}
              />
            </div>
          </Card>

          {/* ── Service Health ─────────────────────────────────────────────────── */}
          <div className="flex flex-col gap-3">
            <div className="flex items-center justify-between">
              <h2 className="text-sm font-semibold text-fg">Service Health</h2>
              {status && status.foundry_endpoint && (
                // Foundry has no port; this is the literal "in-process (native SDK)" hint.
                <code className="text-xs text-fg-subtle font-mono bg-elevated px-2 py-0.5 rounded-sm">
                  {status.foundry_endpoint}
                </code>
              )}
            </div>
            <div className="grid gap-3 md:grid-cols-2 3xl:grid-cols-4">
              {status?.services.map((svc) => (
                <ServiceCard key={svc.name} service={svc} />
              ))}
            </div>
          </div>

          {/* ── Foundry ────────────────────────────────────────────────────────── */}
          <Card
            title="Foundry Local"
            actions={
              <Badge variant={status?.foundry_ready ? 'success' : 'error'} dot>
                {status?.foundry_ready ? 'ready' : 'down'}
              </Badge>
            }
          >
            <div className="flex flex-col gap-3">
              <p className="text-sm text-fg-muted">
                The chat + embedding core runs in-process (native SDK) — there is no
                service to start or stop. Re-register execution providers to re-run GPU/
                NPU/CPU discovery after a driver or hardware change.
              </p>

              {isAdmin && (
                <div>
                  <Button
                    variant="secondary"
                    leftIcon={<RefreshCw size={16} />}
                    loading={reregistering}
                    onClick={handleReregister}
                    className="min-h-[44px]"
                  >
                    Re-register execution providers
                  </Button>
                </div>
              )}

              {/* DocumentDB guidance — shown only when its health line reports an error. */}
              {docDbErrored && (
                <div className="flex flex-col gap-1.5 rounded-md border border-warning/20 bg-warning-subtle px-3 py-2.5">
                  <span className="text-sm text-warning font-medium">DocumentDB is not reachable.</span>
                  <span className="text-xs text-fg-muted">Start it, then Refresh:</span>
                  <code className="text-xs text-fg font-mono bg-elevated px-2 py-1 rounded-sm break-all">
                    docker compose up -d documentdb
                  </code>
                </div>
              )}
            </div>
          </Card>

          {/* ── Active Chat Model ──────────────────────────────────────────────── */}
          <Card
            title="Active Chat Model"
            actions={
              <Button
                size="sm"
                variant="ghost"
                leftIcon={<ExternalLink size={14} />}
                onClick={() => navigate('/models')}
                className="min-h-[44px]"
              >
                Manage Models
              </Button>
            }
          >
            <div className="flex flex-col gap-3">
              <InfoRow icon={<Server size={18} />} label="Current" value={activeModel} />

              {isAdmin ? (
                modelOptions.length > 0 ? (
                  <Select
                    label="Switch model"
                    hint={switching ? 'Applying — this can take a while while the model loads…' : 'Downloads if needed, loads, and sets as current.'}
                    value={status?.active_chat_model ?? ''}
                    disabled={switching}
                    onChange={(e) => handleSwitchModel(e.target.value)}
                  >
                    {/* Placeholder shown when nothing is currently active. */}
                    {!status?.active_chat_model && <option value="">— select a model —</option>}
                    {modelOptions.map((m) => (
                      <option key={m.id} value={m.id}>
                        {m.id}{m.loaded ? ' (loaded)' : ''}
                      </option>
                    ))}
                  </Select>
                ) : (
                  <p className="text-xs text-fg-subtle">
                    No cached models to switch to. Download one from Manage Models.
                  </p>
                )
              ) : (
                // Non-admins: read-only. The current value above is all they see.
                <p className="text-xs text-fg-subtle">Model selection is restricted to administrators.</p>
              )}
            </div>
          </Card>

          {/* ── Loaded Models ──────────────────────────────────────────────────── */}
          <Card
            title="Loaded Models"
            actions={
              isAdmin && status && status.loaded_models.length > 0 ? (
                <Button
                  size="sm"
                  variant="danger"
                  leftIcon={<HardDrive size={14} />}
                  loading={unloading}
                  onClick={handleUnloadAll}
                  className="min-h-[44px]"
                >
                  Unload all
                </Button>
              ) : undefined
            }
          >
            {status && status.loaded_models.length > 0 ? (
              <div className="flex flex-wrap gap-2">
                {status.loaded_models.map((id) => (
                  <Badge key={id} variant="info">{id}</Badge>
                ))}
              </div>
            ) : (
              <EmptyState
                icon={<Boxes size={28} />}
                title="No models loaded"
                description="Loaded models are held in memory for fast inference. Switch to a model above to load one."
              />
            )}
          </Card>

          {/* ── Activity ───────────────────────────────────────────────────────── */}
          <ServiceConsole />
        </>
      )}
    </div>
    </PageContainer>
  );
}
