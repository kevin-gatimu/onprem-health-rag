// ConnectionFormModal — Add or Edit a source database connection.
// Calling conventions: save/update bridge calls live here; query invalidation
// is lifted to the parent via onSaved() so Connections.tsx is the single owner
// of ['sources'] and ['stats'] cache state.
import { useState, useEffect } from 'react';
import { Button, Input, Select, Modal } from '../../components/ui';
import { testSource, saveSource, updateSource } from '../../lib/bridge';
import { toast } from '../../stores/ui';
import type { SourceInfo, SourceKind } from '../../lib/bridge';

export interface ConnectionFormModalProps {
  open: boolean;
  onClose: () => void;
  /** Null = adding a new connection; non-null = editing an existing one. */
  editing: SourceInfo | null;
  /** Called after a successful save/update so the parent can invalidate queries. */
  onSaved: () => void;
}

interface FormState {
  name: string;
  kind: SourceKind;
  host: string;
  /** Stored as a string because HTML <input type="number"> always yields a string. */
  port: string;
  database: string;
  username: string;
  password: string;
  query: string;
  table: string;
}

interface FormErrors {
  name?: string;
  host?: string;
  database?: string;
  username?: string;
  password?: string;
}

// Default port placeholder shown when the field is blank.
// We display these as hints/placeholders — we never force a value so the server
// can use its own default when the user leaves the field empty.
const KIND_PORT_PLACEHOLDER: Record<SourceKind, string> = {
  postgres: '5432',
  mysql: '3306',
  mssql: '1433',
};

const BLANK_FORM: FormState = {
  name: '',
  kind: 'postgres',
  host: '',
  port: '',
  database: '',
  username: '',
  password: '',
  query: '',
  table: '',
};

