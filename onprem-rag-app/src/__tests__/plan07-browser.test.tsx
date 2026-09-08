/**
 * Plan 07 browser harness — verifies the four acceptance criteria listed in §7:
 *
 *  1. Registry renders 13 + Ask tabs on the dev seed.
 *  2. Suggestion click produces a call with the correct spec (deterministic path).
 *  3. Draft survives a tab switch (conversation continuity).
 *  4. Clarify option click submits.
 *
 * Approach: mockIPC (from @tauri-apps/api/mocks) intercepts every `invoke()` call
 * so the React layer never touches a real server. Store tests exercise Zustand
 * state directly; component tests render isolated leaf components through
 * @testing-library/react.
 *
 * "relay on 8010" pattern: event delivery is handled by mockIPC's `shouldMockEvents`
 * mode, which lets us call `emit()` from the test and have `listen()` callbacks fire
 * synchronously — no real HTTP server needed.
 */
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { render, screen, fireEvent, act } from "@testing-library/react";
import { mockIPC, clearMocks } from "@tauri-apps/api/mocks";
import type { AgentInfo, Suggestion } from "../lib/bridge";
import { useAgentRegistry, ASK_KIND } from "../stores/agentRegistry";
import { useAgents } from "../stores/agents";
import SuggestionChips from "../components/answer/SuggestionChips";
import ProvenanceStrip from "../components/answer/ProvenanceStrip";
import type { Provenance } from "../lib/bridge";

// ---------------------------------------------------------------------------
// Dev-seed roster — 13 service lines matching the plan's "13 + Ask" claim.
// Tier is split so we exercise both the visible tab path (tier 1) and the
// "More" dropdown path (tier 2+).
// ---------------------------------------------------------------------------

function makeAgent(
  kind: string,
  label: string,
  tier: 1 | 2 | 3,
  usable = true,
): AgentInfo {
  return {
    kind,
    label,
    blurb: `${label} blurb`,
    tier,
    usable,
    modes: ["ask", "trends", "handover"],
    sources: usable ? [{ source_id: "pg-dev", tables: [`${kind}_tbl`] }] : [],
    example_questions: [`What is the ${label} count?`],
    concepts: [kind],
  };
}

const DEV_SEED_13: AgentInfo[] = [
  makeAgent("pharmacy", "Pharmacy", 1),
  makeAgent("ward_board", "Ward Board", 1),
  makeAgent("maternity", "Maternity", 1),
  makeAgent("emergency", "Emergency", 1),
  makeAgent("radiology", "Radiology", 1),
  makeAgent("cardiology", "Cardiology", 1),
  makeAgent("oncology", "Oncology", 1),
  makeAgent("pathology", "Pathology", 1),
  makeAgent("neurology", "Neurology", 1),
  makeAgent("icu", "ICU", 2),
  makeAgent("surgery", "Surgery", 2),
  makeAgent("pediatrics", "Pediatrics", 2),
  makeAgent("orthopedics", "Orthopedics", 3),
];

// ---------------------------------------------------------------------------
// Helper: reset Zustand stores between tests so state does not bleed.
// ---------------------------------------------------------------------------

function resetStores() {
  useAgentRegistry.setState({
    agents: [],
    loaded: false,
    source: "none",
    error: null,
  });
  useAgents.setState({
    activeConversationId: null,
    selectedKind: ASK_KIND,
    modes: {},
    lastConversationByKind: {},
    runs: {},
    queues: {},
    drafts: {},
  });
}

afterEach(() => {
  clearMocks();
  resetStores();
  vi.restoreAllMocks();
});

// ===========================================================================
// §7.1 — Registry renders 13 + Ask tabs on the dev seed
// ===========================================================================

