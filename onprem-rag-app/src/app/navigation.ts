// Navigation metadata — the single source for the sidebar, the md: icon rail, and
// the mobile "More" sheet. The bottom tab bar picks a subset (see layout/BottomBar).
// Access is enforced separately by lib/permissions.ts; the shell filters these
// items through canAccess(route, effectiveRole).
import {
  LayoutDashboard,
  Database,
  Download,
  FolderOpen,
  MessageSquare,
  Cpu,
  Boxes,
  BarChart3,
  Bell,
  ClipboardList,
  Settings,
  User,
  ShieldCheck,
  type LucideIcon,
} from "lucide-react";
import type { Route } from "../lib/types";

export type NavSection = "Main" | "Analytics" | "System";

export interface NavItem {
  route: Route;
  label: string;
  icon: LucideIcon;
  section: NavSection;
}

/** The 13 navigable destinations, in display order, grouped into three sections. */
export const NAV_ITEMS: NavItem[] = [
  // Main
  { route: "/", label: "Dashboard", icon: LayoutDashboard, section: "Main" },
  { route: "/connections", label: "Connections", icon: Database, section: "Main" },
  { route: "/ingest", label: "Ingest", icon: Download, section: "Main" },
  { route: "/data", label: "Data Explorer", icon: FolderOpen, section: "Main" },
  { route: "/chat", label: "AI Chat", icon: MessageSquare, section: "Main" },
  { route: "/agents", label: "AI Agents", icon: Cpu, section: "Main" },
  { route: "/models", label: "Models", icon: Boxes, section: "Main" },
  // Analytics
  { route: "/analytics", label: "Analytics", icon: BarChart3, section: "Analytics" },
  { route: "/alerts", label: "Outbreak Alerts", icon: Bell, section: "Analytics" },
  // System
  { route: "/audit", label: "Audit Log", icon: ClipboardList, section: "System" },
  { route: "/settings", label: "Settings", icon: Settings, section: "System" },
  { route: "/profile", label: "Profile", icon: User, section: "System" },
  { route: "/admin", label: "Admin", icon: ShieldCheck, section: "System" },
];

/** Section display order for the expanded sidebar. */
export const NAV_SECTIONS: NavSection[] = ["Main", "Analytics", "System"];

/**
 * Routes surfaced in the phone bottom tab bar (the rest live behind "More").
 * Chosen for the primary daily tasks; "More" opens the full NAV_ITEMS sheet.
 */
export const BOTTOM_BAR_ROUTES: Route[] = ["/", "/data", "/chat", "/agents"];

/** Look up an item's metadata by route (e.g. to title the current page). */
export function navItem(route: Route): NavItem | undefined {
  return NAV_ITEMS.find((i) => i.route === route);
}
