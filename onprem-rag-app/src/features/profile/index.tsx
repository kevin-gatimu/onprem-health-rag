// Profile screen — self-service for ALL roles (Stage 9, Layer 3a).
// Mobile-first: single column, max-w-2xl mx-auto. Tap targets ≥ 44 px. Dark theme only.
//
// What the user can change here:
//   • Display name — inline edit row, Enter saves, Esc cancels.
//   • Password — modal with current / new / confirm validation before bridging.
//
// Read-only fields: email (admin-assigned), role (admin-assigned), member since.
//
// After a name save, useSession.setUser(updated) is called so the sidebar/header
// refresh without a page reload.
import { useState } from 'react';
import { Check, KeyRound, Pencil, X } from 'lucide-react';
import { updateMe, changeMyPassword } from '../../lib/bridge';
import { toast } from '../../stores/ui';
import { useSession } from '../../stores/session';
import { Button, Input, Badge, Modal, Card } from '../../components/ui';
import { PageContainer } from '../../components/layout/PageContainer';
import { ROLE_BADGE, ROLE_LABEL } from '../admin/roles';

/** Derive 1-2 initials from display name, falling back to username. */
function getInitials(name: string, username: string): string {
  const src = (name.trim() !== '' ? name : username).trim();
  const parts = src.split(/\s+/).filter(Boolean);
  if (parts.length >= 2) {
    return (parts[0][0] + parts[1][0]).toUpperCase();
  }
  return src.slice(0, 2).toUpperCase() || '?';
}

