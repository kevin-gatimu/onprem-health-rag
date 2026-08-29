// Ingestion wizard store — step machine + progress that survives navigation.
//
// State lives here (not in a component) so leaving /ingest mid-run and coming
// back shows the live view rather than starting over. The bridgeEvents listener
// (registered once at boot) fans ingest://progress events into `applyProgress`.
import { create } from 'zustand';
import type { TableSchema, SchemaAnalysis, IngestProgress } from '../lib/bridge';

export type IngestStep =
  | 'pick-connection'
  | 'loading-schema'
  | 'analyzing'
  | 'select-tables'
  | 'ingesting'
  | 'complete';

export interface TableSelection {
  selected: boolean;
  /** Column names the user chose to drop (not embedded or stored). */
  excluded_columns: string[];
  expanded: boolean;
}

interface IngestionState {
  step: IngestStep;
  sourceId: string | null;
  schema: TableSchema[];
  analysis: SchemaAnalysis | null;
  analysisError: string | null;
  selections: Record<string, TableSelection>;
  progress: IngestProgress | null;

  // Actions
  pickSource: (id: string) => void;
  setStep: (step: IngestStep) => void;
  /** Set schema tables and seed selections (all selected, no exclusions). */
  setSchema: (tables: TableSchema[]) => void;
  /** Set analysis result; if suggested_tables is non-empty, flip selections to match. */
  setAnalysis: (a: SchemaAnalysis) => void;
  setAnalysisError: (msg: string | null) => void;
  toggleTable: (name: string) => void;
  toggleColumn: (table: string, col: string) => void;
  toggleExpand: (name: string) => void;
  selectAll: (value: boolean) => void;
  /** Apply a progress snapshot; flips step → 'complete' on any terminal status. */
  applyProgress: (p: IngestProgress) => void;
  /** Called immediately before startIngest: flip step → 'ingesting', clear progress. */
  startJob: () => void;
  /** Force step → 'complete' (used when ingest://done arrives without a payload). */
  markComplete: () => void;
  resetWizard: () => void;
}

export const useIngestion = create<IngestionState>((set) => ({
  step: 'pick-connection',
  sourceId: null,
  schema: [],
  analysis: null,
  analysisError: null,
  selections: {},
  progress: null,

  pickSource: (id) => set({ sourceId: id }),
  setStep: (step) => set({ step }),

  setSchema: (tables) => {
    const selections: Record<string, TableSelection> = {};
    for (const t of tables) {
      selections[t.name] = { selected: true, excluded_columns: [], expanded: false };
    }
    set({ schema: tables, selections });
  },

  setAnalysis: (a) =>
    set((s) => {
      // If AI returned suggested tables, flip each selection to match.
      if (a.suggested_tables.length > 0) {
        const selections: Record<string, TableSelection> = {};
        for (const [name, sel] of Object.entries(s.selections)) {
          selections[name] = { ...sel, selected: a.suggested_tables.includes(name) };
        }
        return { analysis: a, selections };
      }
      return { analysis: a };
    }),

  setAnalysisError: (msg) => set({ analysisError: msg }),

  toggleTable: (name) =>
    set((s) => {
      const sel = s.selections[name];
      if (!sel) return s;
      return {
        selections: { ...s.selections, [name]: { ...sel, selected: !sel.selected } },
      };
    }),

  toggleColumn: (table, col) =>
    set((s) => {
      const t = s.selections[table];
      if (!t) return s;
      const has = t.excluded_columns.includes(col);
      return {
        selections: {
          ...s.selections,
          [table]: {
            ...t,
            excluded_columns: has
              ? t.excluded_columns.filter((c) => c !== col)
              : [...t.excluded_columns, col],
          },
        },
      };
    }),

  toggleExpand: (name) =>
    set((s) => {
      const sel = s.selections[name];
      if (!sel) return s;
      return {
        selections: { ...s.selections, [name]: { ...sel, expanded: !sel.expanded } },
      };
    }),

  selectAll: (value) =>
    set((s) => {
      const selections: Record<string, TableSelection> = {};
      for (const [k, sel] of Object.entries(s.selections)) {
        selections[k] = { ...sel, selected: value };
      }
      return { selections };
    }),

  applyProgress: (p) =>
    set((s) => ({
      progress: p,
      step:
        p.status === 'completed' || p.status === 'partial' || p.status === 'failed'
          ? 'complete'
          : s.step,
    })),

  startJob: () => set({ step: 'ingesting', progress: null }),

  markComplete: () => set({ step: 'complete' }),

  resetWizard: () =>
    set({
      step: 'pick-connection',
      sourceId: null,
      schema: [],
      analysis: null,
      analysisError: null,
      selections: {},
      progress: null,
    }),
}));