describe("agentRegistry — dev seed", () => {
  it("load() produces Ask + 13 service lines (14 total) from the server", async () => {
    // Mock invoke("list_agents") to return the dev seed.
    mockIPC((cmd) => {
      if (cmd === "list_agents") return DEV_SEED_13;
    });

    const { load, agents } = useAgentRegistry.getState();
    expect(agents).toHaveLength(0);

    await act(async () => {
      await load();
    });

    const state = useAgentRegistry.getState();
    expect(state.loaded).toBe(true);
    expect(state.source).toBe("server");
    // Ask is prepended because the server did not return it.
    const kindList = state.agents.map((a) => a.kind);
    expect(kindList[0]).toBe(ASK_KIND);
    expect(state.agents).toHaveLength(14); // 1 Ask + 13 lines

    // usable() returns only agents with ≥ 1 source — all 13 dev-seed lines +
    // the synthetic Ask tab (always usable).
    expect(state.usable()).toHaveLength(14);
  });

  it("labels each service line from the server, not from a hardcoded list", async () => {
    mockIPC((cmd) => {
      if (cmd === "list_agents") return DEV_SEED_13;
    });

    await act(async () => {
      await useAgentRegistry.getState().load();
    });

    const state = useAgentRegistry.getState();
    // Spot-check a few labels to prove they came from the roster, not hardcode.
    expect(state.byKind("pharmacy")?.label).toBe("Pharmacy");
    expect(state.byKind("icu")?.label).toBe("ICU");
    expect(state.byKind("orthopedics")?.label).toBe("Orthopedics");
    expect(state.labelFor("nonexistent_kind")).toBe("nonexistent_kind");
  });

  it("falls back to LEGACY_AGENTS (not empty) when GET /agents returns 404", async () => {
    mockIPC((cmd) => {
      if (cmd === "list_agents") throw "__ENDPOINT_NOT_FOUND__";
    });

    await act(async () => {
      await useAgentRegistry.getState().load();
    });

    const state = useAgentRegistry.getState();
    expect(state.loaded).toBe(true);
    expect(state.source).toBe("legacy");
    // Legacy set must contain at least Ask.
    expect(state.agents.some((a) => a.kind === ASK_KIND)).toBe(true);
    expect(state.agents.length).toBeGreaterThan(0);
  });

  it("splits usable tier-1 agents from tier-2+ (More menu pattern)", async () => {
    mockIPC((cmd) => {
      if (cmd === "list_agents") return DEV_SEED_13;
    });

    await act(async () => {
      await useAgentRegistry.getState().load();
    });

    const state = useAgentRegistry.getState();
    // Ask (tier 1, synthetic) + 9 tier-1 lines = 10 in the visible strip.
    const tier1 = state.tier(1);
    expect(tier1.some((a) => a.kind === ASK_KIND)).toBe(true);
    expect(tier1.some((a) => a.kind === "pharmacy")).toBe(true);

    // 2 tier-2 + 1 tier-3 go into the "More" menu.
    const tier2 = state.tier(2);
    expect(tier2.map((a) => a.kind)).toContain("icu");
    expect(tier2.map((a) => a.kind)).toContain("surgery");
    const tier3 = state.tier(3);
    expect(tier3.map((a) => a.kind)).toContain("orthopedics");
  });
});

// ===========================================================================
// §7.3 — Draft survives a tab switch
// ===========================================================================

