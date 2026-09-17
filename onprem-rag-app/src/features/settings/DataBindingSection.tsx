// Data Binding (admin) — plan 07 §4 "src/features/settings".
//
// Shows, per source: the schema binding's coverage, which of the hospital service
// lines it makes usable, an orphan-table warning, and an editable table list whose
// concept / service-line / column-role overrides are saved back to the server.
//
// Registry-driven, per plan 07 §8: every service-line slug and label comes from
// `useAgentRegistry` (i.e. from `GET /agents`), and the concept vocabulary is the
// union of the registry's own `concepts` plus whatever the binding already used.
// Nothing in this file spells a service line or a concept out.
//
// Column roles are the one vocabulary the server does not publish anywhere — see
// the comment on the role input. It is a free-text field rather than a select
// built from a hardcoded list, because a hardcoded list here would be an
// unverifiable mirror that drifts silently.
import { useEffect, useMemo, useState } from 'react';
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query';
import {
  AlertTriangle, ChevronDown, ChevronRight, Database, RefreshCw, Trash2,
} from 'lucide-react';
import {
  EMPTY_METADATA_OVERRIDES,
  getCatalogOverrides,
  getSchema,
  getSourceBinding,
  listSources,
  rebuildSourceBinding,
  saveCatalogOverrides,
} from '../../lib/bridge';
import type { MetadataOverrides, SourceBinding } from '../../lib/bridge';
import { useAgentRegistry, ASK_KIND } from '../../stores/agentRegistry';
import { toast } from '../../stores/ui';
import { Badge, Button, Card, Input, Select } from '../../components/ui';

/** Select value meaning "exclude this table from every service line". */
const IGNORE_CONCEPT = '__ignore__';
/** Select value meaning "leave the binder's own choice in place". */
const AUTO_CONCEPT = '__auto__';

