// src/__tests__/TuneCard.test.tsx — the tune loop card's state machine: start →
// round 1 candidate (moves, A/B, measured, cleared/still), "better" and "not
// better" run the next round with the right decision, "Save this" persists the
// step's CUMULATIVE ops (with the scene overlay) and ends the loop without a
// discard, "Stop & discard" ends it with one, and a converged/exhausted step
// offers "Save what I kept" only when the baseline carries edits.

import { describe, it, expect, vi, beforeEach } from "vitest";
import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";

import { ThemeProvider } from "../theme/ThemeProvider";

vi.mock("../lib/invoke", async (importOriginal) => {
  const actual = await importOriginal<typeof import("../lib/invoke")>();
  return {
    ...actual,
    doctorTuneStep: vi.fn(),
    doctorTuneEnd: vi.fn(),
    doctorSave: vi.fn(),
  };
});

import { doctorSave, doctorTuneEnd, doctorTuneStep } from "../lib/invoke";
import { TuneCard } from "../views/doctor/TuneCard";
import type {
  DoctorDiag,
  DoctorSoundResult,
  DoctorTuneStep,
  GraphNode,
  SceneNodeOverlay,
} from "../lib/types";

const LABELS = ["Lows", "Low-mids", "Mids", "High-mids", "Highs", "Air"];

const MUDDY: DoctorDiag = {
  key: "muddy",
  label: "Muddy",
  sev: "high",
  severity: 3.2,
  bands: [1],
  detail: "buildup around 250–350 Hz (+7.0 dB)",
  explain: "There's a buildup in the low-mids.",
  rx: [],
  fromLevel: "quiet",
};

const DARK: DoctorDiag = {
  key: "dark",
  label: "Dark",
  sev: "med",
  severity: 1.0,
  bands: [4, 5],
  detail: "tilted 4.0 dB/octave darker",
  explain: "The whole tone leans dark.",
  rx: [],
  fromLevel: "rehearsal",
};

const OPS_R1 = [
  {
    kind: "param" as const,
    groupId: "G1",
    nodeId: "amp",
    param: "bass",
    value: 0.4,
  },
];

function stepResult(overrides: Partial<DoctorTuneStep> = {}): DoctorTuneStep {
  return {
    round: 1,
    status: "candidate",
    candidate: {
      moves: [
        {
          groupId: "G1",
          nodeId: "amp",
          model: "ACD_TwinReverb65NoFx",
          blockName: "'65 Twin Reverb",
          param: "bass",
          controlLabel: "Bass",
          unit: "knob",
          from: 0.6,
          to: 0.4,
          fromLabel: "6.0",
          toLabel: "4.0",
        },
      ],
      beforeDb: [0, 7, 0, 0, 0, 0],
      predictedDb: [-1, 2, 0, 0, 0, 0],
      bandLabels: LABELS,
      clears: ["muddy"],
      remains: [{ key: "dark", fromLevel: "stage" }],
      loudnessDeltaDb: -1.2,
      balanceErrorBeforeDb: 4.2,
      balanceErrorAfterDb: 1.1,
      rx: {
        kind: "oneclick",
        title: "Round 1",
        detail: "'65 Twin Reverb: Bass 6.0 → 4.0",
        cpuNote: "no CPU change",
        ops: OPS_R1,
      },
      model: "nominal-tonestack-v1",
    },
    note: { learned: [], excluded: [], cap: 1 },
    ops: OPS_R1,
    baselineOps: [],
    baselineClip: "data:audio/wav;base64,AAAA",
    candidateClip: "data:audio/wav;base64,BBBB",
    measured: {
      bandLabels: LABELS,
      beforeBalanceDb: [0, 7, 0, 0, 0, 0],
      afterBalanceDb: [-1, 2.2, 0, 0, 0, 0],
      deltaDb: [-1, -4.8, 0, 0, 0, 0],
      loudnessDeltaDb: -1.2,
    },
    baselineDiags: [MUDDY, DARK],
    candidateDiags: [DARK],
    cleared: ["muddy"],
    remained: ["dark"],
    introduced: [],
    bandLabels: LABELS,
    baselineBalanceDb: [0, 7, 0, 0, 0, 0],
    candidateBalanceDb: [-1, 2.2, 0, 0, 0, 0],
    message:
      "Round 1: applied '65 Twin Reverb: Bass 6.0 → 4.0 — listen, then say whether it's better.",
    ...overrides,
  };
}

const SOUND: DoctorSoundResult = {
  key: "p0s1",
  listIndex: 0,
  scene: 1,
  footswitch: null,
  label: "Crunch",
  tag: "S2",
  diags: [MUDDY, DARK],
  integratedLufs: -20,
  tailRatioDb: -30,
  balanceDb: [0, 7, 0, 0, 0, 0],
  bandLabels: LABELS,
  cutThrough: null,
  plan: null,
  error: null,
};

const NODES: GraphNode[] = [
  {
    group_id: "G1",
    node_id: "amp",
    model: "ACD_TwinReverb65NoFx",
    bypassed: false,
    params: { bass: 0.6 },
  },
];

const OVERLAY: SceneNodeOverlay[] = [
  { group_id: "G1", node_id: "amp", bypassed: false, params: { bass: 0.6 } },
];

function renderCard() {
  return render(
    <ThemeProvider>
      <TuneCard
        sound={SOUND}
        listIndex={0}
        presetName="Strat (Orange)"
        nodes={NODES}
        footswitches={[]}
        sceneOverlay={OVERLAY}
      />
    </ThemeProvider>,
  );
}

beforeEach(() => {
  vi.mocked(doctorTuneStep).mockReset();
  vi.mocked(doctorTuneEnd).mockReset();
  vi.mocked(doctorSave).mockReset();
  vi.mocked(doctorTuneEnd).mockResolvedValue(undefined);
  vi.mocked(doctorSave).mockResolvedValue(undefined);
});