export default function Profile() {
  const user = useSession((s) => s.user);
  const setUser = useSession((s) => s.setUser);

  // ── Name inline edit ─────────────────────────────────────────────────────────
  const [editing, setEditing] = useState(false);
  const [nameInput, setNameInput] = useState('');
  const [nameSaving, setNameSaving] = useState(false);

  // ── Change password modal ────────────────────────────────────────────────────
  const [pwOpen, setPwOpen] = useState(false);
  const [currentPw, setCurrentPw] = useState('');
  const [newPw, setNewPw] = useState('');
  const [confirmPw, setConfirmPw] = useState('');
  const [currentPwErr, setCurrentPwErr] = useState('');
  const [newPwErr, setNewPwErr] = useState('');
  const [confirmPwErr, setConfirmPwErr] = useState('');
  const [pwSaving, setPwSaving] = useState(false);

  // ── Guard (the route requires auth, but keep a defensive fallback) ────────────
  if (!user) {
    return (
      <div className="flex flex-col items-center justify-center py-20 text-fg-muted text-sm">
        Not signed in.
      </div>
    );
  }

  // Derived display values — safe to compute because user is non-null here.
  const initials = getInitials(user.name, user.username);
  const displayName = user.name.trim() !== '' ? user.name : user.username;
  const memberSince = new Date(user.created_at).toLocaleDateString(undefined, {
    year: 'numeric',
    month: 'long',
    day: 'numeric',
  });
  const canSaveName =
    nameInput.trim() !== '' && nameInput.trim() !== user.name;

  // ── Name handlers ─────────────────────────────────────────────────────────────
  function startEditing() {
    setNameInput(user!.name);
    setEditing(true);
  }

  function cancelEditing() {
    setEditing(false);
  }

  async function saveName() {
    const trimmed = nameInput.trim();
    if (!trimmed || trimmed === user!.name) {
      setEditing(false);
      return;
    }
    setNameSaving(true);
    try {
      const updated = await updateMe(trimmed);
      setUser(updated);
      toast.success('Display name updated.');
      setEditing(false);
    } catch (err) {
      toast.error(String(err));
    } finally {
      setNameSaving(false);
    }
  }

  function handleNameKey(e: React.KeyboardEvent<HTMLInputElement>) {
    if (e.key === 'Enter') { e.preventDefault(); saveName(); }
    if (e.key === 'Escape') { cancelEditing(); }
  }

  // ── Password handlers ─────────────────────────────────────────────────────────
  function closePwModal() {
    if (pwSaving) return;
    setPwOpen(false);
    setCurrentPw('');
    setNewPw('');
    setConfirmPw('');
    setCurrentPwErr('');
    setNewPwErr('');
    setConfirmPwErr('');
  }

  async function submitPasswordChange() {
    // Client validation — clear all errors then check each field.
    setCurrentPwErr('');
    setNewPwErr('');
    setConfirmPwErr('');

    let valid = true;
    if (!currentPw.trim()) {
      setCurrentPwErr('Current password is required.');
      valid = false;
    }
    if (newPw.length < 8) {
      setNewPwErr('New password must be at least 8 characters.');
      valid = false;
    }
    if (newPw !== confirmPw) {
      setConfirmPwErr('Passwords do not match.');
      valid = false;
    }
    if (!valid) return;

    setPwSaving(true);
    try {
      await changeMyPassword(currentPw, newPw);
      toast.success('Password changed successfully.');
      closePwModal();
    } catch (err) {
      // Bridge maps HTTP 401 → "current password is incorrect".
      const msg = String(err);
      if (
        msg.toLowerCase().includes('incorrect') ||
        msg.toLowerCase().includes('unauthorized')
      ) {
        setCurrentPwErr('Current password is incorrect.');
      } else {
        setCurrentPwErr(msg);
      }
    } finally {
      setPwSaving(false);
    }
  }

  // ── Render ────────────────────────────────────────────────────────────────────
  return (
    // Tighter cap than default flow — profile is a compact form, not a reading page.
    <PageContainer variant="flow" className="max-w-2xl 3xl:max-w-3xl 4xl:max-w-4xl">
    <div className="flex flex-col gap-5">

      {/* ── Page header ───────────────────────────────────────────────────────── */}
      <div>
        <h1 className="text-xl font-bold text-fg">Profile</h1>
        <p className="text-sm text-fg-muted mt-0.5">
          Manage your personal information and security settings.
        </p>
      </div>

      {/* ── Identity card ─────────────────────────────────────────────────────── */}
      <Card>
        <div className="flex flex-col items-center gap-4 py-2 sm:flex-row sm:items-start">
          {/* Avatar */}
          <div
            className={
              'w-16 h-16 rounded-full bg-accent flex items-center justify-center ' +
              'text-xl font-bold text-accent-fg flex-shrink-0 select-none'
            }
            aria-hidden="true"
          >
            {initials}
          </div>
          {/* Name / email / role */}
          <div className="flex flex-col gap-1 min-w-0 w-full text-center sm:text-left">
            <span className="text-lg font-semibold text-fg break-words">{displayName}</span>
            <span className="text-sm text-fg-muted break-all">{user.email}</span>
            <div className="flex justify-center sm:justify-start mt-1">
              <Badge variant={ROLE_BADGE[user.role]}>{ROLE_LABEL[user.role]}</Badge>
            </div>
          </div>
        </div>
      </Card>

      {/* ── Account details ───────────────────────────────────────────────────── */}
      <Card title="Account details">
        <div className="flex flex-col gap-5">

          {/* Display name — inline edit */}
          <div className="flex flex-col gap-1">
            <label className="text-sm font-medium text-fg-muted">Display name</label>
            {editing ? (
              <div className="flex flex-col gap-2 sm:flex-row sm:items-start">
                <Input
                  value={nameInput}
                  onChange={(e) => setNameInput(e.target.value)}
                  onKeyDown={handleNameKey}
                  autoFocus
                  placeholder="Your display name"
                  className="flex-1"
                />
                <div className="flex gap-2">
                  <Button
                    variant="primary"
                    size="sm"
                    className="min-h-[44px] flex-1 sm:flex-none"
                    onClick={saveName}
                    loading={nameSaving}
                    disabled={!canSaveName}
                    leftIcon={<Check size={14} />}
                  >
                    Save
                  </Button>
                  <Button
                    variant="ghost"
                    size="sm"
                    className="min-h-[44px] flex-1 sm:flex-none"
                    onClick={cancelEditing}
                    disabled={nameSaving}
                    leftIcon={<X size={14} />}
                  >
                    Cancel
                  </Button>
                </div>
              </div>
            ) : (
              <div className="flex items-center justify-between gap-3 min-h-[44px]">
                <span className="text-sm text-fg py-2 flex-1 break-words">
                  {user.name.trim() !== '' ? user.name : (
                    <em className="text-fg-muted not-italic">Not set</em>
                  )}
                </span>
                <Button
                  variant="ghost"
                  size="sm"
                  className="min-h-[44px] flex-shrink-0"
                  onClick={startEditing}
                  leftIcon={<Pencil size={14} />}
                >
                  Edit
                </Button>
              </div>
            )}
          </div>

          {/* Email — read-only */}
          <div className="flex flex-col gap-1">
            <label className="text-sm font-medium text-fg-muted">Email address</label>
            <span className="text-sm text-fg py-2 break-all">{user.email}</span>
            <span className="text-xs text-fg-subtle">
              Contact an administrator to change your email.
            </span>
          </div>

          {/* Role — read-only */}
          <div className="flex flex-col gap-1">
            <label className="text-sm font-medium text-fg-muted">Role</label>
            <div className="py-1.5">
              <Badge variant={ROLE_BADGE[user.role]}>{ROLE_LABEL[user.role]}</Badge>
            </div>
            <span className="text-xs text-fg-subtle">Assigned by your administrator.</span>
          </div>

          {/* Member since */}
          <div className="flex flex-col gap-1">
            <label className="text-sm font-medium text-fg-muted">Member since</label>
            <span className="text-sm text-fg py-2">{memberSince}</span>
          </div>
        </div>
      </Card>

      {/* ── Security ──────────────────────────────────────────────────────────── */}
      <Card title="Security">
        <div className="flex items-center justify-between gap-4">
          <div className="flex flex-col gap-0.5">
            <span className="text-sm font-medium text-fg">Password</span>
            <span className="text-xs text-fg-muted">Update your login password.</span>
          </div>
          <Button
            variant="secondary"
            leftIcon={<KeyRound size={16} />}
            onClick={() => setPwOpen(true)}
            className="min-h-[44px] flex-shrink-0"
          >
            Change
          </Button>
        </div>
      </Card>

      {/* ── Change password modal ─────────────────────────────────────────────── */}
      <Modal
        open={pwOpen}
        onClose={closePwModal}
        title="Change password"
        footer={
          <>
            <Button
              variant="ghost"
              onClick={closePwModal}
              disabled={pwSaving}
              className="min-h-[44px]"
            >
              Cancel
            </Button>
            <Button
              variant="primary"
              onClick={submitPasswordChange}
              loading={pwSaving}
              className="min-h-[44px]"
            >
              Update password
            </Button>
          </>
        }
      >
        <div className="flex flex-col gap-4">
          <Input
            label="Current password"
            type="password"
            showToggle
            value={currentPw}
            onChange={(e) => setCurrentPw(e.target.value)}
            error={currentPwErr || undefined}
            autoComplete="current-password"
          />
          <Input
            label="New password"
            type="password"
            showToggle
            value={newPw}
            onChange={(e) => setNewPw(e.target.value)}
            hint="Minimum 8 characters."
            error={newPwErr || undefined}
            autoComplete="new-password"
          />
          <Input
            label="Confirm new password"
            type="password"
            showToggle
            value={confirmPw}
            onChange={(e) => setConfirmPw(e.target.value)}
            error={confirmPwErr || undefined}
            autoComplete="new-password"
          />
        </div>
      </Modal>
    </div>
    </PageContainer>
  );
}
