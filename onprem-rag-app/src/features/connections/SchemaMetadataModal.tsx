import { useEffect, useState } from 'react';
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query';
import { DatabaseZap, Plus, RefreshCw, Trash2 } from 'lucide-react';
import { Badge, Button, Input, Modal } from '../../components/ui';
import {
  getSchemaCatalog,
  getSchemaCatalogHistory,
  EMPTY_METADATA_OVERRIDES,
  getSchemaMetadataOverrides,
  refreshSchemaCatalog,
  saveSchemaMetadataOverrides,
} from '../../lib/bridge';
import type { MetadataOverrides, SourceInfo } from '../../lib/bridge';
import { toast } from '../../stores/ui';

interface SchemaMetadataModalProps {
  source: SourceInfo | null;
  onClose: () => void;
}

function formatCapturedAt(value: string | null | undefined): string {
  if (!value) return 'Not available';
  const normalized = value.replace(
    /^(\d{4}-\d{2}-\d{2}) (\d{2}:\d{2}:\d{2}(?:\.\d+)?) \+00:00:00$/,
    '$1T$2Z',
  );
  const date = new Date(normalized);
  return Number.isNaN(date.getTime()) ? value : date.toLocaleString();
}

export default function SchemaMetadataModal({ source, onClose }: SchemaMetadataModalProps) {
  const queryClient = useQueryClient();
  const queryKey = ['schema-catalog', source?.id];
  const catalogQuery = useQuery({
    queryKey,
    queryFn: () => getSchemaCatalog(source!.id),
    enabled: source !== null,
    staleTime: 30_000,
    retry: false,
  });
  const historyQuery = useQuery({
    queryKey: ['schema-catalog-history', source?.id],
    queryFn: () => getSchemaCatalogHistory(source!.id),
    enabled: source !== null,
    retry: false,
  });
  const overridesQuery = useQuery({
    queryKey: ['schema-metadata-overrides', source?.id],
    queryFn: () => getSchemaMetadataOverrides(source!.id),
    enabled: source !== null && catalogQuery.isSuccess,
    retry: false,
  });
  // Seeded from EMPTY_METADATA_OVERRIDES, not a two-field literal: this modal
  // PUTs the whole document back, and the server replaces it wholesale, so any
  // field missing here would erase the admin's concept / column-role /
  // service-line overrides set on the Settings Data Binding page.
  const [overrides, setOverrides] = useState<MetadataOverrides>(EMPTY_METADATA_OVERRIDES);
  useEffect(() => {
    if (overridesQuery.data) setOverrides(overridesQuery.data);
  }, [overridesQuery.data]);

  const refreshMutation = useMutation({
    mutationFn: () => refreshSchemaCatalog(source!.id),
    onSuccess: async (result) => {
      await Promise.all([
        queryClient.invalidateQueries({ queryKey }),
        queryClient.invalidateQueries({ queryKey: ['schema-catalog-history', source?.id] }),
      ]);
      toast.success(`Schema metadata refreshed for ${result.tables_indexed} tables.`);
    },
    onError: (error) => toast.error(`Metadata refresh failed: ${String(error)}`),
  });

  const saveOverridesMutation = useMutation({
    mutationFn: () => saveSchemaMetadataOverrides(source!.id, overrides),
    onSuccess: (saved) => {
      setOverrides(saved);
      queryClient.setQueryData(['schema-metadata-overrides', source?.id], saved);
      toast.success('Metadata overrides saved.');
    },
    onError: (error) => toast.error(`Could not save overrides: ${String(error)}`),
  });

  const catalog = catalogQuery.data;

  return (
    <Modal
      open={source !== null}
      onClose={onClose}
      title={`Schema metadata${source ? ` — ${source.name}` : ''}`}
      size="lg"
      footer={
        <>
          <Button variant="ghost" onClick={onClose}>Close</Button>
          <Button
            variant="primary"
            leftIcon={<RefreshCw size={15} />}
            loading={refreshMutation.isPending}
            disabled={!source || catalogQuery.isFetching}
            onClick={() => refreshMutation.mutate()}
          >
            Refresh metadata
          </Button>
        </>
      }
    >
      <div className="flex flex-col gap-5">
        <div className="flex items-start gap-3 rounded-lg border border-border bg-elevated p-4">
          <span className="flex h-9 w-9 shrink-0 items-center justify-center rounded-md bg-accent-subtle text-accent">
            <DatabaseZap size={18} />
          </span>
          <div>
            <p className="text-sm font-medium text-fg">Used by live SQL chat</p>
            <p className="mt-0.5 text-xs leading-relaxed text-fg-muted">
              This catalog lets the router understand tables, columns, and relationships without rediscovering the database schema for every question.
            </p>
          </div>
        </div>

        {catalogQuery.isLoading ? (
          <p className="text-sm text-fg-muted">Loading schema metadata…</p>
        ) : catalogQuery.isError ? (
          <div className="rounded-lg border border-warning/30 bg-warning-subtle p-4">
            <p className="text-sm font-medium text-warning">No active metadata catalog found</p>
            <p className="mt-1 text-xs text-fg-muted">
              Refresh metadata to inspect this source and create its first active catalog.
            </p>
          </div>
        ) : catalog ? (
          <div className="grid gap-3 sm:grid-cols-2">
            <div className="rounded-lg border border-border p-3">
              <p className="text-xs text-fg-subtle">Status</p>
              <div className="mt-1.5">
                <Badge variant={catalog.status === 'active' ? 'success' : 'warning'} dot>
                  {catalog.status}
                </Badge>
              </div>
            </div>
            <div className="rounded-lg border border-border p-3">
              <p className="text-xs text-fg-subtle">Health</p>
              <div className="mt-1.5">
                <Badge variant={catalog.health === 'healthy' ? 'success' : 'warning'} dot>
                  {catalog.health}
                </Badge>
              </div>
              {catalog.last_error && <p className="mt-2 text-xs text-danger">{catalog.last_error}</p>}
            </div>
            <div className="rounded-lg border border-border p-3">
              <p className="text-xs text-fg-subtle">Tables indexed</p>
              <p className="mt-1 text-lg font-semibold text-fg">{catalog.table_count}</p>
            </div>
            <div className="rounded-lg border border-border p-3">
              <p className="text-xs text-fg-subtle">Last background check</p>
              <p className="mt-1 text-sm text-fg">{formatCapturedAt(catalog.last_check_at)}</p>
            </div>
            <div className="rounded-lg border border-border p-3 sm:col-span-2">
              <p className="text-xs text-fg-subtle">Last refreshed</p>
              <p className="mt-1 text-sm text-fg">{formatCapturedAt(catalog.captured_at)}</p>
            </div>
            <div className="rounded-lg border border-border p-3 sm:col-span-2">
              <p className="text-xs text-fg-subtle">Catalog version</p>
              <p className="mt-1 break-all font-mono text-xs text-fg-muted">{catalog.active_version}</p>
            </div>
            <div className="rounded-lg border border-border p-3 sm:col-span-2">
              <p className="text-xs text-fg-subtle">Schema fingerprint</p>
              <p className="mt-1 break-all font-mono text-xs text-fg-muted">{catalog.schema_hash}</p>
            </div>
          </div>
        ) : null}

        {catalog && (
          <section>
            <h3 className="mb-2 text-sm font-semibold text-fg">Recent metadata activity</h3>
            <div className="max-h-40 overflow-auto rounded-lg border border-border">
              {historyQuery.data?.length ? historyQuery.data.map((item, index) => (
                <div key={`${item.checked_at}-${index}`} className="flex items-start justify-between gap-3 border-b border-border px-3 py-2 last:border-b-0">
                  <div>
                    <p className="text-xs font-medium text-fg">{item.trigger} · {item.outcome}</p>
                    <p className="text-xs text-fg-subtle">{formatCapturedAt(item.checked_at)}</p>
                    {item.error && <p className="mt-1 text-xs text-danger">{item.error}</p>}
                  </div>
                  <span className="text-xs text-fg-muted">{item.table_count} tables</span>
                </div>
              )) : <p className="p-3 text-xs text-fg-muted">No refresh checks recorded yet.</p>}
            </div>
          </section>
        )}

        {catalog && (
          <section className="space-y-4">
            <div>
              <div className="mb-2 flex items-center justify-between">
                <div>
                  <h3 className="text-sm font-semibold text-fg">Business aliases</h3>
                  <p className="text-xs text-fg-muted">Map familiar terms to exact schema names.</p>
                </div>
                <Button variant="ghost" size="sm" leftIcon={<Plus size={14} />} onClick={() => setOverrides((value) => ({
                  ...value, aliases: [...value.aliases, { table: '', column: null, alias: '' }],
                }))}>Add alias</Button>
              </div>
              <div className="space-y-2">
                {overrides.aliases.map((alias, index) => (
                  <div key={index} className="grid gap-2 sm:grid-cols-[1fr_1fr_1fr_auto]">
                    <Input placeholder="Table" value={alias.table} onChange={(event) => setOverrides((value) => ({ ...value, aliases: value.aliases.map((item, i) => i === index ? { ...item, table: event.target.value } : item) }))} />
                    <Input placeholder="Column (optional)" value={alias.column ?? ''} onChange={(event) => setOverrides((value) => ({ ...value, aliases: value.aliases.map((item, i) => i === index ? { ...item, column: event.target.value || null } : item) }))} />
                    <Input placeholder="Business term" value={alias.alias} onChange={(event) => setOverrides((value) => ({ ...value, aliases: value.aliases.map((item, i) => i === index ? { ...item, alias: event.target.value } : item) }))} />
                    <Button variant="ghost" size="sm" aria-label="Remove alias" onClick={() => setOverrides((value) => ({ ...value, aliases: value.aliases.filter((_, i) => i !== index) }))}><Trash2 size={14} /></Button>
                  </div>
                ))}
              </div>
            </div>

            <div>
              <div className="mb-2 flex items-center justify-between">
                <div>
                  <h3 className="text-sm font-semibold text-fg">Relationship overrides</h3>
                  <p className="text-xs text-fg-muted">Add joins missing from database constraints.</p>
                </div>
                <Button variant="ghost" size="sm" leftIcon={<Plus size={14} />} onClick={() => setOverrides((value) => ({
                  ...value, relationships: [...value.relationships, { from_table: '', from_column: '', to_table: '', to_column: '' }],
                }))}>Add relationship</Button>
              </div>
              <div className="space-y-2">
                {overrides.relationships.map((edge, index) => (
                  <div key={index} className="grid gap-2 sm:grid-cols-[1fr_1fr_1fr_1fr_auto]">
                    {(['from_table', 'from_column', 'to_table', 'to_column'] as const).map((field) => (
                      <Input key={field} placeholder={field.replace('_', ' ')} value={edge[field]} onChange={(event) => setOverrides((value) => ({ ...value, relationships: value.relationships.map((item, i) => i === index ? { ...item, [field]: event.target.value } : item) }))} />
                    ))}
                    <Button variant="ghost" size="sm" aria-label="Remove relationship" onClick={() => setOverrides((value) => ({ ...value, relationships: value.relationships.filter((_, i) => i !== index) }))}><Trash2 size={14} /></Button>
                  </div>
                ))}
              </div>
            </div>
            <div className="flex justify-end">
              <Button variant="secondary" loading={saveOverridesMutation.isPending} disabled={overridesQuery.isLoading} onClick={() => saveOverridesMutation.mutate()}>
                Save aliases and relationships
              </Button>
            </div>
          </section>
        )}
      </div>
    </Modal>
  );
}
