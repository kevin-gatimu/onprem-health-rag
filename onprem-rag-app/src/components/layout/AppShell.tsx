// The authenticated application frame. Composes the three responsive navigation
// surfaces around the routed content:
//   < md : TopBar + BottomBar + NavSheet (single column)
//   md+  : Sidebar (60px rail, expandable to 240px at xl)
// Enforces route access against the effective role — an admin previewing another
// role is bounced from routes that role can't reach, just like that role would be.
import { Suspense } from "react";
import { ShieldAlert } from "lucide-react";
import { Sidebar } from "./Sidebar";
import { TopBar } from "./TopBar";
import { BottomBar } from "./BottomBar";
import { NavSheet } from "./NavSheet";
import { renderRoute } from "../../app/routes";
import { EmptyState } from "../ui";
import { useUi, currentRoute } from "../../stores/ui";
import { useEffectiveRole } from "../../hooks/useEffectiveRole";
import { canAccess } from "../../lib/permissions";

export function AppShell() {
  const route = useUi(currentRoute);
  const role = useEffectiveRole();
  const allowed = role != null && canAccess(route, role);

  return (
    <div className="flex h-full w-full overflow-hidden bg-base text-fg">
      <Sidebar />

      <div className="flex min-w-0 flex-1 flex-col">
        <TopBar />

        <main className="flex-1 overflow-y-auto p-4 pb-4 md:p-6 lg:p-8 xl:p-12">
          {allowed ? (
            <Suspense fallback={null}>{renderRoute(route)}</Suspense>
          ) : (
            <EmptyState
              icon={<ShieldAlert size={32} />}
              title="No access"
              description="Your role doesn't have permission to view this screen."
            />
          )}
        </main>

        <BottomBar />
      </div>

      <NavSheet />
    </div>
  );
}
