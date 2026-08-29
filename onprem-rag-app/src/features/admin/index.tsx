// Admin / User Management screen — admin role only (Stage 9, Layer 3b).
// Mobile-first: user rows are stacked cards below `md`; a full table at `md+`.
// The table is NOT rendered at all below `md` so there is zero horizontal scroll at 360px.
//
// Gating: every mutating control is gated on the REAL role (useSession), never previewRole.
// The route guard already blocks non-admins from reaching this page; we gate controls anyway.
//
// Five modals: Create · Edit · Change password · Delete confirm.
// Each modal has its own local form state + loading flag; mutations invalidate ['users'].
import { useState } from 'react';
import { useQuery, useQueryClient } from '@tanstack/react-query';
import { KeyRound, Pencil, Trash2, UserPlus, Users } from 'lucide-react';
import {
  listUsers,
  createUser,
  updateUser,
  deleteUser,
  setUserPassword,
} from '../../lib/bridge';
import type { User, Role } from '../../lib/types';
import { toast } from '../../stores/ui';
import { useSession } from '../../stores/session';
import { Button, Input, Select, Badge, Modal, Card, EmptyState, cn } from '../../components/ui';
import { PageContainer } from '../../components/layout/PageContainer';
import { ROLE_OPTIONS, ROLE_LABEL, ROLE_BADGE } from './roles';

/** Format a created_at RFC3339 string for display in the table/card. */
function fmtDate(iso: string): string {
  return new Date(iso).toLocaleDateString(undefined, {
    year: 'numeric',
    month: 'short',
    day: 'numeric',
  });
}

/** Display name: prefer user.name, fall back to user.username. */
function displayName(u: User): string {
  return u.name.trim() !== '' ? u.name : u.username;
}

/** Small inline "you" tag used on the self row. */
function YouTag() {
  return (
    <span className="text-xs bg-accent-subtle text-accent px-1.5 py-0.5 rounded-full font-medium">
      you
    </span>
  );
}

