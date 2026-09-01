// VariantCard — one downloadable/loadable Foundry variant (Stage 8, Layer 3b).
// Mobile-first: a self-contained card that stacks its pills, id row, and a single
// state-appropriate action set at 360 px; tap targets ≥ 44 px. Dark theme only.
//
// State machine (mirrors plans/11 §3b): the visible action is derived from the
// variant's live flags carried on `VariantInfo` (getModelRoles already resolves
// cached/loaded/current), NOT from a separate lifecycle:
//   not cached          → Download (pull weights only, load:false, live progress)
//   cached && !loaded   → Load (select → download-if-needed → load → set current)
//   loaded              → Unload (free memory, keep weights) + Delete (drop weights)
// "Set as default" (per-role router override) is offered whenever the variant is
// cached. Every mutation is admin-only; non-admins get copy-to-clipboard only.
//
// WHY progress state is in the models STORE: a download outlives this card —
// navigating to another tab unmounts it, and the bar must reappear on return.
// Each card subscribes only to its own variant's entry, so one variant
// downloading doesn't re-render every other card.
import { useState } from 'react';
import { Cpu, Zap, Microchip, Copy, Check, Download, Play, Trash2, Star, StarOff } from 'lucide-react';
import { useQuery } from '@tanstack/react-query';
import {
  getSetupStatus, unloadModel, deleteModel, setRouter,
} from '../../lib/bridge';
import type { VariantInfo } from '../../lib/bridge';
import { toast } from '../../stores/ui';
import { useModels } from '../../stores/models';
import { Button, Badge, Modal } from '../../components/ui';

export interface VariantCardProps {
  variant: VariantInfo;
  /** The role this variant belongs to — target for the "Set as default" router override. */
  role: string;
  /** True when this variant is the role's persisted `override_variant`. */
  isDefault: boolean;
  isAdmin: boolean;
  /** Called after any successful mutation so the parent can invalidate both queries. */
  onChanged: () => void;
}

// Derive an accelerator badge from tokens in the (lowercased) variant id. Order
// matters: an explicit device token (npu/gpu/cpu) wins over the generic openvino
// runtime, so `…-openvino-gpu` reads as GPU while a bare `…-openvino` reads as the
// OpenVINO runtime (Intel GPU/NPU-accelerated → the Zap "accelerated" glyph).
function deviceBadge(id: string): { icon: typeof Cpu; label: string } {
  const s = id.toLowerCase();
  if (s.includes('npu')) return { icon: Microchip, label: 'NPU' };
  if (s.includes('gpu')) return { icon: Zap, label: 'GPU' };
  if (s.includes('cpu')) return { icon: Cpu, label: 'CPU' };
  if (s.includes('openvino')) return { icon: Zap, label: 'OpenVINO' };
  return { icon: Cpu, label: 'CPU' };
}

