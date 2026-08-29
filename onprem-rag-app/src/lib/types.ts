// Shared client types. Server-facing shapes live in bridge.ts; this file holds
// the app's own domain types (roles, routes, nav) used across stores and UI.

/** The four roles the server issues. There is no `readonly` role. */
export type Role = "admin" | "doctor" | "nurse" | "analyst";

/** Public user identity as returned by the bridge (mirrors the server UserInfo). */
export interface User {
  id: string;
  username: string;
  email: string;
  name: string;
  role: Role;
  /** RFC3339 creation timestamp. Backs the Admin "Created" column and Profile "Member since". */
  created_at: string;
}

/**
 * App routes. These double as permission keys (see permissions.ts) and as the
 * entries in the `ui` store's nav stack — the app has no URL router, navigation
 * is a stack in state so Android's hardware back button can pop it.
 */
export type Route =
  | "/"
  | "/connections"
  | "/ingest"
  | "/data"
  | "/chat"
  | "/agents"
  | "/models"
  | "/analytics"
  | "/alerts"
  | "/audit"
  | "/settings"
  | "/admin"
  | "/profile";
