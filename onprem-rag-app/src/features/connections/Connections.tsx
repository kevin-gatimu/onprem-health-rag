// Connections — Stage 3 of the mobile-first UI rebuild.
// Mobile-first: designed at 360 px, then md:/lg:/xl: breakpoints scale up.
// Manages the full lifecycle of source-database connections: list, add, edit, test, delete.
import { useState } from 'react';
import { Plus, Database } from 'lucide-react';
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query';
import { listSources, testSavedSource, deleteSource } from '../../lib/bridge';
import type { SourceInfo } from '../../lib/bridge';
import { toast } from '../../stores/ui';
import { Button, EmptyState } from '../../components/ui';
import { PageContainer } from '../../components/layout/PageContainer';
import ConnectionCard from './ConnectionCard';
import ConnectionFormModal from './ConnectionFormModal';
import SchemaMetadataModal from './SchemaMetadataModal';

export default function Connections() {
  const queryClient = useQueryClient();

  // Track which source is mid-test or mid-delete so the card can show its loading state.
  const [testingId, setTestingId]   = useState<string | null>(null);
  const [deletingId, setDeletingId] = useState<string | null>(null);

  // Modal state: open flag + the source being edited (null = new connection).
  const [modalOpen, setModalOpen]       = useState(false);
  const [editingSource, setEditingSource] = useState<SourceInfo | null>(null);
  const [metadataSource, setMetadataSource] = useState<SourceInfo | null>(null);

  const { data: sources, isLoading } = useQuery({
    queryKey: ['sources'],
    queryFn: listSources,
    staleTime: 30_000,
  });

  // Re-test a saved source. testSavedSource never throws on connection failure —
  // the outcome lands in the returned SourceInfo.status / .error fields.
  const testMutation = useMutation({
    mutationFn: (id: string) => testSavedSource(id),
    onMutate:   (id)        => setTestingId(id),
    onSettled:  ()          => setTestingId(null),
    onSuccess:  (updated)   => {
      queryClient.invalidateQueries({ queryKey: ['sources'] });
      if (updated.status === 'connected') {
        toast.success(`"${updated.name}" connected.`);
      } else {
        // status: 'error' — surface the human-readable reason as a warning so it
        // doesn't feel as alarming as a hard error toast.
        toast.warning(`"${updated.name}" test failed: ${updated.error ?? 'Unknown error'}`);
      }
    },
    onError: (err) => toast.error(String(err)),
  });

  const deleteMutation = useMutation({
    mutationFn: (id: string) => deleteSource(id),
    onMutate:  (id)          => setDeletingId(id),
    onSettled: ()            => setDeletingId(null),
    onSuccess: ()            => {
      queryClient.invalidateQueries({ queryKey: ['sources'] });
      // Invalidate dashboard stats — active_connections is derived from source count.
      queryClient.invalidateQueries({ queryKey: ['stats'] });
      toast.success('Connection deleted.');
    },
    onError: (err) => toast.error(String(err)),
  });

  function openAdd() {
    setEditingSource(null);
    setModalOpen(true);
  }

  function openEdit(source: SourceInfo) {
    setEditingSource(source);
    setModalOpen(true);
  }

  function handleDelete(source: SourceInfo) {
    // Confirm before deleting because ingested records are also removed server-side.
    if (!window.confirm('Delete this connection? Its ingested records will also be removed.')) return;
    deleteMutation.mutate(source.id);
  }

  // Called by the modal after a successful save or update.
  // This is the single place that invalidates the caches so the dashboard and
  // this page both reflect the new state.
  function handleSaved() {
    queryClient.invalidateQueries({ queryKey: ['sources'] });
    queryClient.invalidateQueries({ queryKey: ['stats'] });
    setModalOpen(false);
    setEditingSource(null);
  }

  return (
    <PageContainer variant="board">
    <div className="flex flex-col gap-5">

      {/* ── Header ─────────────────────────────────────────────────────────── */}
      <div className="flex flex-col gap-3 sm:flex-row sm:items-start sm:justify-between">
        <div>
          <h1 className="text-xl font-bold text-fg">Database Connections</h1>
          <p className="text-sm text-fg-muted mt-0.5">
            Connect to your clinic's MySQL, PostgreSQL, or MSSQL databases.
          </p>
        </div>
        {/* On phones the button fills the row below the heading.
            On sm+ it shrinks to content width and sits top-right. */}
        <div className="sm:shrink-0">
          <Button
            variant="primary"
            leftIcon={<Plus size={16} />}
            onClick={openAdd}
            className="w-full sm:w-auto"
          >
            Add Connection
          </Button>
        </div>
      </div>

      {/* ── Connection list ─────────────────────────────────────────────────── */}
      {isLoading ? (
        <p className="text-sm text-fg-muted">Loading connections…</p>
      ) : !sources || sources.length === 0 ? (
        <EmptyState
          icon={<Database size={32} />}
          title="No connections yet"
          description="Add your first database connection to start ingesting health records."
          action={
            <Button variant="primary" leftIcon={<Plus size={16} />} onClick={openAdd}>
              Add Connection
            </Button>
          }
        />
      ) : (
        // Responsive grid: 1-col on phones, 2-col on md+, 3-col on xl+, 4-col on 3xl+.
        <div className="grid gap-4 md:grid-cols-2 xl:grid-cols-3 3xl:grid-cols-4">
          {sources.map((source) => (
            <ConnectionCard
              key={source.id}
              source={source}
              testing={testingId === source.id}
              deleting={deletingId === source.id}
              onTest={() => testMutation.mutate(source.id)}
              onEdit={() => openEdit(source)}
              onDelete={() => handleDelete(source)}
              onManageMetadata={() => setMetadataSource(source)}
            />
          ))}
        </div>
      )}

      {/* ── Add / Edit modal ───────────────────────────────────────────────── */}
      <ConnectionFormModal
        open={modalOpen}
        onClose={() => setModalOpen(false)}
        editing={editingSource}
        onSaved={handleSaved}
      />
      <SchemaMetadataModal
        source={metadataSource}
        onClose={() => setMetadataSource(null)}
      />

    </div>
    </PageContainer>
  );
}
