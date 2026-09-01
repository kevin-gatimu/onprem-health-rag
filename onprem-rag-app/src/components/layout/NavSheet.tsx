// Mobile full-navigation sheet, opened from the TopBar menu or the bottom bar's
// "More" tab. Slides up from the bottom, lists every accessible destination
// grouped by section, and carries the user identity + logout at the foot.
// Rendered only when the ui store's activeSheet === "nav".
import { X, LogOut } from "lucide-react";
import { useUi, currentRoute } from "../../stores/ui";
import { useSession } from "../../stores/session";
import { useEffectiveRole } from "../../hooks/useEffectiveRole";
import { canAccess } from "../../lib/permissions";
import { NAV_ITEMS, NAV_SECTIONS } from "../../app/navigation";
import { logout } from "../../lib/bridge";
import { resetConversationRuntime } from "../../lib/conversationRuntime";
import { queryClient } from "../../lib/queryClient";
import { cn } from "../ui/cn";

export function NavSheet() {
  const activeSheet = useUi((s) => s.activeSheet);
  const closeSheet = useUi((s) => s.closeSheet);
  const navigate = useUi((s) => s.navigate);
  const active = useUi(currentRoute);
  const role = useEffectiveRole();
  const user = useSession((s) => s.user);
  const clearUser = useSession((s) => s.clearUser);

  if (activeSheet !== "nav") return null;

  const items = NAV_ITEMS.filter((i) => canAccess(i.route, role));

  async function handleLogout() {
    closeSheet();
    try {
      await logout();
    } finally {
      resetConversationRuntime();
      queryClient.clear();
      clearUser();
    }
  }

  return (
    <div className="fixed inset-0 z-50 md:hidden" role="dialog" aria-modal="true">
      {/* Scrim */}
      <button
        type="button"
        aria-label="Close navigation"
        onClick={closeSheet}
        className="absolute inset-0 bg-[rgba(12,15,20,0.8)]"
      />
      {/* Sheet */}
      <div
        className="absolute inset-x-0 bottom-0 max-h-[85vh] overflow-y-auto rounded-t-2xl border-t border-border bg-surface"
        style={{ paddingBottom: "env(safe-area-inset-bottom)" }}
      >
        <div className="sticky top-0 flex items-center justify-between border-b border-border bg-surface px-4 py-3">
          <span className="font-semibold text-fg">Navigation</span>
          <button
            type="button"
            aria-label="Close"
            onClick={closeSheet}
            className="rounded-md p-1.5 text-fg-muted hover:bg-elevated hover:text-fg"
          >
            <X size={20} />
          </button>
        </div>

        <div className="p-2">
          {NAV_SECTIONS.map((section) => {
            const sectionItems = items.filter((i) => i.section === section);
            if (sectionItems.length === 0) return null;
            return (
              <div key={section} className="mb-2">
                <div className="px-3 pt-2 pb-1 text-xs uppercase tracking-wide text-fg-subtle">
                  {section}
                </div>
                {sectionItems.map(({ route, label, icon: Icon }) => {
                  const isActive = route === active;
                  return (
                    <button
                      key={route}
                      type="button"
                      onClick={() => navigate(route)}
                      className={cn(
                        "flex w-full items-center gap-3 rounded-lg px-3 py-3 text-sm",
                        isActive
                          ? "bg-accent-subtle text-accent-hover"
                          : "text-fg-muted hover:bg-elevated hover:text-fg",
                      )}
                    >
                      <Icon size={20} className="shrink-0" />
                      <span>{label}</span>
                    </button>
                  );
                })}
              </div>
            );
          })}
        </div>

        {/* User footer */}
        <div className="flex items-center gap-3 border-t border-border p-4">
          <div className="flex h-9 w-9 shrink-0 items-center justify-center rounded-full bg-accent text-accent-fg text-sm font-semibold">
            {(user?.name || user?.username || "?").charAt(0).toUpperCase()}
          </div>
          <div className="min-w-0 flex-1">
            <div className="truncate text-sm text-fg">{user?.name || user?.username}</div>
            <div className="truncate text-xs text-fg-subtle capitalize">{role}</div>
          </div>
          <button
            type="button"
            onClick={handleLogout}
            className="flex items-center gap-1.5 rounded-md px-3 py-2 text-sm text-fg-muted hover:bg-elevated hover:text-danger"
          >
            <LogOut size={16} />
            Log out
          </button>
        </div>
      </div>
    </div>
  );
}
