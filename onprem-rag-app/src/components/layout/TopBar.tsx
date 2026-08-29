// Mobile app bar (< md). Shows the brand + current screen title on the left and a
// menu button that opens the full-nav sheet. Hidden at md+ where the rail/sidebar
// carries navigation.
import { Activity, Menu } from "lucide-react";
import { useUi, currentRoute } from "../../stores/ui";
import { navItem } from "../../app/navigation";

export function TopBar() {
  const openSheet = useUi((s) => s.openSheet);
  const active = useUi(currentRoute);
  const item = navItem(active);

  return (
    <header className="flex h-14 shrink-0 items-center gap-3 border-b border-border bg-surface px-4 md:hidden">
      <Activity size={20} className="text-accent shrink-0" />
      <span className="min-w-0 flex-1 truncate font-semibold text-fg">
        {item?.label ?? "Health RAG"}
      </span>
      <button
        type="button"
        aria-label="Open navigation"
        onClick={() => openSheet("nav")}
        className="rounded-md p-2 text-fg-muted hover:bg-elevated hover:text-fg"
      >
        <Menu size={20} />
      </button>
    </header>
  );
}