describe("TuneCard", () => {
  it("starts the loop and renders round 1's candidate, A/B, measurement and findings", async () => {
    const user = userEvent.setup();
    vi.mocked(doctorTuneStep).mockResolvedValue(stepResult());
    renderCard();
    expect(screen.getByText("Search for a better balance")).toBeInTheDocument();
    await user.click(screen.getByRole("button", { name: /start the search/i }));

    expect(doctorTuneStep).toHaveBeenCalledWith(
      expect.objectContaining({
        listIndex: 0,
        name: "Strat (Orange)",
        scene: 1,
        footswitch: null,
        nodes: NODES,
        sceneOverlay: OVERLAY,
        ops: [],
      }),
      "start",
    );
    expect(await screen.findByText("Round 1")).toBeInTheDocument();
    expect(screen.getByTestId("plan-moves")).toHaveTextContent(
      "Bass 6.0 → 4.0",
    );
    expect(screen.getByText("Listen & compare")).toBeInTheDocument();
    expect(screen.getByTestId("measured-change")).toHaveTextContent(
      "Low-mids −4.8",
    );
    expect(screen.getByText("Cleared")).toBeInTheDocument();
    expect(screen.getByText("Still")).toBeInTheDocument();
    expect(
      screen.getByText("Applied to the unit · not saved"),
    ).toBeInTheDocument();
  });

  it("'better' and 'not better' run the next round with that decision and keep a history", async () => {
    const user = userEvent.setup();
    vi.mocked(doctorTuneStep)
      .mockResolvedValueOnce(stepResult())
      .mockResolvedValueOnce(stepResult({ round: 2 }))
      .mockResolvedValueOnce(stepResult({ round: 3 }));
    renderCard();
    await user.click(screen.getByRole("button", { name: /start the search/i }));
    await screen.findByText("Round 1");
    await user.click(
      screen.getByRole("button", { name: /better — next round/i }),
    );
    await screen.findByText("Round 2");
    expect(doctorTuneStep).toHaveBeenLastCalledWith(
      expect.anything(),
      "better",
    );
    expect(screen.getByTestId("tune-history")).toHaveTextContent(
      "Round 1 · kept",
    );
    await user.click(
      screen.getByRole("button", { name: /not better — try another/i }),
    );
    await screen.findByText("Round 3");
    expect(doctorTuneStep).toHaveBeenLastCalledWith(expect.anything(), "worse");
    expect(screen.getByTestId("tune-history")).toHaveTextContent(
      "Round 2 · rejected",
    );
  });

  it("'Save this' persists the step's cumulative ops with the overlay and ends without discard", async () => {
    const user = userEvent.setup();
    vi.mocked(doctorTuneStep).mockResolvedValue(stepResult());
    renderCard();
    await user.click(screen.getByRole("button", { name: /start the search/i }));
    await screen.findByText("Round 1");
    const save = screen.getByRole("button", { name: /save this/i });
    expect(save).toBeDisabled();
    await user.click(screen.getByText("I've backed up with Pro Control"));
    await user.click(save);
    expect(await screen.findByText("Saved to the preset.")).toBeInTheDocument();
    expect(doctorSave).toHaveBeenCalledWith(
      0,
      "Strat (Orange)",
      1,
      OPS_R1,
      OVERLAY,
    );
    expect(doctorTuneEnd).toHaveBeenCalledWith(0, false);
  });

  it("'Stop & discard' ends the loop with a discard and returns to idle", async () => {
    const user = userEvent.setup();
    vi.mocked(doctorTuneStep).mockResolvedValue(stepResult());
    renderCard();
    await user.click(screen.getByRole("button", { name: /start the search/i }));
    await screen.findByText("Round 1");
    await user.click(screen.getByRole("button", { name: /stop & discard/i }));
    expect(
      await screen.findByRole("button", { name: /start the search/i }),
    ).toBeInTheDocument();
    expect(doctorTuneEnd).toHaveBeenCalledWith(0, true);
    expect(doctorSave).not.toHaveBeenCalled();
  });

  it("a converged step offers 'Save what I kept' only when the baseline has edits", async () => {
    const user = userEvent.setup();
    vi.mocked(doctorTuneStep).mockResolvedValueOnce(
      stepResult({
        status: "converged",
        candidate: null,
        candidateClip: null,
        measured: null,
        candidateDiags: [],
        cleared: [],
        remained: [],
        baselineOps: OPS_R1,
        ops: OPS_R1,
        message:
          "No tonal finding is left on this sound — nothing more to move.",
      }),
    );
    renderCard();
    await user.click(screen.getByRole("button", { name: /start the search/i }));
    expect(await screen.findByText("Nothing left to fix")).toBeInTheDocument();
    expect(
      screen.queryByRole("button", { name: /better — next round/i }),
    ).toBeNull();
    await user.click(screen.getByText("I've backed up with Pro Control"));
    await user.click(screen.getByRole("button", { name: /save what i kept/i }));
    expect(doctorSave).toHaveBeenCalledWith(
      0,
      "Strat (Orange)",
      1,
      OPS_R1,
      OVERLAY,
    );
  });

  it("a failed round surfaces the error and returns to idle", async () => {
    const user = userEvent.setup();
    vi.mocked(doctorTuneStep).mockRejectedValue(
      new Error("no signal on USB 1/2"),
    );
    renderCard();
    await user.click(screen.getByRole("button", { name: /start the search/i }));
    expect(await screen.findByText("no signal on USB 1/2")).toBeInTheDocument();
    expect(
      screen.getByRole("button", { name: /start the search/i }),
    ).toBeEnabled();
  });
});