export default function VariantCard({ variant, role, isDefault, isAdmin, onChanged }: VariantCardProps) {
  // Download progress lives in the models store (survives navigation); this card
  // subscribes to its own variant's entry only. Other busy flags stay local.
  const download = useModels((s) => s.downloads[variant.id]);
  const startDownload = useModels((s) => s.startDownload);
  // Loads are single-flight (concurrent native loads segfault the server core) —
  // the store holds the ONE in-flight id so every card's Load button locks together.
  const loadingId = useModels((s) => s.loadingId);
  const startLoad = useModels((s) => s.startLoad);
  const downloading = download !== undefined;
  const loading = loadingId === variant.id;
  const [unloading, setUnloading]     = useState(false);
  const [deleting, setDeleting]       = useState(false);
  const [confirmOpen, setConfirmOpen] = useState(false);
  const [settingDefault, setSettingDefault] = useState(false);
  const [copied, setCopied]           = useState(false);

  // Live loaded flag: the manifest's `variant.loaded` snapshot can go stale between
  // refetches, so cross-check the shared setup-status poll (same cache as Settings).
  const { data: status } = useQuery({
    queryKey: ['setup-status'],
    queryFn: getSetupStatus,
    staleTime: 30_000,
  });
  const loaded = variant.loaded || (status?.loaded_models.includes(variant.id) ?? false);

  // Any in-flight mutation disables the others so the card can't fan out overlapping
  // calls; a load ANYWHERE locks this card's Load too (loadingId !== null).
  const busy = downloading || loadingId !== null || unloading || deleting || settingDefault;

  const dev = deviceBadge(variant.id);
  const DeviceIcon = dev.icon;

  function copyId() {
    navigator.clipboard.writeText(variant.id).then(() => {
      setCopied(true);
      setTimeout(() => setCopied(false), 2000);
    }).catch(() => { /* clipboard unavailable — silently ignore */ });
  }

  // ── Actions (admin only) ──────────────────────────────────────────────────────

  function handleDownload() {
    // Fire-and-forget: the store owns the whole lifecycle (progress events, the
    // completion toast, query invalidation), so it finishes even if this card
    // unmounts mid-download. `onChanged` is not needed — the store invalidates.
    void startDownload(variant.id);
  }

  async function handleLoad() {
    // Fire-and-forget: the store owns the lifecycle (single-flight guard, toast,
    // query invalidation), so it finishes even if this card unmounts mid-load.
    void startLoad(variant.id);
  }

  async function handleUnload() {
    setUnloading(true);
    try {
      await unloadModel(variant.id);
      toast.success(`Unloaded ${variant.id}.`);
      onChanged();
    } catch (err) {
      toast.error(String(err));
    } finally {
      setUnloading(false);
    }
  }

  async function handleDelete() {
    setDeleting(true);
    try {
      const result = await deleteModel(variant.id);
      // Deleting weights also clears any role default that named this variant —
      // surface which roles were reset so the admin isn't surprised by a router change.
      const cleared = result.cleared_roles.length > 0
        ? ` Cleared default for: ${result.cleared_roles.join(', ')}.`
        : '';
      toast.success(`Deleted ${variant.id}.${cleared}`);
      onChanged();
    } catch (err) {
      toast.error(String(err));
    } finally {
      setDeleting(false);
      setConfirmOpen(false);
    }
  }

  async function handleSetDefault(clear: boolean) {
    setSettingDefault(true);
    try {
      // clear → pass null to drop the override; otherwise pin this variant as the role default.
      await setRouter(role, clear ? null : variant.id);
      toast.success(clear ? `Cleared default for ${role}.` : `Set ${variant.id} as default for ${role}.`);
      onChanged();
    } catch (err) {
      toast.error(String(err));
    } finally {
      setSettingDefault(false);
    }
  }

  // ── Render ──────────────────────────────────────────────────────────────────

  return (
    <div className="flex flex-col gap-3 rounded-lg border border-border bg-surface p-4">

      {/* ── Header: device badge + state pills ─────────────────────────────── */}
      <div className="flex flex-wrap items-center gap-2">
        <span className="inline-flex items-center gap-1 rounded-md bg-accent-subtle px-2 py-0.5 text-xs font-medium text-accent">
          <DeviceIcon size={13} aria-hidden="true" />
          {dev.label}
        </span>
        {variant.current && <Badge variant="success" dot>current</Badge>}
        {loaded && <Badge variant="info">loaded</Badge>}
        {variant.cached && <Badge variant="neutral">cached</Badge>}
        {isDefault && <Badge variant="warning">default</Badge>}
      </div>

      {/* ── Model id + copy ─────────────────────────────────────────────────── */}
      <div className="flex items-start gap-2">
        <code className="min-w-0 flex-1 break-all font-mono text-xs text-fg-muted">{variant.id}</code>
        <button
          onClick={copyId}
          className="flex-shrink-0 flex items-center justify-center w-8 h-8 rounded-md text-fg-subtle hover:bg-elevated hover:text-fg transition-colors"
          aria-label={copied ? 'Copied' : 'Copy model id'}
        >
          {copied ? <Check size={14} aria-hidden="true" /> : <Copy size={14} aria-hidden="true" />}
        </button>
      </div>

      {/* Context length — only when the manifest reports it. */}
      {variant.context_length != null && (
        <span className="text-xs text-fg-subtle">
          Context: {variant.context_length.toLocaleString()} tokens
        </span>
      )}

      {/* ── Download progress (from the models store — survives navigation) ─── */}
      {download && (
        <div className="flex flex-col gap-1">
          <div className="h-1.5 w-full overflow-hidden rounded-full bg-elevated">
            <div
              className="h-full bg-accent transition-all duration-200"
              style={{ width: `${Math.max(0, Math.min(100, download.pct))}%` }}
            />
          </div>
          <span className="text-xs text-fg-subtle">
            {download.status || 'downloading'} · {Math.round(download.pct)}%
          </span>
        </div>
      )}

      {/* ── Actions ─────────────────────────────────────────────────────────── */}
      {isAdmin ? (
        <div className="flex flex-wrap gap-2">
          {!variant.cached && (
            <Button
              size="sm"
              variant="primary"
              leftIcon={<Download size={14} />}
              loading={downloading}
              disabled={busy}
              onClick={handleDownload}
              className="min-h-[44px]"
            >
              Download
            </Button>
          )}

          {variant.cached && (
            <Button
              size="sm"
              variant="secondary"
              leftIcon={<Play size={14} />}
              loading={loading}
              disabled={busy || loaded}
              onClick={handleLoad}
              className="min-h-[44px]"
            >
              {loaded ? 'Loaded' : 'Load'}
            </Button>
          )}

          {loaded && (
            <>
              <Button
                size="sm"
                variant="secondary"
                loading={unloading}
                disabled={busy}
                onClick={handleUnload}
                className="min-h-[44px]"
              >
                Unload
              </Button>
              <Button
                size="sm"
                variant="danger"
                leftIcon={<Trash2 size={14} />}
                loading={deleting}
                disabled={busy}
                onClick={() => setConfirmOpen(true)}
                className="min-h-[44px]"
              >
                Delete
              </Button>
            </>
          )}

          {/* Set-as-default is available whenever the variant is cached. */}
          {variant.cached && (
            isDefault ? (
              <Button
                size="sm"
                variant="ghost"
                leftIcon={<StarOff size={14} />}
                loading={settingDefault}
                disabled={busy}
                onClick={() => handleSetDefault(true)}
                className="min-h-[44px]"
              >
                Clear default
              </Button>
            ) : (
              <Button
                size="sm"
                variant="ghost"
                leftIcon={<Star size={14} />}
                loading={settingDefault}
                disabled={busy}
                onClick={() => handleSetDefault(false)}
                className="min-h-[44px]"
              >
                Set as default
              </Button>
            )
          )}
        </div>
      ) : (
        // Non-admins: no mutating controls — the copy button above is all they get.
        <span className="text-xs text-fg-subtle">Model management is restricted to administrators.</span>
      )}

      {/* ── Delete confirm ──────────────────────────────────────────────────── */}
      <Modal
        open={confirmOpen}
        onClose={() => (deleting ? undefined : setConfirmOpen(false))}
        title="Delete model weights?"
        size="sm"
        footer={
          <>
            <Button variant="ghost" onClick={() => setConfirmOpen(false)} disabled={deleting} className="min-h-[44px]">
              Cancel
            </Button>
            <Button variant="danger" loading={deleting} onClick={handleDelete} className="min-h-[44px]">
              Delete
            </Button>
          </>
        }
      >
        <p className="text-sm text-fg-muted break-words">
          This permanently deletes the weights for{' '}
          <code className="font-mono text-fg">{variant.id}</code> from the server's model
          cache. Any role default naming it will be cleared. You can re-download later.
        </p>
      </Modal>
    </div>
  );
}