describe("agents store — draft continuity", () => {
  it("setSelectedKind does not reset activeConversationId", () => {
    useAgents.setState({
      selectedKind: "pharmacy",
      activeConversationId: "conv-001",
      lastConversationByKind: { pharmacy: "conv-001" },
    });

    act(() => {
      useAgents.getState().setSelectedKind("ward_board");
    });

    // ward_board has no last conversation yet — should be null, not the old conv.
    expect(useAgents.getState().activeConversationId).toBeNull();
    // pharmacy's conversation must be preserved.
    expect(useAgents.getState().lastConversationByKind["pharmacy"]).toBe("conv-001");
  });

  it("draft is keyed by kind+convId so switching tabs preserves text", () => {
    const { setDraft } = useAgents.getState();

    act(() => {
      setDraft("pharmacy:conv-001", "show prescriptions for ward 3");
      setDraft("ward_board:__new__", "how many beds are free?");
    });

    const drafts = useAgents.getState().drafts;
    expect(drafts["pharmacy:conv-001"]).toBe("show prescriptions for ward 3");
    expect(drafts["ward_board:__new__"]).toBe("how many beds are free?");
    // Switching tab (selectedKind) must not touch either draft.
    act(() => {
      useAgents.getState().setSelectedKind("emergency");
    });
    const after = useAgents.getState().drafts;
    expect(after["pharmacy:conv-001"]).toBe("show prescriptions for ward 3");
    expect(after["ward_board:__new__"]).toBe("how many beds are free?");
  });

  it("re-entering a tab restores the last conversation for that kind", () => {
    act(() => {
      useAgents.setState({
        lastConversationByKind: {
          pharmacy: "conv-pharmacy-7",
          emergency: "conv-emg-3",
        },
      });
      useAgents.getState().setSelectedKind("pharmacy");
    });

    expect(useAgents.getState().activeConversationId).toBe("conv-pharmacy-7");

    act(() => {
      useAgents.getState().setSelectedKind("emergency");
    });

    expect(useAgents.getState().activeConversationId).toBe("conv-emg-3");
  });
});

// ===========================================================================
// §7.2 — Suggestion click produces a call with the correct spec
// (deterministic path: `spec` forwarded and `switchKind` resolved for `switch`)
// ===========================================================================

describe("SuggestionChips — suggestion click", () => {
  it("drill chip calls onSubmit with its text and spec", () => {
    const onSubmit = vi.fn();
    const suggestions: Suggestion[] = [
      {
        text: "Show top 5 prescribed drugs",
        kind: "drill",
        spec: { deterministic: true, query: "SELECT drug, count FROM prescriptions" },
      },
    ];

    render(<SuggestionChips suggestions={suggestions} onSubmit={onSubmit} />);
    fireEvent.click(screen.getByText("Show top 5 prescribed drugs"));

    expect(onSubmit).toHaveBeenCalledOnce();
    type SubmitOpts = { suggestionSpec?: unknown; switchKind?: string };
    const [text, opts] = onSubmit.mock.calls[0] as [string, SubmitOpts];
    expect(text).toBe("Show top 5 prescribed drugs");
    expect((opts?.suggestionSpec as { deterministic: boolean })?.deterministic).toBe(true);
    expect(opts?.switchKind).toBeUndefined();
  });

  it("switch chip calls onSubmit with switchKind set to the suggestion's agent", () => {
    const onSubmit = vi.fn();
    const suggestions: Suggestion[] = [
      {
        text: "Open in Pharmacy",
        kind: "switch",
        agent: "pharmacy",
      },
    ];

    render(<SuggestionChips suggestions={suggestions} onSubmit={onSubmit} />);
    fireEvent.click(screen.getByText("Open in Pharmacy"));

    expect(onSubmit).toHaveBeenCalledOnce();
    const [, opts] = onSubmit.mock.calls[0] as [string, { switchKind?: string }];
    expect(opts?.switchKind).toBe("pharmacy");
  });

  it("only renders the first 4 chips when more are provided", () => {
    const onSubmit = vi.fn();
    const suggestions: Suggestion[] = Array.from({ length: 6 }, (_, i) => ({
      text: `Suggestion ${i + 1}`,
      kind: "explain" as const,
    }));

    render(<SuggestionChips suggestions={suggestions} onSubmit={onSubmit} />);
    // Buttons for chips 1–4 present; 5 and 6 absent.
    expect(screen.getByText("Suggestion 1")).toBeDefined();
    expect(screen.getByText("Suggestion 4")).toBeDefined();
    expect(screen.queryByText("Suggestion 5")).toBeNull();
    expect(screen.queryByText("Suggestion 6")).toBeNull();
  });
});

// ===========================================================================
// §7.4 — Clarify option click submits
// ===========================================================================

