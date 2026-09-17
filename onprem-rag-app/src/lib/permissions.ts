// Route-level permission matrix, ported from the reference's shared/permissions.ts
// with the `readonly` role removed. One matrix drives BOTH the route guard and the
// sidebar filtering, so access rules never drift between them.
import type { Role, Route } from "./types";

const ALL: Role[] = ["admin", "doctor", "nurse", "analyst"];

/**
 * Each route maps to the roles that may access it. With `readonly` gone, the
 * reference's "everyone except readonly" tier collapses to simply "all roles".
 */
export const ROUTE_PERMISSIONS: Record<Route, Role[]> = {
  "/": ALL,
  "/connections": ["admin"],
  "/ingest": ["admin"],
  "/audit": ["admin"],
  "/admin": ["admin"],
  "/agents": ALL,
  "/analytics": ["admin", "doctor", "analyst"],
  "/alerts": ["admin", "doctor", "analyst"],
  "/data": ALL,
  "/chat": ALL,
  "/models": ALL,
  "/settings": ALL,
  "/profile": ALL,
};

/** True if `role` may access `route`. A null role (logged out) is denied. */
export function canAccess(route: Route, role: Role | null | undefined): boolean {
  if (!role) return false;
  return ROUTE_PERMISSIONS[route].includes(role);
}
