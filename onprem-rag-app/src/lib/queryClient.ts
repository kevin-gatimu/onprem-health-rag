// Single TanStack Query client for all server reads. Defaults chosen to replace
// the reference's hand-rolled SWR stores (`ensureXLoaded(maxAgeMs = 30_000)`):
// a 30s stale window, no refetch-on-focus (this is a desktop/embedded app, not a
// browser tab the user leaves and returns to), and one retry.
import { QueryClient } from "@tanstack/react-query";

export const queryClient = new QueryClient({
  defaultOptions: {
    queries: {
      staleTime: 30_000,
      refetchOnWindowFocus: false,
      retry: 1,
    },
  },
});
