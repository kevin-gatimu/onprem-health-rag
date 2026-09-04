import { useEffect, useState } from 'react';
import { Cpu, Sparkles } from 'lucide-react';
import type { ExecutionProvider, ModelRole, VariantInfo } from '../../lib/bridge';
import { Badge, Button, Card, Select } from '../../components/ui';
import { useModels } from '../../stores/models';

interface SharedLlmCardProps {
  role: ModelRole;
  isAdmin: boolean;
  /** Registered execution providers (from setup-status), for accurate dropdown labels. */
  executionProviders: ExecutionProvider[];
  onChanged: () => void;
}

interface ModelIdentity {
  family: string;
  weight: string;
}

function modelIdentity(alias: string): ModelIdentity {
  const qwen = alias.match(/^qwen3-(.+)$/i);
  if (qwen) return { family: 'Qwen3', weight: qwen[1].toUpperCase() };
  if (alias === 'gemma-4-e2b-it') return { family: 'Gemma 4 E2B IT', weight: 'E2B' };
  if (alias === 'mistral-nemo-12b-instruct') return { family: 'Mistral Nemo', weight: '12B' };
  if (alias === 'olmo-3-7b-instruct') return { family: 'Olmo 3', weight: '7B' };
  if (alias === 'phi-4-mini') return { family: 'Phi-4 Mini', weight: '3.8B' };
  const deepSeek = alias.match(/^deepseek-r1-(.+)$/i);
  if (deepSeek) return { family: 'DeepSeek R1', weight: deepSeek[1].toUpperCase() };
  return { family: alias, weight: alias };
}

function preferredVariant(role: ModelRole): VariantInfo | undefined {
  return role.variants.find((variant) => variant.id === role.override_variant)
    ?? role.variants.find((variant) => variant.alias === role.model)
    ?? role.variants[0];
}

function acceleratorLabel(variant: VariantInfo): string {
  if (variant.accelerator?.trim()) {
    return variant.accelerator === 'WebGPU' ? 'GPU (WebGPU)' : variant.accelerator;
  }

  const id = variant.id.toLowerCase();
  if (id.includes('cuda')) return 'NVIDIA CUDA';
  if (id.includes('tensorrt') || id.includes('rtx')) return 'NVIDIA RTX';
  if (id.includes('qnn')) return 'Qualcomm QNN';
  if (id.includes('vitis')) return 'AMD Vitis';
  if (id.includes('openvino-gpu')) return 'Intel OpenVINO (GPU)';
  if (id.includes('openvino-npu')) return 'Intel OpenVINO (NPU)';
  if (id.includes('generic-gpu')) return 'GPU (WebGPU)';
  return 'CPU';
}

/// Every accelerator class Foundry ships variants for. The local catalog only lists
/// device-compatible variants, so classes with no matching variant are rendered as
/// disabled entries rather than omitted.
const ALL_ACCELERATIONS = [
  'GPU (WebGPU)',
  'NVIDIA CUDA',
  'NVIDIA RTX',
  'Intel OpenVINO (GPU)',
  'Intel OpenVINO (NPU)',
  'Qualcomm QNN',
  'AMD Vitis',
  'CPU',
];

/// EP-name fragment that must be registered for each acceleration class to run here.
const ACCEL_EP_FRAGMENT: Record<string, string> = {
  'GPU (WebGPU)': 'webgpu',
  'NVIDIA CUDA': 'cuda',
  'NVIDIA RTX': 'tensorrt',
  'Intel OpenVINO (GPU)': 'openvino',
  'Intel OpenVINO (NPU)': 'openvino',
  'Qualcomm QNN': 'qnn',
  'AMD Vitis': 'vitis',
  CPU: 'cpu',
};

function deviceSupports(accel: string, eps: ExecutionProvider[]): boolean {
  if (accel === 'CPU') return true;
  const fragment = ACCEL_EP_FRAGMENT[accel];
  return eps.some((ep) => ep.registered && ep.name.toLowerCase().includes(fragment));
}