export default function ConnectionFormModal({
  open,
  onClose,
  editing,
  onSaved,
}: ConnectionFormModalProps) {
  const [form, setFormState] = useState<FormState>(BLANK_FORM);
  const [errors, setErrors] = useState<FormErrors>({});
  const [showAdvanced, setShowAdvanced] = useState(false);
  const [testing, setTesting] = useState(false);
  const [saving, setSaving] = useState(false);

  // Reset form state every time the modal opens or the editing target changes.
  // Keeps stale input from one session leaking into the next.
  useEffect(() => {
    if (!open) return;
    if (editing) {
      setFormState({
        name:     editing.name,
        kind:     editing.kind,
        host:     editing.host,
        port:     editing.port != null ? String(editing.port) : '',
        database: editing.database,
        username: editing.username,
        password: '', // always blank — show hint "leave blank to keep current"
        query:    editing.query ?? '',
        table:    editing.table ?? '',
      });
    } else {
      setFormState(BLANK_FORM);
    }
    setErrors({});
    setShowAdvanced(false);
  }, [open, editing]);

  // Merge a partial update into form state.
  function set(partial: Partial<FormState>) {
    setFormState((prev) => ({ ...prev, ...partial }));
  }

  // Clear a single inline validation error when the user starts correcting a field.
  function clearErr(field: keyof FormErrors) {
    setErrors((prev) => ({ ...prev, [field]: undefined }));
  }

  // Parse the port string → number | null. Empty string or non-numeric → null.
  // We never send NaN to the bridge.
  function parsePort(s: string): number | null {
    const trimmed = s.trim();
    if (!trimmed) return null;
    const n = Number(trimmed);
    return Number.isNaN(n) ? null : n;
  }

  // Validate required fields; sets inline errors and returns true iff all valid.
  function validate(): boolean {
    const errs: FormErrors = {};
    if (!form.name.trim())     errs.name     = 'Name is required.';
    if (!form.host.trim())     errs.host     = 'Host is required.';
    if (!form.database.trim()) errs.database = 'Database is required.';
    if (!form.username.trim()) errs.username = 'Username is required.';
    // Password is required only when adding; on edit a blank field means "keep current".
    if (!editing && !form.password.trim()) errs.password = 'Password is required when adding.';
    setErrors(errs);
    return Object.keys(errs).length === 0;
  }

  async function handleTest() {
    // We cannot test unsaved edits without a password — the stored password is
    // server-side only. The card's Test button re-tests the saved source instead.
    if (!form.password.trim()) {
      toast.info('Enter a password to test unsaved changes, or use the card\'s Test button to re-test the saved connection.');
      return;
    }
    setTesting(true);
    try {
      await testSource({
        name:     form.name || 'test',
        kind:     form.kind,
        host:     form.host,
        port:     parsePort(form.port),
        database: form.database,
        username: form.username,
        password: form.password,
        query:    form.query  || null,
        table:    form.table  || null,
      });
      toast.success('Connection successful.');
    } catch (err) {
      toast.error(String(err));
    } finally {
      setTesting(false);
    }
  }

  async function handleSave() {
    if (!validate()) return;
    setSaving(true);
    try {
      if (editing) {
        // Build a SourceUpdate — only include password when the user typed something.
        const patch = {
          name:     form.name,
          kind:     form.kind,
          host:     form.host,
          port:     parsePort(form.port),
          database: form.database,
          username: form.username,
          query:    form.query  || null,
          table:    form.table  || null,
          ...(form.password.trim() ? { password: form.password } : {}),
        };
        await updateSource(editing.id, patch);
        toast.success(`"${form.name}" updated.`);
      } else {
        await saveSource({
          name:     form.name,
          kind:     form.kind,
          host:     form.host,
          port:     parsePort(form.port),
          database: form.database,
          username: form.username,
          password: form.password,
          query:    form.query  || null,
          table:    form.table  || null,
        });
        toast.success(`"${form.name}" added and verified.`);
      }
      // Parent owns the cache; it invalidates ['sources'] and ['stats'], then closes.
      onSaved();
    } catch (err) {
      // Keep modal open so the user can correct and retry.
      toast.error(String(err));
    } finally {
      setSaving(false);
    }
  }

  // When editing with no password entered, we can't construct a valid SourceInput
  // for an unsaved test — disable the Test button and surface a tooltip hint.
  const testDisabled = editing !== null && !form.password.trim();

  return (
    <Modal
      open={open}
      onClose={onClose}
      title={editing ? 'Edit Connection' : 'Add Connection'}
      size="md"
      footer={
        <>
          {/* mr-auto pushes Cancel/Save to the right inside the footer's flex-end row */}
          <Button
            variant="secondary"
            size="sm"
            loading={testing}
            disabled={testDisabled}
            onClick={handleTest}
            title={testDisabled ? 'Enter a password above to test unsaved changes.' : undefined}
            className="mr-auto min-h-[44px]"
          >
            Test Connection
          </Button>
          <Button
            variant="ghost"
            size="sm"
            onClick={onClose}
            className="min-h-[44px]"
          >
            Cancel
          </Button>
          <Button
            variant="primary"
            size="sm"
            loading={saving}
            onClick={handleSave}
            className="min-h-[44px]"
          >
            Save
          </Button>
        </>
      }
    >
      <div className="flex flex-col gap-4">

        {/* Name */}
        <Input
          label="Name"
          placeholder="e.g. Clinic EMR"
          value={form.name}
          onChange={(e) => { set({ name: e.target.value }); clearErr('name'); }}
          error={errors.name}
          required
        />

        {/* Type */}
        <Select
          label="Type"
          value={form.kind}
          onChange={(e) => set({ kind: e.target.value as SourceKind })}
          options={[
            { value: 'postgres', label: 'PostgreSQL' },
            { value: 'mysql',    label: 'MySQL' },
            { value: 'mssql',    label: 'SQL Server' },
          ]}
        />

        {/* Host + Port — side-by-side on md+, stacked on phones */}
        <div className="grid grid-cols-1 md:grid-cols-2 gap-4">
          <Input
            label="Host"
            placeholder="e.g. 192.168.1.10"
            value={form.host}
            onChange={(e) => { set({ host: e.target.value }); clearErr('host'); }}
            error={errors.host}
            required
          />
          <Input
            label="Port"
            type="number"
            placeholder={KIND_PORT_PLACEHOLDER[form.kind]}
            hint={`Default: ${KIND_PORT_PLACEHOLDER[form.kind]}`}
            value={form.port}
            onChange={(e) => set({ port: e.target.value })}
          />
        </div>

        {/* Database */}
        <Input
          label="Database"
          placeholder="e.g. clinic_db"
          value={form.database}
          onChange={(e) => { set({ database: e.target.value }); clearErr('database'); }}
          error={errors.database}
          required
        />

        {/* Username */}
        <Input
          label="Username"
          placeholder="e.g. readonly_user"
          value={form.username}
          onChange={(e) => { set({ username: e.target.value }); clearErr('username'); }}
          error={errors.username}
          required
        />

        {/* Password — blank on edit means "keep stored password" */}
        <Input
          label="Password"
          type="password"
          showToggle
          placeholder={editing ? '(unchanged)' : 'Enter password'}
          hint={editing ? 'Leave blank to keep the current password.' : undefined}
          value={form.password}
          onChange={(e) => { set({ password: e.target.value }); clearErr('password'); }}
          error={errors.password}
          required={!editing}
        />

        {/* Advanced — collapsed by default; query + table are optional */}
        <div>
          <button
            type="button"
            onClick={() => setShowAdvanced((v) => !v)}
            className="flex items-center gap-1.5 text-xs text-fg-muted hover:text-fg transition-colors duration-150 min-h-[44px]"
          >
            <span className="text-[10px]">{showAdvanced ? '▲' : '▼'}</span>
            Advanced options (optional)
          </button>
          {showAdvanced && (
            <div className="flex flex-col gap-4 mt-3">
              <Input
                label="Custom Query"
                placeholder="SELECT * FROM patients WHERE active = true"
                hint="Optional. Overrides the default full-table fetch during ingestion."
                value={form.query}
                onChange={(e) => set({ query: e.target.value })}
              />
              <Input
                label="Table"
                placeholder="e.g. patients"
                hint="Optional. Limit ingestion to a single table (if no custom query is set)."
                value={form.table}
                onChange={(e) => set({ table: e.target.value })}
              />
            </div>
          )}
        </div>

      </div>
    </Modal>
  );
}
