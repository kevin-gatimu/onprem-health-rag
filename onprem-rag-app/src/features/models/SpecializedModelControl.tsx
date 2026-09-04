import { useState } from 'react';
import { CheckCircle2, Download } from 'lucide-react';
import type { ModelRole } from '../../lib/bridge';
import { loadSpecializedModel } from '../../lib/bridge';
import { Badge, Button } from '../../components/ui';
import { toast } from '../../stores/ui';

interface SpecializedModelControlProps {
  role: ModelRole;
  isAdmin: boolean;
  onChanged: () => void;
}

export default function SpecializedModelControl({
  role,
  isAdmin,
  onChanged,
}: SpecializedModelControlProps) {
  const [loading, setLoading] = useState(false);

  async function handleLoad() {
    setLoading(true);
    try {
      await loadSpecializedModel(role.role);
      toast.success(`${role.label} is ready.`);
      onChanged();
    } catch (err) {
      toast.error(String(err));
    } finally {
      setLoading(false);
    }
  }

  return (
    <div className="flex flex-col gap-3 border-t border-border pt-4 sm:flex-row sm:items-center sm:justify-between">
      <div className="flex min-w-0 items-center gap-3">
        <span className="flex h-9 w-9 shrink-0 items-center justify-center rounded-md bg-accent-subtle text-accent">
          <CheckCircle2 size={17} aria-hidden="true" />
        </span>
        <div className="min-w-0">
          <div className="flex flex-wrap items-center gap-2">
            <span className="text-sm font-semibold text-fg">{role.model}</span>
            <Badge variant={role.loaded ? 'success' : 'neutral'} dot>
              {role.loaded ? 'loaded' : 'not loaded'}
            </Badge>
          </div>
          <p className="text-xs text-fg-subtle">
            {role.loaded ? 'Ready in server memory.' : 'Downloads missing weights, sets up ONNX, and loads the model.'}
          </p>
        </div>
      </div>

      {isAdmin ? (
        <Button
          size="sm"
          variant={role.loaded ? 'secondary' : 'primary'}
          leftIcon={role.loaded ? <CheckCircle2 size={14} /> : <Download size={14} />}
          loading={loading}
          disabled={loading || role.loaded}
          onClick={handleLoad}
          className="min-h-11 w-full sm:w-auto"
        >
          {role.loaded ? 'Ready' : 'Download & load'}
        </Button>
      ) : (
        <span className="text-xs text-fg-subtle">Administrator setup required.</span>
      )}
    </div>
  );
}