export default function SharedLlmCard({ role, isAdmin, executionProviders }: SharedLlmCardProps) {
  const initial = preferredVariant(role);
  const [selectedId, setSelectedId] = useState(initial?.id ?? '');

  useEffect(() => {
    if (!selectedId || !role.variants.some((variant) => variant.id === selectedId)) {
      setSelectedId(preferredVariant(role)?.id ?? '');
    }
  }, [role, selectedId]);

  const selected = role.variants.find((variant) => variant.id === selectedId) ?? initial;
  const identity = modelIdentity(selected?.alias ?? role.model);
  const families = Array.from(new Set(role.variants.map((variant) => modelIdentity(variant.alias).family)));
  const familyVariants = role.variants.filter(
    (variant) => modelIdentity(variant.alias).family === identity.family,
  );
  const aliases = Array.from(new Set(familyVariants.map((variant) => variant.alias)));
  const acceleratorVariants = familyVariants.filter((variant) => variant.alias === selected?.alias);
  const isActive = selected?.id === (role.override_variant ?? initial?.id);
  const isCompatible = selected?.supports_tool_calling ?? false;

  // Progress/apply state lives in the store — applyShared owns the whole lifecycle
  // (download → load → set-shared → toast → invalidate) so it survives this card
  // unmounting mid-apply. Only subscribe to this variant's own download entry.
  const download = useModels((s) => (selected ? s.downloads[selected.id] : undefined));
  const loadingId = useModels((s) => s.loadingId);
  const applyShared = useModels((s) => s.applyShared);
  const applying = download !== undefined || loadingId === selected?.id;

  function selectFirst(variants: VariantInfo[]) {
    if (variants[0]) setSelectedId(variants[0].id);
  }

  function applySelection() {
    if (!selected) return;
    // Fire-and-forget: the store finishes this even if the card unmounts.
    void applyShared(selected.id, selected.cached);
  }


  return (
    <Card
      title={<span className="inline-flex items-center gap-2"><Sparkles size={16} />Core LLM</span>}
      actions={<Badge variant={isActive ? 'success' : 'neutral'} dot>{isActive ? 'active' : 'configured'}</Badge>}
    >
      <div className="flex flex-col gap-4">
        <p className="text-sm text-fg-muted">
          One local model powers chat, classification, query rewrite, extraction, and SQL generation.
        </p>

        {role.variants.length > 0 ? (
          <>
            <div className="grid gap-3 md:grid-cols-3">
              <Select
                label="LLM name"
                value={identity.family}
                disabled={!isAdmin || applying}
                options={families.map((family) => ({ value: family, label: family }))}
                onChange={(event) => selectFirst(role.variants.filter(
                  (variant) => modelIdentity(variant.alias).family === event.target.value,
                ))}
              />
              <Select
                label="Weight"
                value={selected?.alias ?? ''}
                disabled={!isAdmin || applying}
                options={aliases.map((alias) => ({ value: alias, label: modelIdentity(alias).weight }))}
                onChange={(event) => selectFirst(familyVariants.filter(
                  (variant) => variant.alias === event.target.value,
                ))}
              />
              <Select
                label="Acceleration"
                value={selectedId}
                disabled={!isAdmin || applying}
                options={ALL_ACCELERATIONS.map((accel) => {
                  const variant = acceleratorVariants.find((v) => acceleratorLabel(v) === accel);
                  if (variant) return { value: variant.id, label: accel };
                  // No catalog build ≠ no hardware: only claim "not supported" when
                  // the EP really isn't registered on this device.
                  const reason = deviceSupports(accel, executionProviders)
                    ? 'no build for this model'
                    : 'not supported by this device';
                  return { value: `unsupported:${accel}`, label: `${accel} — ${reason}`, disabled: true };
                })}
                onChange={(event) => setSelectedId(event.target.value)}
              />
            </div>

            {/* Download progress (from the models store — survives navigation). */}
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

            <div className="flex flex-wrap items-center justify-between gap-3 border-t border-border pt-4">
              <div className="flex flex-col gap-1">
                <div className="flex flex-wrap items-center gap-2 text-xs text-fg-muted">
                  <Cpu size={15} />
                  <span>{selected?.id}</span>
                  {selected?.cached && <Badge variant="neutral">downloaded</Badge>}
                  {selected?.loaded && <Badge variant="info">loaded</Badge>}
                </div>
                {!isCompatible && (
                  <span className="text-xs text-warning">
                    This model doesn't report tool calling — structured analytics (counts, trends, SQL) may fall back to semantic retrieval.
                  </span>
                )}
              </div>
              {isAdmin && (
                <Button
                  onClick={applySelection}
                  loading={applying}
                  disabled={!selected || isActive}
                  className="min-h-11"
                >
                  {isActive ? 'Active' : selected?.cached ? 'Load & apply' : 'Download, load & apply'}
                </Button>
              )}
            </div>
          </>
        ) : (
          <p className="text-sm text-fg-muted">Model variants will appear when Foundry Local is ready.</p>
        )}
      </div>
    </Card>
  );
}