export default function Admin() {
  const currentUser = useSession((s) => s.user);
  const isAdmin = currentUser?.role === 'admin';

  const qc = useQueryClient();

  const usersQuery = useQuery({
    queryKey: ['users'],
    queryFn: listUsers,
    staleTime: 30_000,
  });
  const users = usersQuery.data ?? [];

  function invalidateUsers() {
    qc.invalidateQueries({ queryKey: ['users'] });
  }

  // ── Role select options (reused across create + edit modals) ─────────────────
  const roleOptions = ROLE_OPTIONS.map((r) => ({ value: r, label: ROLE_LABEL[r] }));

  // ─────────────────────────────────────────────────────────────────────────────
  // CREATE modal
  // ─────────────────────────────────────────────────────────────────────────────
  const [createOpen, setCreateOpen] = useState(false);
  const [cUsername, setCUsername] = useState('');
  const [cName, setCName] = useState('');
  const [cEmail, setCEmail] = useState('');
  const [cPassword, setCPassword] = useState('');
  const [cRole, setCRole] = useState<Role>('doctor');
  const [cError, setCError] = useState('');
  const [creating, setCreating] = useState(false);

  function openCreate() {
    setCUsername(''); setCName(''); setCEmail(''); setCPassword('');
    setCRole('doctor'); setCError('');
    setCreateOpen(true);
  }
  function closeCreate() {
    if (creating) return;
    setCreateOpen(false);
  }

  async function submitCreate() {
    setCError('');
    if (!cUsername.trim()) { setCError('Username is required.'); return; }
    if (cPassword.length < 8) { setCError('Password must be at least 8 characters.'); return; }
    setCreating(true);
    try {
      await createUser({
        username: cUsername.trim(),
        name: cName.trim() || undefined,
        email: cEmail.trim() || undefined,
        password: cPassword,
        role: cRole,
      });
      toast.success(`User "${cUsername.trim()}" created.`);
      invalidateUsers();
      setCreateOpen(false);
    } catch (err) {
      setCError(String(err));
    } finally {
      setCreating(false);
    }
  }

  // ─────────────────────────────────────────────────────────────────────────────
  // EDIT modal
  // ─────────────────────────────────────────────────────────────────────────────
  const [editTarget, setEditTarget] = useState<User | null>(null);
  const [eName, setEName] = useState('');
  const [eEmail, setEEmail] = useState('');
  const [eRole, setERole] = useState<Role>('doctor');
  const [eError, setEError] = useState('');
  const [updating, setUpdating] = useState(false);

  function openEdit(u: User) {
    setEName(u.name); setEEmail(u.email); setERole(u.role); setEError('');
    setEditTarget(u);
  }
  function closeEdit() {
    if (updating) return;
    setEditTarget(null);
  }

  async function submitEdit() {
    if (!editTarget) return;
    setEError('');
    if (!eName.trim()) { setEError('Full name is required.'); return; }
    setUpdating(true);
    try {
      await updateUser(editTarget.id, {
        name: eName.trim(),
        email: eEmail.trim() || undefined,
        role: eRole,
      });
      toast.success(`User "${editTarget.username}" updated.`);
      invalidateUsers();
      setEditTarget(null);
    } catch (err) {
      setEError(String(err));
    } finally {
      setUpdating(false);
    }
  }

  // ─────────────────────────────────────────────────────────────────────────────
  // SET PASSWORD modal (admin path — no current-password check)
  // ─────────────────────────────────────────────────────────────────────────────
  const [pwTarget, setPwTarget] = useState<User | null>(null);
  const [pwNew, setPwNew] = useState('');
  const [pwError, setPwError] = useState('');
  const [settingPw, setSettingPw] = useState(false);

  function openPw(u: User) {
    setPwNew(''); setPwError('');
    setPwTarget(u);
  }
  function closePw() {
    if (settingPw) return;
    setPwTarget(null);
  }

  async function submitSetPw() {
    if (!pwTarget) return;
    setPwError('');
    if (pwNew.length < 8) { setPwError('Password must be at least 8 characters.'); return; }
    setSettingPw(true);
    try {
      await setUserPassword(pwTarget.id, pwNew);
      toast.success(`Password updated for "${pwTarget.username}".`);
      invalidateUsers();
      setPwTarget(null);
    } catch (err) {
      setPwError(String(err));
    } finally {
      setSettingPw(false);
    }
  }

  // ─────────────────────────────────────────────────────────────────────────────
  // DELETE confirm modal
  // ─────────────────────────────────────────────────────────────────────────────
  const [deleteTarget, setDeleteTarget] = useState<User | null>(null);
  const [deleting, setDeleting] = useState(false);

  function openDelete(u: User) { setDeleteTarget(u); }
  function closeDelete() {
    if (deleting) return;
    setDeleteTarget(null);
  }

  async function submitDelete() {
    if (!deleteTarget) return;
    setDeleting(true);
    try {
      await deleteUser(deleteTarget.id);
      toast.success(`User "${deleteTarget.username}" deleted.`);
      invalidateUsers();
      setDeleteTarget(null);
    } catch (err) {
      toast.error(String(err));
      setDeleteTarget(null);
    } finally {
      setDeleting(false);
    }
  }

  // ─────────────────────────────────────────────────────────────────────────────
  // Render
  // ─────────────────────────────────────────────────────────────────────────────
  return (
    <PageContainer variant="board">
    <div className="flex flex-col gap-5">

      {/* ── Header ──────────────────────────────────────────────────────────── */}
      <div className="flex flex-col gap-3 sm:flex-row sm:items-start sm:justify-between">
        <div>
          <h1 className="text-xl font-bold text-fg">User Management</h1>
          <p className="text-sm text-fg-muted mt-0.5">
            Create, edit, and manage user accounts and roles.
          </p>
        </div>
        {isAdmin && (
          <Button
            variant="primary"
            leftIcon={<UserPlus size={16} />}
            onClick={openCreate}
            className="w-full sm:w-auto min-h-[44px] sm:flex-shrink-0"
          >
            New user
          </Button>
        )}
      </div>

      {/* ── User list ───────────────────────────────────────────────────────── */}
      {usersQuery.isLoading ? (
        <p className="text-sm text-fg-muted">Loading users…</p>
      ) : usersQuery.isError ? (
        <p className="text-sm text-danger">Failed to load users. {String(usersQuery.error)}</p>
      ) : users.length === 0 ? (
        <EmptyState
          icon={<Users size={32} />}
          title="No users yet"
          description="Create the first user account to get started."
        />
      ) : (
        <>
          {/* ── Mobile cards (hidden at md+) ─────────────────────────────── */}
          <div className="flex flex-col gap-3 md:hidden">
            {users.map((u) => {
              const isSelf = u.id === currentUser?.id;
              return (
                <Card
                  key={u.id}
                  className={cn(isSelf && 'ring-1 ring-accent/30')}
                >
                  {/* Top row: name + "you" + role badge */}
                  <div className="flex flex-wrap items-center gap-2 mb-2">
                    <span className="text-sm font-semibold text-fg break-words">
                      {displayName(u)}
                    </span>
                    {isSelf && <YouTag />}
                    <Badge variant={ROLE_BADGE[u.role]}>{ROLE_LABEL[u.role]}</Badge>
                  </div>
                  {/* Details */}
                  <div className="flex flex-col gap-1 mb-3">
                    <span className="text-xs text-fg-muted break-all">{u.email}</span>
                    <span className="text-xs text-fg-subtle">
                      @{u.username} · joined {fmtDate(u.created_at)}
                    </span>
                  </div>
                  {/* Actions */}
                  {isAdmin && (
                    <div className="flex flex-wrap gap-2 border-t border-border pt-3">
                      <Button
                        variant="ghost"
                        size="sm"
                        leftIcon={<Pencil size={14} />}
                        onClick={() => openEdit(u)}
                        className="min-h-[44px] flex-1"
                      >
                        Edit
                      </Button>
                      <Button
                        variant="ghost"
                        size="sm"
                        leftIcon={<KeyRound size={14} />}
                        onClick={() => openPw(u)}
                        className="min-h-[44px] flex-1"
                      >
                        Password
                      </Button>
                      <Button
                        variant="danger"
                        size="sm"
                        leftIcon={<Trash2 size={14} />}
                        onClick={() => openDelete(u)}
                        disabled={isSelf}
                        className="min-h-[44px] flex-1"
                      >
                        Delete
                      </Button>
                    </div>
                  )}
                </Card>
              );
            })}
          </div>

          {/* ── Desktop table (hidden below md) ──────────────────────────── */}
          <div className="hidden md:block">
            <Card padding="p-0">
              <table className="w-full text-sm">
                <thead>
                  <tr className="border-b border-border">
                    <th className="px-4 py-3 text-left font-medium text-fg-muted">Name</th>
                    <th className="px-4 py-3 text-left font-medium text-fg-muted">Email</th>
                    <th className="px-4 py-3 text-left font-medium text-fg-muted">Role</th>
                    <th className="px-4 py-3 text-left font-medium text-fg-muted whitespace-nowrap">
                      Created
                    </th>
                    {isAdmin && (
                      <th className="px-4 py-3 text-right font-medium text-fg-muted">Actions</th>
                    )}
                  </tr>
                </thead>
                <tbody>
                  {users.map((u) => {
                    const isSelf = u.id === currentUser?.id;
                    return (
                      <tr
                        key={u.id}
                        className={cn(
                          'border-b border-border last:border-0',
                          isSelf && 'bg-accent/5',
                        )}
                      >
                        <td className="px-4 py-3 text-fg">
                          <div className="flex items-center gap-1.5 min-w-0">
                            <span className="truncate max-w-[160px]">{displayName(u)}</span>
                            {isSelf && <YouTag />}
                          </div>
                          <span className="text-xs text-fg-subtle">@{u.username}</span>
                        </td>
                        <td className="px-4 py-3 text-fg-muted break-all">{u.email}</td>
                        <td className="px-4 py-3">
                          <Badge variant={ROLE_BADGE[u.role]}>{ROLE_LABEL[u.role]}</Badge>
                        </td>
                        <td className="px-4 py-3 text-fg-muted whitespace-nowrap">
                          {fmtDate(u.created_at)}
                        </td>
                        {isAdmin && (
                          <td className="px-4 py-3">
                            <div className="flex items-center justify-end gap-1">
                              <button
                                onClick={() => openEdit(u)}
                                className={
                                  'flex items-center justify-center w-[38px] h-[38px] rounded-md ' +
                                  'text-fg-subtle hover:bg-elevated hover:text-fg transition-colors'
                                }
                                aria-label={`Edit ${u.username}`}
                              >
                                <Pencil size={15} />
                              </button>
                              <button
                                onClick={() => openPw(u)}
                                className={
                                  'flex items-center justify-center w-[38px] h-[38px] rounded-md ' +
                                  'text-fg-subtle hover:bg-elevated hover:text-fg transition-colors'
                                }
                                aria-label={`Change password for ${u.username}`}
                              >
                                <KeyRound size={15} />
                              </button>
                              <button
                                onClick={() => openDelete(u)}
                                disabled={isSelf}
                                className={cn(
                                  'flex items-center justify-center w-[38px] h-[38px] rounded-md transition-colors',
                                  isSelf
                                    ? 'opacity-30 cursor-not-allowed text-fg-subtle'
                                    : 'text-danger hover:bg-danger-subtle',
                                )}
                                aria-label={
                                  isSelf
                                    ? 'Cannot delete your own account'
                                    : `Delete ${u.username}`
                                }
                              >
                                <Trash2 size={15} />
                              </button>
                            </div>
                          </td>
                        )}
                      </tr>
                    );
                  })}
                </tbody>
              </table>
            </Card>
          </div>
        </>
      )}

      {/* ── CREATE modal ──────────────────────────────────────────────────────── */}
      <Modal
        open={createOpen}
        onClose={closeCreate}
        title="New user"
        size="md"
        footer={
          <>
            <Button variant="ghost" onClick={closeCreate} disabled={creating} className="min-h-[44px]">
              Cancel
            </Button>
            <Button variant="primary" onClick={submitCreate} loading={creating} className="min-h-[44px]">
              Create user
            </Button>
          </>
        }
      >
        <div className="flex flex-col gap-4">
          <Input
            label="Username"
            value={cUsername}
            onChange={(e) => setCUsername(e.target.value)}
            placeholder="e.g. jsmith"
            autoComplete="username"
          />
          <Input
            label="Full name"
            value={cName}
            onChange={(e) => setCName(e.target.value)}
            placeholder="e.g. Jane Smith"
            autoComplete="name"
          />
          <Input
            label="Email"
            type="email"
            value={cEmail}
            onChange={(e) => setCEmail(e.target.value)}
            placeholder="user@hospital.org"
            autoComplete="email"
          />
          <Input
            label="Password"
            type="password"
            showToggle
            value={cPassword}
            onChange={(e) => setCPassword(e.target.value)}
            hint="Minimum 8 characters."
            autoComplete="new-password"
          />
          <Select
            label="Role"
            value={cRole}
            onChange={(e) => setCRole(e.target.value as Role)}
            options={roleOptions}
          />
          {cError && (
            <p className="text-sm text-danger">{cError}</p>
          )}
        </div>
      </Modal>

      {/* ── EDIT modal ────────────────────────────────────────────────────────── */}
      <Modal
        open={editTarget !== null}
        onClose={closeEdit}
        title={editTarget ? `Edit "${editTarget.username}"` : 'Edit user'}
        size="md"
        footer={
          <>
            <Button variant="ghost" onClick={closeEdit} disabled={updating} className="min-h-[44px]">
              Cancel
            </Button>
            <Button variant="primary" onClick={submitEdit} loading={updating} className="min-h-[44px]">
              Save changes
            </Button>
          </>
        }
      >
        <div className="flex flex-col gap-4">
          <Input
            label="Full name"
            value={eName}
            onChange={(e) => setEName(e.target.value)}
            placeholder="Full name"
          />
          <Input
            label="Email"
            type="email"
            value={eEmail}
            onChange={(e) => setEEmail(e.target.value)}
            placeholder="user@hospital.org"
          />
          <Select
            label="Role"
            value={eRole}
            onChange={(e) => setERole(e.target.value as Role)}
            options={roleOptions}
          />
          {eError && (
            <p className="text-sm text-danger">{eError}</p>
          )}
        </div>
      </Modal>

      {/* ── SET PASSWORD modal ────────────────────────────────────────────────── */}
      <Modal
        open={pwTarget !== null}
        onClose={closePw}
        title={pwTarget ? `Set password for "${pwTarget.username}"` : 'Set password'}
        size="sm"
        footer={
          <>
            <Button variant="ghost" onClick={closePw} disabled={settingPw} className="min-h-[44px]">
              Cancel
            </Button>
            <Button variant="primary" onClick={submitSetPw} loading={settingPw} className="min-h-[44px]">
              Set password
            </Button>
          </>
        }
      >
        <div className="flex flex-col gap-4">
          <Input
            label="New password"
            type="password"
            showToggle
            value={pwNew}
            onChange={(e) => setPwNew(e.target.value)}
            hint="Minimum 8 characters."
            error={pwError || undefined}
            autoComplete="new-password"
          />
        </div>
      </Modal>

      {/* ── DELETE confirm modal ──────────────────────────────────────────────── */}
      <Modal
        open={deleteTarget !== null}
        onClose={closeDelete}
        title="Delete user?"
        size="sm"
        footer={
          <>
            <Button variant="ghost" onClick={closeDelete} disabled={deleting} className="min-h-[44px]">
              Cancel
            </Button>
            <Button variant="danger" onClick={submitDelete} loading={deleting} className="min-h-[44px]">
              Delete
            </Button>
          </>
        }
      >
        <p className="text-sm text-fg-muted">
          Permanently delete{' '}
          <span className="font-semibold text-fg">
            {deleteTarget?.username ?? ''}
          </span>
          ? This action cannot be undone. Their conversations will be orphaned but not deleted.
        </p>
      </Modal>
    </div>
    </PageContainer>
  );
}
