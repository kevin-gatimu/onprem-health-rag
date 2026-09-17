// RoleSection — one NON-managed (fastembed) role from the manifest (Stage 8, Layer 3b).
// Mobile-first: role meta stacks at 360 px. Dark theme only.
//
// One shared LLM now serves every managed (Foundry) role, so those are rendered by
// SharedLlmCard + the manage-variants grid in index.tsx instead. This component only
// ever receives `managed === false` roles (embeddings, reranker): a meta strip plus
// SpecializedModelControl, which downloads/loads the fastembed model in one step (no
// variant catalog, no router override).
import type { ModelRole } from '../../lib/bridge';
import { Badge, Card } from '../../components/ui';
import SpecializedModelControl from './SpecializedModelControl';

export interface RoleSectionProps {
  role: ModelRole;
  isAdmin: boolean;
  /** Bubbled to VariantCards so a mutation invalidates both queries in the parent. */
  onChanged: () => void;
}

// A compact label/value pair used in the role meta strip. Kept local (not a new
// primitive) — values wrap instead of overflowing at 360 px.
function Meta({ label, value }: { label: string; value: string }) {
  return (
    <div className="flex flex-col min-w-0">
      <span className="text-xs text-fg-subtle">{label}</span>
      <span className="text-sm text-fg break-words">{value}</span>
    </div>
  );
}

export default function RoleSection({ role, isAdmin, onChanged }: RoleSectionProps) {
  // "planned" roles aren't wired in this build; badge reflects that vs "active".
  const statusBadge = role.status === 'active'
    ? <Badge variant="success" dot>active</Badge>
    : <Badge variant="neutral">planned</Badge>;

  return (
    <Card title={role.label} actions={statusBadge}>
      <div className="flex flex-col gap-4">

        {/* ── Role meta: what this role is + where it runs ─────────────────── */}
        <p className="text-sm text-fg-muted break-words">{role.usage}</p>
        <div className="grid grid-cols-2 gap-3 md:grid-cols-4">
          <Meta label="Engine" value={role.engine} />
          <Meta label="Device" value={role.device} />
          <Meta label="Model" value={role.model} />
          <Meta
            label="Default"
            value={role.override_variant ?? '(auto — no override)'}
          />
        </div>

        {/* ── Read-only status + load control (non-managed / fastembed) ────── */}
        <SpecializedModelControl
          role={role}
          isAdmin={isAdmin}
          onChanged={onChanged}
        />
      </div>
    </Card>
  );
}

