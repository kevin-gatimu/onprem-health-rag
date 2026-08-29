// RoleSection — one model role from the manifest (Stage 8, Layer 3b).
// Mobile-first: role meta stacks at 360 px, the variant grid is single-column and
// scales to md:2 / xl:3. Dark theme only.
//
// Catalog source is getModelRoles() — our REAL role→variant config — not a
// reinvented family catalog. Two render shapes:
//   managed === true  → a grid of VariantCards (downloadable/loadable Foundry variants)
//   managed === false → a single read-only status row (fastembed embeddings/reranker,
//                        which are not Foundry-managed and have no variant actions)
// The role's persisted `override_variant` is shown as its current default; each cached
// variant offers "Set as default", and the section header offers "Clear default" when
// an override is set (both admin-only, wired via setRouter in VariantCard).
import { Layers } from 'lucide-react';
import type { ModelRole } from '../../lib/bridge';
import { Badge, Card, EmptyState, cn } from '../../components/ui';
import VariantCard from './VariantCard';

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

        {/* ── Variants (managed) or read-only status (non-managed) ─────────── */}
        {role.managed ? (
          role.variants.length > 0 ? (
            <div className={cn('grid gap-3 md:grid-cols-2 xl:grid-cols-3 3xl:grid-cols-4')}>
              {role.variants.map((v) => (
                <VariantCard
                  key={v.id}
                  variant={v}
                  role={role.role}
                  isDefault={role.override_variant === v.id}
                  isAdmin={isAdmin}
                  onChanged={onChanged}
                />
              ))}
            </div>
          ) : (
            // Managed but empty — typically because the Foundry core is down (variants
            // that need `state.foundry()` come back empty). Keep the role visible.
            <EmptyState
              icon={<Layers size={26} />}
              title="No variants available"
              description="Variants load from the Foundry core. If it is unavailable, they'll appear once it's ready."
            />
          )
        ) : (
          // Non-managed roles (fastembed embeddings/reranker): read-only, no actions.
          <div className="rounded-lg border border-border bg-elevated px-4 py-3">
            <p className="text-xs text-fg-subtle">
              Served locally by <span className="text-fg-muted">{role.engine}</span> — not Foundry-managed,
              so there are no download/load actions.
            </p>
          </div>
        )}
      </div>
    </Card>
  );
}