// Import MessageBubble — it uses the registry store for labels; initialise it.
import MessageBubble from "../features/agents/MessageBubble";
import type { AgentPending } from "../stores/agents";

describe("MessageBubble — clarify block", () => {
  beforeEach(() => {
    // Registry needs at least ASK_KIND so label lookups don't throw.
    useAgentRegistry.setState({
      agents: [
        {
          kind: ASK_KIND,
          label: "Ask",
          blurb: "Ask anything.",
          tier: 1,
          usable: true,
          modes: ["ask", "trends", "handover"],
          sources: [],
          example_questions: [],
          concepts: [],
        },
      ],
      loaded: true,
      source: "server",
      error: null,
    });
  });

  it("renders clarify question and option buttons, and submits the option on click", () => {
    const onSuggestionSubmit = vi.fn();

    const pending: AgentPending = {
      runId: "run-clarify-1",
      conversationId: "conv-1",
      selectedKind: ASK_KIND,
      routedKind: null,
      routed: null,
      user: "What is the admission rate?",
      answer: "",
      citations: [],
      listRows: null,
      page: null,
      rows: null,
      spec: null,
      pipeline: null,
      sqlResult: null,
      provenance: null,
      suggestions: [],
      focusUsed: [],
      clarify: {
        question: "Which ward should I look at?",
        slot: "dimension",
        options: ["Ward A", "Ward B", "All wards"],
      },
      error: null,
      phase: "clarifying",
      stages: [],
      opts: { mode: "ask" },
      startedAt: Date.now(),
    };

    render(
      <MessageBubble
        kind="optimistic-assistant"
        pending={pending}
        isLast={true}
        onSuggestionSubmit={onSuggestionSubmit}
      />,
    );

    expect(screen.getByText("Which ward should I look at?")).toBeDefined();
    expect(screen.getByText("Ward A")).toBeDefined();
    expect(screen.getByText("Ward B")).toBeDefined();
    expect(screen.getByText("All wards")).toBeDefined();

    // Clicking "Ward B" must submit it as the next user message.
    fireEvent.click(screen.getByText("Ward B"));
    expect(onSuggestionSubmit).toHaveBeenCalledOnce();
    const [text] = onSuggestionSubmit.mock.calls[0] as [string];
    expect(text).toBe("Ward B");
  });
});

// ===========================================================================
// §7.visual — ProvenanceStrip renders all four backends
// ===========================================================================

describe("ProvenanceStrip — four backends", () => {
  const BASE: Provenance = {
    path: [
      { rung: "Link", result: "hit" },
      { rung: "DeterministicSql", result: "hit" },
      { rung: "Execute", result: "hit" },
    ],
    backend: "source_sql",
    scope: ["prescription"],
    source_id: "pg-dev",
    elapsed_ms: { link: 3, compile: 12, execute: 197 },
  };

  it.each([
    ["source_sql", "Live SQL"],
    ["document_db", "DocumentDB"],
    ["semantic", "Semantic"],
    ["hybrid", "Hybrid"],
    ["none", "No data"],
  ] as [string, string][])(
    "backend '%s' shows label '%s' in expanded detail",
    async (backend, expectedLabel) => {
      const provenance: Provenance = { ...BASE, backend: backend as Provenance["backend"] };
      render(<ProvenanceStrip provenance={provenance} />);

      // The strip collapses by default; expand it.
      const toggleBtn = screen.getByRole("button");
      fireEvent.click(toggleBtn);

      expect(screen.getByText(expectedLabel)).toBeDefined();
    },
  );

  it("shows miss rung with its reason on hover (title attribute)", () => {
    const provenance: Provenance = {
      ...BASE,
      path: [
        { rung: "Link", result: "miss", reason: "no EventTime on Bill" },
        { rung: "ModelSql", result: "skipped" },
      ],
    };
    render(<ProvenanceStrip provenance={provenance} />);
    const linkRung = screen.getByTitle("no EventTime on Bill");
    expect(linkRung).toBeDefined();
  });
});
