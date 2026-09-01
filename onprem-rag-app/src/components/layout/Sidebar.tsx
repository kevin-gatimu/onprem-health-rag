// Desktop / tablet navigation. One component covers both compositions:
//   md .. xl : a fixed 60px icon rail (labels hidden, tooltips via title)
//   xl+      : a 240px labelled sidebar with section headers, collapsible to 60px
// Hidden entirely below md — phones use TopBar + BottomBar + NavSheet instead.
import { Activity, ChevronLeft, ChevronRight, LogOut } from "lucide-react";
import { useUi, currentRoute } from "../../stores/ui";
import { useSession } from "../../stores/session";
import { useEffectiveRole } from "../../hooks/useEffectiveRole";
import { canAccess } from "../../lib/permissions";
import { NAV_ITEMS, NAV_SECTIONS } from "../../app/navigation";
import { logout } from "../../lib/bridge";
import { queryClient } from "../../lib/queryClient";
import { cn } from "../ui/cn";

export function Sidebar() {
  const collapsed = useUi((s) => s.sidebarCollapsed);
  const toggle = useUi((s) => s.toggleSidebar);
  const navigate = useUi((s) => s.navigate);
  const active = useUi(currentRoute);
  const role = useEffectiveRole();
  const user = useSession((s) => s.user);
  const clearUser = useSession((s) => s.clearUser);

  // Labels/section headers render only in the expanded xl sidebar. Below xl the
  // component is always a rail regardless of the persisted collapsed flag.
  const labelCls = collapsed ? "hidden" : "hidden xl:inline";
  const items = NAV_ITEMS.filter((i) => canAccess(i.route, role));

  async function handleLogout() {
    try {
      await logout();
    } finally {
      clearUser();
      // Drop the React Query cache — it holds persisted chat messages (PHI).
      // Never leave transcripts around for the next signed-in user.
      queryClient.clear();
    }
  }

  return (
    <aside
      className={cn(
        "hidden md:flex flex-col shrink-0 border-r border-border bg-surface",
        "w-[60px]",
        collapsed ? "xl:w-[60px]" : "xl:w-60",
      )}
    >
      {/* Brand */}
      <div className="flex h-14 items-center gap-2 px-3 border-b border-border">
        <Activity size={22} className="text-accent shrink-0" />
        <span className={cn("font-semibold text-fg truncate", labelCls)}>Health RAG</span>
      </div>

      {/* Nav */}
      <nav className="flex-1 overflow-y-auto py-2">
        {NAV_SECTIONS.map((section) => {
          const sectionItems = items.filter((i) => i.section === section);
          if (sectionItems.length === 0) return null;
          return (
            <div key={section} className="mb-2">
              <div
                className={cn(
                  "px-4 pt-2 pb-1 text-xs uppercase tracking-wide text-fg-subtle",
                  collapsed ? "hidden" : "hidden xl:block",
                )}
              >
                {section}
              </div>
              {sectionItems.map(({ route, label, icon: Icon }) => {
                const isActive = route === active;
                return (
                  <button
                    key={route}
                    type="button"
                    title={label}
                    onClick={() => navigate(route)}
                    className={cn(
                      "flex w-full items-center gap-3 px-4 py-2 text-sm transition-colors",
                      "justify-center xl:justify-start",
                      collapsed && "xl:justify-center",
                      isActive
                        ? "bg-accent-subtle text-accent-hover"
                        : "text-fg-muted hover:bg-elevated hover:text-fg",
                    )}
                  >
                    <Icon size={18} className="shrink-0" />
                    <span className={labelCls}>{label}</span>
                  </button>
                );
              })}
            </div>
          );
        })}
      </nav>

      {/* Footer: user + logout + collapse toggle */}
      <div className="border-t border-border p-2">
        <div className="flex items-center gap-2 px-2 py-1">
          <div className="flex h-7 w-7 shrink-0 items-center justify-center rounded-full bg-accent text-accent-fg text-xs font-semibold">
            {(user?.name || user?.username || "?").charAt(0).toUpperCase()}
          </div>
          <div className={cn("min-w-0 flex-1", labelCls)}>
            <div className="truncate text-sm text-fg">{user?.name || user?.username}</div>
            <div className="truncate text-xs text-fg-subtle capitalize">{role}</div>
          </div>
        </div>
        {/* Logout: always reachable — an icon in the rail/collapsed states, icon +
            label in the expanded xl sidebar (mirrors the nav-item alignment). */}
        <button
          type="button"
          title="Log out"
          onClick={handleLogout}
          className={cn(
            "mt-1 flex w-full items-center gap-3 rounded-md px-3 py-2 text-sm transition-colors",
            "text-fg-muted hover:bg-elevated hover:text-danger",
            "justify-center xl:justify-start",
            collapsed && "xl:justify-center",
          )}
        >
          <LogOut size={16} className="shrink-0" />
          <span className={labelCls}>Log out</span>
        </button>
        <button
          type="button"
          onClick={toggle}
          title={collapsed ? "Expand" : "Collapse"}
          className="mt-1 hidden w-full items-center justify-center rounded-md p-1.5 text-fg-muted hover:bg-elevated hover:text-fg xl:flex"
        >
          {collapsed ? <ChevronRight size={16} /> : <ChevronLeft size={16} />}
        </button>
      </div>
    </aside>
  );
}
