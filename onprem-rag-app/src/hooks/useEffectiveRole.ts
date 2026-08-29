// The role the UI should render against: an admin previewing another role (via the
// ui store's previewRole) sees the app as that role, everyone else sees their own.
import { useUi } from "../stores/ui";
import { useSession } from "../stores/session";
import type { Role } from "../lib/types";

export function useEffectiveRole(): Role | null {
  const realRole = useSession((s) => s.user?.role ?? null);
  const previewRole = useUi((s) => s.previewRole);
  // Only an admin may preview; a non-admin's stale previewRole is ignored.
  if (realRole === "admin" && previewRole) return previewRole;
  return realRole;
}