export default function DataBindingSection() {
  const queryClient = useQueryClient();
  const registry = useAgentRegistry();
  const [sourceId, setSourceId] = useState<string>('');
  const [expanded, setExpanded] = useState<string | null>(null);
  const [overrides, setOverrides] = useState<MetadataOverrides>(EMPTY_METADATA_OVERRIDES);

  const sourcesQuery = useQuery({ queryKey: ['sources'], queryFn: listSources, staleTime: 60_000 });
  const sources = useMemo(() => sourcesQuery.data ?? [], [sourcesQuery.data]);

  // Default to the first source once the list arrives.
  useEffect(() => {
    if (!sourceId && sources.length > 0) setSourceId(sources[0].id);
  }, [sourceId, sources]);

  // A source with no binding yet returns 404 — an expected state, not a transient
  // failure, so this query does not retry and the empty case is rendered instead.
  const bindingQuery = useQuery<SourceBinding>({
    queryKey: ['source-binding', sourceId],
    queryFn: () => getSourceBinding(sourceId),
    enabled: sourceId !== '',
    retry: false,
    staleTime: 30_000,
  });

  const overridesQuery = useQuery({
    queryKey: ['catalog-overrides', sourceId],
    queryFn: () => getCatalogOverrides(sourceId),
    enabled: sourceId !== '',
    retry: false,
    staleTime: 30_000,
  });

  useEffect(() => {
    setOverrides(overridesQuery.data ?? EMPTY_METADATA_OVERRIDES);
  }, [overridesQuery.data]);

  // Columns for the expanded table's per-column role editor. Fetched lazily — the
  // binding response carries table names but not their columns.
  const schemaQuery = useQuery({
    queryKey: ['schema', sourceId],
    queryFn: () => getSchema(sourceId),
    enabled: sourceId !== '' && expanded !== null,
    staleTime: 5 * 60_000,
  });

  const rebuildMutation = useMutation({
    mutationFn: () => rebuildSourceBinding(sourceId),
    onSuccess: async (result) => {
      const plural = result.orphans === 1 ? '' : 's';
      toast.success(
        `Binding rebuilt — ${result.bound}/${result.tables} tables bound, ${result.orphans} orphan${plural}.`,
      );
      await queryClient.invalidateQueries({ queryKey: ['source-binding', sourceId] });
      // The roster's usable flags and table scopes are derived from this binding,
      // so the agent tabs must be refetched or they keep the pre-rebuild scope.
      await registry.load(true);
    },
    onError: (error) => toast.error(`Rebuild failed: ${String(error)}`),
  });

  const saveMutation = useMutation({
    mutationFn: () => saveCatalogOverrides(sourceId, overrides),
    onSuccess: async (saved) => {
      setOverrides(saved);
      toast.success('Overrides saved. Rebuild the binding to apply them.');
      await queryClient.invalidateQueries({ queryKey: ['catalog-overrides', sourceId] });
    },
    onError: (error) => toast.error(`Save failed: ${String(error)}`),
  });

  const binding = bindingQuery.data;

  // Service lines the UI can offer — the roster minus the synthetic Ask tab, which
  // is a router affordance rather than a service line.
  const serviceLines = useMemo(
    () => registry.agents.filter((agent) => agent.kind !== ASK_KIND),
    [registry.agents],
  );

  // Concept vocabulary: what the lines declare, plus whatever the binder already
  // assigned, so re-selecting an existing concept is always possible.
  const conceptOptions = useMemo(() => {
    const set = new Set<string>();
    for (const line of serviceLines) for (const concept of line.concepts) set.add(concept);
    for (const table of binding?.tables ?? []) {
      if (table.concept && table.concept !== 'unknown') set.add(table.concept);
    }
    return [...set].sort();
  }, [serviceLines, binding]);

  const columnsByTable = useMemo(() => {
    const map = new Map<string, string[]>();
    for (const table of schemaQuery.data ?? []) {
      map.set(table.name, table.columns.map((column) => column.name));
    }
    return map;
  }, [schemaQuery.data]);

  // ── Override mutators ───────────────────────────────────────────────────────
  function setTableConcept(table: string, value: string) {
    setOverrides((current) => {
      const rest = current.table_concepts.filter((entry) => entry.table !== table);
      if (value === AUTO_CONCEPT) return { ...current, table_concepts: rest };
      const concept = value === IGNORE_CONCEPT ? null : value;
      return { ...current, table_concepts: [...rest, { table, concept }] };
    });
  }

  function toggleServiceLine(slug: string) {
    setOverrides((current) => ({
      ...current,
      service_lines: current.service_lines.includes(slug)
        ? current.service_lines.filter((entry) => entry !== slug)
        : [...current.service_lines, slug],
    }));
  }

  function setColumnRole(table: string, column: string, role: string) {
    setOverrides((current) => {
      const rest = current.column_roles.filter(
        (entry) => !(entry.table === table && entry.column === column),
      );
      return role.trim() === ''
        ? { ...current, column_roles: rest }
        : { ...current, column_roles: [...rest, { table, column, role: role.trim() }] };
    });
  }

  /** The concept that WILL apply — the override if one exists, else automatic. */
  function conceptValue(table: string): string {
    const override = overrides.table_concepts.find((entry) => entry.table === table);
    if (!override) return AUTO_CONCEPT;
    return override.concept === null ? IGNORE_CONCEPT : override.concept;
  }

  function roleValue(table: string, column: string): string {
    const entry = overrides.column_roles.find(
      (item) => item.table === table && item.column === column,
    );
    return entry?.role ?? '';
  }

  // ── Render ──────────────────────────────────────────────────────────────────
  if (sources.length === 0) {
    return (
      <Card title="Data Binding">
        <p className="text-sm text-fg-muted">
          No data sources are connected yet. Add one on the Connections page, refresh its
          schema catalog, then return here to bind it to the hospital service lines.
        </p>
      </Card>
    );
  }

  const coverage = binding?.coverage;
  const usableSet = new Set(binding?.usable_lines ?? []);

  return (
    <Card
      title="Data Binding"
      actions={
        <Button
          variant="secondary"
          size="sm"
          leftIcon={
            <RefreshCw
              size={14}
              className={rebuildMutation.isPending ? 'animate-spin' : undefined}
            />
          }
          loading={rebuildMutation.isPending}
          disabled={sourceId === ''}
          onClick={() => rebuildMutation.mutate()}
          className="min-h-[44px]"
        >
          Rebuild binding
        </Button>
      }
    >
      <div className="flex flex-col gap-4">
        <p className="text-sm text-fg-muted">
          The schema binder maps each table in a source to a clinical concept, then decides
          which hospital service lines that source can answer for. Everything here runs
          on-premises against the connected database catalog.
        </p>

        <Select
          label="Source"
          value={sourceId}
          onChange={(event) => {
            setSourceId(event.target.value);
            setExpanded(null);
          }}
          options={sources.map((source) => ({ value: source.id, label: source.name }))}
        />

        {bindingQuery.isLoading && <p className="text-sm text-fg-muted">Loading binding…</p>}

        {bindingQuery.isError && (
          <div className="flex items-start gap-2 rounded-md border border-border bg-elevated px-3 py-2.5">
            <Database size={15} className="text-fg-subtle mt-0.5 shrink-0" aria-hidden="true" />
            <div className="text-xs text-fg-muted">
              <p className="font-medium text-fg">No binding for this source yet.</p>
              <p className="mt-0.5">
                Refresh the schema catalog for this source on the Connections page, then
                choose Rebuild binding above.
              </p>
            </div>
          </div>
        )}

        {binding && coverage && (
          <>
            {/* Orphans warning — tables in no service line's scope. The server
                names them in `orphans`; older servers only give the count, so the
                banner degrades to the counter rather than disappearing. */}
            {coverage.orphan_tables > 0 && (
              <div
                className="flex items-start gap-2 rounded-md border border-warning/20 bg-warning-subtle px-3 py-2.5"
                role="status"
              >
                <AlertTriangle
                  size={15}
                  className="text-warning mt-0.5 shrink-0"
                  aria-hidden="true"
                />
                <div className="text-xs text-fg-muted">
                  <p className="text-warning font-medium">
                    {coverage.orphan_tables} table
                    {coverage.orphan_tables === 1 ? '' : 's'} bound to no service line.
                  </p>
                  {binding.orphans && binding.orphans.length > 0 && (
                    <p className="mt-0.5 font-mono text-fg-subtle break-words">
                      {binding.orphans.join(', ')}
                    </p>
                  )}
                  <p className="mt-0.5">
                    Set a concept on each orphan below, or mark it ignored, then rebuild.
                  </p>
                </div>
              </div>
            )}

            {binding.degraded && (
              <p className="text-xs text-warning">
                This binding is degraded — it was produced without descriptor vectors, so
                concept confidence is lower than usual.
              </p>
            )}

            <div className="grid grid-cols-2 md:grid-cols-4 gap-2 text-xs">
              <Counter label="Tables" value={coverage.total_tables} />
              <Counter label="Bound" value={coverage.bound_tables} />
              <Counter label="High confidence" value={coverage.exact_concepts} />
              <Counter label="Orphans" value={coverage.orphan_tables} />
            </div>

            {/* Per-service-line coverage. */}
            <div>
              <p className="text-xs font-semibold text-fg-muted uppercase tracking-wide mb-1.5">
                Service line coverage
              </p>
              <div className="overflow-x-auto">
                <table className="min-w-full text-xs">
                  <thead className="text-fg-muted">
                    <tr>
                      <th className="text-left font-medium py-1.5 pr-3">Line</th>
                      <th className="text-left font-medium py-1.5 pr-3">Usable</th>
                      <th className="text-left font-medium py-1.5">Tables in scope</th>
                    </tr>
                  </thead>
                  <tbody>
                    {serviceLines.map((line) => {
                      const tables = binding.tables.filter((table) =>
                        table.service_lines.includes(line.kind),
                      );
                      const usable = usableSet.has(line.kind);
                      return (
                        <tr key={line.kind} className="border-t border-border">
                          <td className="py-1.5 pr-3 text-fg">{line.label}</td>
                          <td className="py-1.5 pr-3">
                            <Badge variant={usable ? 'success' : 'neutral'}>
                              {usable ? 'yes' : 'no'}
                            </Badge>
                          </td>
                          <td className="py-1.5 text-fg-muted font-mono">
                            {tables.length > 0
                              ? tables.map((table) => table.table_name).join(', ')
                              : '—'}
                          </td>
                        </tr>
                      );
                    })}
                  </tbody>
                </table>
              </div>
            </div>

            {/* Forced service lines. Empty = the server's automatic detection. */}
            <div>
              <p className="text-xs font-semibold text-fg-muted uppercase tracking-wide mb-1.5">
                Force service lines on
              </p>
              <p className="text-xs text-fg-subtle mb-1.5">
                Leave all unselected to use automatic detection.
              </p>
              <div className="flex flex-wrap gap-1.5">
                {serviceLines.map((line) => {
                  const on = overrides.service_lines.includes(line.kind);
                  return (
                    <button
                      key={line.kind}
                      onClick={() => toggleServiceLine(line.kind)}
                      aria-pressed={on}
                      className={[
                        'px-2.5 py-1.5 rounded-full border text-xs transition-colors min-h-[36px]',
                        on
                          ? 'border-accent bg-accent-subtle text-fg'
                          : 'border-border text-fg-muted hover:text-fg hover:bg-elevated',
                      ].join(' ')}
                    >
                      {line.label}
                    </button>
                  );
                })}
              </div>
            </div>

            {/* Table list with inline overrides. */}
            <div>
              <p className="text-xs font-semibold text-fg-muted uppercase tracking-wide mb-1.5">
                Tables
              </p>
              <div className="flex flex-col divide-y divide-border border border-border rounded-md">
                {binding.tables.map((table) => {
                  const open = expanded === table.table_name;
                  const orphan = table.service_lines.length === 0;
                  return (
                    <div key={table.table_name}>
                      <button
                        onClick={() => setExpanded(open ? null : table.table_name)}
                        aria-expanded={open}
                        className="w-full flex items-center gap-2 px-3 py-2 text-left hover:bg-elevated transition-colors min-h-[44px]"
                      >
                        {open ? (
                          <ChevronDown size={13} aria-hidden="true" />
                        ) : (
                          <ChevronRight size={13} aria-hidden="true" />
                        )}
                        <span className="font-mono text-xs text-fg flex-1 truncate">
                          {table.table_name}
                        </span>
                        <span className="text-xs text-fg-muted font-mono">{table.concept}</span>
                        <span className="text-xs text-fg-subtle tabular-nums">
                          {Math.round(table.confidence * 100)}%
                        </span>
                        {orphan && <Badge variant="neutral">orphan</Badge>}
                      </button>

                      {open && (
                        <div className="px-3 pb-3 pt-1 flex flex-col gap-3">
                          <Select
                            label="Concept"
                            value={conceptValue(table.table_name)}
                            onChange={(event) =>
                              setTableConcept(table.table_name, event.target.value)
                            }
                            options={[
                              { value: AUTO_CONCEPT, label: `Automatic (${table.concept})` },
                              { value: IGNORE_CONCEPT, label: 'Ignore this table' },
                              ...conceptOptions.map((concept) => ({
                                value: concept,
                                label: concept,
                              })),
                            ]}
                          />

                          <div className="flex flex-wrap items-center gap-1.5 text-xs text-fg-muted">
                            <span>Service lines:</span>
                            {table.service_lines.length > 0 ? (
                              table.service_lines.map((slug) => (
                                <span key={slug} className="px-1.5 py-0.5 rounded bg-elevated">
                                  {registry.labelFor(slug) ?? slug}
                                </span>
                              ))
                            ) : (
                              <span className="text-fg-subtle">none</span>
                            )}
                          </div>

                          {table.event_time_col && (
                            <p className="text-xs text-fg-subtle">
                              Event time column:{' '}
                              <span className="font-mono text-fg-muted">
                                {table.event_time_col}
                              </span>
                            </p>
                          )}

                          {/* Per-column role overrides. Free text on purpose: the
                              server publishes no role vocabulary anywhere, and it
                              validates the slug (`ColumnRole::from_slug` in
                              onprem-rag-server/src/nl2sql/http.rs), returning a
                              readable 400 for an unknown one. A hardcoded list
                              here would be an unverifiable mirror that drifts. */}
                          <div>
                            <p className="text-xs font-medium text-fg mb-1">Column roles</p>
                            {schemaQuery.isLoading && (
                              <p className="text-xs text-fg-subtle">Loading columns…</p>
                            )}
                            {(columnsByTable.get(table.table_name) ?? []).map((column) => (
                              <div key={column} className="flex items-center gap-2 py-0.5">
                                <span className="font-mono text-xs text-fg-muted w-40 truncate shrink-0">
                                  {column}
                                </span>
                                <Input
                                  placeholder="role slug, e.g. event_time"
                                  value={roleValue(table.table_name, column)}
                                  onChange={(event) =>
                                    setColumnRole(table.table_name, column, event.target.value)
                                  }
                                />
                                {roleValue(table.table_name, column) !== '' && (
                                  <Button
                                    variant="ghost"
                                    size="sm"
                                    aria-label={`Clear role for ${column}`}
                                    onClick={() => setColumnRole(table.table_name, column, '')}
                                  >
                                    <Trash2 size={13} aria-hidden="true" />
                                  </Button>
                                )}
                              </div>
                            ))}
                          </div>
                        </div>
                      )}
                    </div>
                  );
                })}
              </div>
            </div>

            <div className="flex items-center gap-2 flex-wrap">
              <Button
                variant="secondary"
                loading={saveMutation.isPending}
                disabled={overridesQuery.isLoading}
                onClick={() => saveMutation.mutate()}
                className="min-h-[44px]"
              >
                Save overrides
              </Button>
              <span className="text-xs text-fg-subtle">
                Overrides take effect on the next rebuild.
              </span>
            </div>
          </>
        )}
      </div>
    </Card>
  );
}

function Counter({ label, value }: { label: string; value: number }) {
  return (
    <div className="rounded-md border border-border bg-elevated px-2.5 py-2">
      <p className="text-fg-muted">{label}</p>
      <p className="text-base font-semibold text-fg tabular-nums">{value}</p>
    </div>
  );
}
