// Mobile bottom tab bar (< md). Shows the primary daily destinations plus a
// "More" tab that opens the full-nav sheet. Sits above the safe-area inset so it
// clears the phone's home indicator. Hidden at md+.
import { MoreHorizontal } from "lucide-react";
import { useUi, currentRoute } from "../../stores/ui";
import { useEffectiveRole } from "../../hooks/useEffectiveRole";
import { canAccess } from "../../lib/permissions";
import { BOTTOM_BAR_ROUTES, navItem } from "../../app/navigation";
import { cn } from "../ui/cn";

export function BottomBar() {
  const navigate = useUi((s) => s.navigate);
  const openSheet = useUi((s) => s.openSheet);
  const active = useUi(currentRoute);
  const role = useEffectiveRole();

  const tabs = BOTTOM_BAR_ROUTES.map(navItem)
    .filter((i): i is NonNullable<typeof i> => Boolean(i))
    .filter((i) => canAccess(i.route, role));

  return (
    <nav
      className="flex shrink-0 items-stretch border-t border-border bg-surface md:hidden"
      style={{ paddingBottom: "env(safe-area-inset-bottom)" }}
    >
      {tabs.map(({ route, label, icon: Icon }) => {
        const isActive = route === active;
        return (
          <button
            key={route}
            type="button"
            onClick={() => navigate(route)}
            className={cn(
              "flex flex-1 flex-col items-center justify-center gap-0.5 py-2 text-xs",
              isActive ? "text-accent-hover" : "text-fg-muted",
            )}
          >
            <Icon size={20} />
            <span className="truncate">{label}</span>
          </button>
        );
      })}
      <button
        type="button"
        onClick={() => openSheet("nav")}
        className="flex flex-1 flex-col items-center justify-center gap-0.5 py-2 text-xs text-fg-muted"
      >
        <MoreHorizontal size={20} />
        <span>More</span>
      </button>
    </nav>
  );
}
