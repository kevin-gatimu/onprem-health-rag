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
// WHY progress state is LOCAL: a download drives a per-card progress bar via the
// pullModel callbacks. Keeping it in this component (never a shared store) means
// one variant downloading doesn't spin every other card's UI.
import { useState } from 'react';
import { Cpu, Zap, Microchip, Copy, Check, Download, Play, Trash2, Star, StarOff } from 'lucide-react';
import {
  pullModel, selectModel, unloadModel, deleteModel, setRouter,
} from '../../lib/bridge';
import type { VariantInfo } from '../../lib/bridge';
import { toast } from '../../stores/ui';
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
  // Per-card, per-action busy flags + local download progress (see header note).
  const [downloading, setDownloading] = useState(false);
  const [progress, setProgress]       = useState(0);
  const [statusText, setStatusText]   = useState('');
  const [loading, setLoading]         = useState(false);
  const [unloading, setUnloading]     = useState(false);
  const [deleting, setDeleting]       = useState(false);
  const [confirmOpen, setConfirmOpen] = useState(false);
  const [settingDefault, setSettingDefault] = useState(false);
  const [copied, setCopied]           = useState(false);

  // Any in-flight mutation disables the others so the card can't fan out overlapping calls.
  const busy = downloading || loading || unloading || deleting || settingDefault;

  const dev = deviceBadge(variant.id);
  const DeviceIcon = dev.icon;

  function copyId() {
    navigator.clipboard.writeText(variant.id).then(() => {
      setCopied(true);
      setTimeout(() => setCopied(false), 2000);
    }).catch(() => { /* clipboard unavailable — silently ignore */ });
  }

  // ── Actions (admin only) ──────────────────────────────────────────────────────

  async function handleDownload() {
    setDownloading(true);
    setProgress(0);
    setStatusText('starting…');
    try {
      // load:false → pure Download (fetch weights, don't load into memory). Progress
      // and status stream back through the pullModel callbacks; done/error settle here.
      await pullModel(variant.id, false, {
        onProgress: (pct) => setProgress(pct),
        onStatus:   (s) => setStatusText(s),
        onError:    (e) => toast.error(e),
        onDone:     () => {
          toast.success(`Downloaded ${variant.id}.`);
          onChanged();
        },
      });
    } catch (err) {
      toast.error(String(err));
    } finally {
      setDownloading(false);
      setProgress(0);
      setStatusText('');
    }
  }

  async function handleLoad() {
    setLoading(true);
    try {
      // selectModel = download-if-needed → load → set current, so it can take a while.
      const resolved = await selectModel(variant.id);
      toast.success(`Loaded ${resolved}.`);
      onChanged();
    } catch (err) {
      toast.error(String(err));
    } finally {
      setLoading(false);
    }
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
        {variant.loaded && <Badge variant="info">loaded</Badge>}
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

      {/* ── Download progress (local, visible only mid-download) ────────────── */}
      {downloading && (
        <div className="flex flex-col gap-1">
          <div className="h-1.5 w-full overflow-hidden rounded-full bg-elevated">
            <div
              className="h-full bg-accent transition-all duration-200"
              style={{ width: `${Math.max(0, Math.min(100, progress))}%` }}
            />
          </div>
          <span className="text-xs text-fg-subtle">
            {statusText || 'downloading'} · {Math.round(progress)}%
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

          {variant.cached && !variant.loaded && (
            <Button
              size="sm"
              variant="secondary"
              leftIcon={<Play size={14} />}
              loading={loading}
              disabled={busy}
              onClick={handleLoad}
              className="min-h-[44px]"
            >
              Load
            </Button>
          )}

          {variant.loaded && (
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
