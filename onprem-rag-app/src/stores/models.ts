// Model download store — per-variant download progress that survives navigation.
//
// State lives here (not in VariantCard) so leaving Models mid-download and coming
// back shows the live progress bar rather than nothing. The boot-time listeners in
// bridgeEvents.ts fan `model://progress|status` events into `applyProgress` /
// `applyStatus` keyed by variant id; `startDownload` owns the whole lifecycle
// (invoke → toast → query invalidation) so completion is handled even when the
// Models screen is unmounted. Keyed entries also mean two concurrent downloads
// can't cross-wire each other's bars.
import { create } from 'zustand';
import { pullModel, selectModel } from '../lib/bridge';
import { notifyBackground } from '../lib/notify';
import { queryClient } from '../lib/queryClient';
import { toast } from './ui';

export interface DownloadEntry {
  /** 0..100 as reported by the server's progress events. */
  pct: number;
  /** Last status transition (e.g. "downloading", "verifying"); '' until one arrives. */
  status: string;
}

interface ModelsState {
  /** In-flight downloads keyed by variant id. Absent key = not downloading. */
  downloads: Record<string, DownloadEntry>;
  /** Variant id of the ONE load in flight, or null. Loads are single-flight:
   *  concurrent native loads segfault the server's Foundry core. */
  loadingId: string | null;

  applyProgress: (variantId: string, pct: number) => void;
  applyStatus: (variantId: string, status: string) => void;
  /**
   * Kick off a weights-only download (load:false) and own its full lifecycle.
   * No-op if this variant is already downloading. Safe to call from a component
   * that unmounts mid-download — completion toasts + cache invalidation run here.
   */
  startDownload: (variantId: string) => Promise<void>;
  /** Load a variant (select → download-if-needed → load → set current). No-op
   *  while ANY load is in flight, mirroring the server-side load gate. */
  startLoad: (variantId: string) => Promise<void>;
}

export const useModels = create<ModelsState>((set, get) => ({
  downloads: {},
  loadingId: null,

  applyProgress: (variantId, pct) =>
    set((s) => {
      const entry = s.downloads[variantId];
      // Ignore events for variants we're not tracking (stale/foreign streams).
      if (!entry) return s;
      return { downloads: { ...s.downloads, [variantId]: { ...entry, pct } } };
    }),

  applyStatus: (variantId, status) =>
    set((s) => {
      const entry = s.downloads[variantId];
      if (!entry) return s;
      return { downloads: { ...s.downloads, [variantId]: { ...entry, status } } };
    }),

  startDownload: async (variantId) => {
    if (get().downloads[variantId]) return; // already in flight
    set((s) => ({
      downloads: { ...s.downloads, [variantId]: { pct: 0, status: 'starting…' } },
    }));
    try {
      // Resolves on the stream's done event; rejects with the server error payload.
      await pullModel(variantId, false);
      toast.success(`Downloaded ${variantId}.`);
      void notifyBackground('Model download complete', `${variantId} is ready to use.`);
    } catch (err) {
      toast.error(String(err));
      void notifyBackground('Model download failed', `${variantId}: ${String(err)}`);
    } finally {
      set((s) => {
        const { [variantId]: _, ...rest } = s.downloads;
        return { downloads: rest };
      });
      // A finished (or failed) download changes variant flags + cached lists.
      queryClient.invalidateQueries({ queryKey: ['model-roles'] });
      queryClient.invalidateQueries({ queryKey: ['setup-status'] });
    }
  },

  startLoad: async (variantId) => {
    if (get().loadingId) return; // one load at a time
    set({ loadingId: variantId });
    try {
      // selectModel = download-if-needed → load → set current, so it can take a while.
      const { model: resolved, repaired } = await selectModel(variantId);
      if (repaired) {
        toast.info(`${resolved}: cached weights were corrupt — re-downloaded automatically.`);
      }
      toast.success(`Loaded ${resolved}.`);
      void notifyBackground('Model loaded', `${resolved} is ready to use.`);
    } catch (err) {
      toast.error(String(err));
    } finally {
      set({ loadingId: null });
      queryClient.invalidateQueries({ queryKey: ['model-roles'] });
      queryClient.invalidateQueries({ queryKey: ['setup-status'] });
    }
  },
}));
