// src/__tests__/TonePlanCard.test.tsx — the balance-plan card: renders the
// backend's per-block knob moves ("'65 Twin Reverb · Bass 6.0 → 4.0"), the
// honest outcome sentence (clears / still expected + from which volume), the
// loudness note, and hands the plan's `rx` to PrescriptionCard's apply flow
// — whose applied state now shows the MEASURED band change the backend
// returns. Also pins the pure planModel helpers.

import { describe, it, expect, vi, beforeEach } from "vitest";
import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";

import { ThemeProvider } from "../theme/ThemeProvider";

vi.mock("../lib/invoke", async (importOriginal) => {
  const actual = await importOriginal<typeof import("../lib/invoke")>();
  return {
    ...actual,
    doctorApply: vi.fn(),
  };
});

import { doctorApply } from "../lib/invoke";
import { TonePlanCard } from "../views/doctor/TonePlanCard";
import { sceneOverlayFor } from "../views/doctor/useDoctorFlow";
import {
  groupByBlock,
  measuredBandLine,
  planOutcomeSentence,
} from "../views/doctor/planModel";
import type {
  DoctorDiag,
  DoctorSoundResult,
  DoctorTonePlan,
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

function plan(overrides: Partial<DoctorTonePlan> = {}): DoctorTonePlan {
  return {
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
      {
        groupId: "G1",
        nodeId: "amp",
        model: "ACD_TwinReverb65NoFx",
        blockName: "'65 Twin Reverb",
        param: "treb",
        controlLabel: "Treble",
        unit: "knob",
        from: 0.5,
        to: 0.55,
        fromLabel: "5.0",
        toLabel: "5.5",
      },
      {
        groupId: "G1",
        nodeId: "ts",
        model: "ACD_TubeScreamer",
        blockName: "Greenbox 8",
        param: "tone",
        controlLabel: "Tone",
        unit: "knob",
        from: 0.5,
        to: 0.6,
        fromLabel: "5.0",
        toLabel: "6.0",
      },
    ],
    beforeDb: [0, 7, 0, 0, 0, 0],
    predictedDb: [-1.2, 1.0, 0.3, 0.4, 0.8, 1.1],
    bandLabels: LABELS,
    clears: ["muddy"],
    remains: [{ key: "dark", fromLevel: "stage" }],
    loudnessDeltaDb: -1.3,
    rx: {
      kind: "oneclick",
      title: "Rebalance with the blocks you have",
      detail:
        "'65 Twin Reverb: Bass 6.0 → 4.0, Treble 5.0 → 5.5 · Greenbox 8: Tone 5.0 → 6.0",
      cpuNote: "no CPU change",
      ops: [
        {
          kind: "param",
          groupId: "G1",
          nodeId: "amp",
          param: "bass",
          value: 0.4,
        },
        {
          kind: "param",
          groupId: "G1",
          nodeId: "amp",
          param: "treb",
          value: 0.55,
        },
        {
          kind: "param",
          groupId: "G1",
          nodeId: "ts",
          param: "tone",
          value: 0.6,
        },
      ],
    },
    model: "nominal-tonestack-v1",
    ...overrides,
  };
}

function sound(p: DoctorTonePlan | null): DoctorSoundResult {
  return {
    key: "p0",
    listIndex: 0,
    scene: null,
    footswitch: null,
    label: "Base",
    tag: null,
    diags: [MUDDY, DARK],
    integratedLufs: -20,
    tailRatioDb: -30,
    balanceDb: [0, 7, 0, 0, 0, 0],
    bandLabels: LABELS,
    cutThrough: null,
    plan: p,
    error: null,
  };
}

const NODES: GraphNode[] = [
  {
    group_id: "G1",
    node_id: "ts",
    model: "ACD_TubeScreamer",
    bypassed: false,
    params: { tone: 0.5 },
  },
  {
    group_id: "G1",
    node_id: "amp",
    model: "ACD_TwinReverb65NoFx",
    bypassed: false,
    params: { bass: 0.6, treb: 0.5 },
  },
];

const SCENE_OVERLAY: SceneNodeOverlay[] = [
  { group_id: "G1", node_id: "amp", bypassed: false, params: { bass: 0.3 } },
];

function renderCard(p: DoctorTonePlan, sceneOverlay?: SceneNodeOverlay[]) {
  return render(
    <ThemeProvider>
      <TonePlanCard
        sound={sound(p)}
        plan={p}
        listIndex={0}
        presetName="Test Preset"
        nodes={NODES}
        footswitches={[]}
        sceneOverlay={sceneOverlay}
      />
    </ThemeProvider>,
  );
}

beforeEach(() => {
  vi.mocked(doctorApply).mockReset();
});

describe("TonePlanCard", () => {
  it("renders one row per block with the knob moves and the honest outcome", () => {
    renderCard(plan());
    expect(screen.getByText("Balance plan")).toBeInTheDocument();
    expect(screen.getByText("Estimated")).toBeInTheDocument();
    expect(screen.getByText("Your own knobs")).toBeInTheDocument();
    expect(
      screen.getByText("Rebalance with the blocks you have"),
    ).toBeInTheDocument();
    expect(screen.getByText("'65 Twin Reverb")).toBeInTheDocument();
    expect(screen.getByText("Greenbox 8")).toBeInTheDocument();
    // Knob moves: from → to, per control.
    const moves = screen.getByTestId("plan-moves");
    expect(moves).toHaveTextContent("Bass 6.0 → 4.0");
    expect(moves).toHaveTextContent("Treble 5.0 → 5.5");
    expect(moves).toHaveTextContent("Tone 5.0 → 6.0");
    // Outcome: what clears and what still fires, with the volume wording.
    expect(
      screen.getByText(
        "Predicted to clear Muddy at any volume. Dark at stage volume only — still expected after this.",
      ),
    ).toBeInTheDocument();
    expect(
      screen.getByText(/About −1\.3 dB quieter overall/),
    ).toBeInTheDocument();
    // Honesty line + no CPU change.
    expect(
      screen.getByText(/Estimated from a nominal tone-stack model/),
    ).toBeInTheDocument();
    expect(screen.getByText("no CPU change")).toBeInTheDocument();
  });

  it("applies the plan's param ops verbatim and shows the measured change after", async () => {
    const user = userEvent.setup();
    vi.mocked(doctorApply).mockResolvedValue({
      beforeClip: "data:audio/wav;base64,AAAA",
      afterClip: "data:audio/wav;base64,BBBB",
      measured: {
        bandLabels: LABELS,
        beforeBalanceDb: [0, 7, 0, 0, 0, 0],
        afterBalanceDb: [-0.4, 2.1, 0.2, 0.1, 0.9, 0.3],
        deltaDb: [-0.4, -4.9, 0.2, 0.1, 0.9, 0.3],
        loudnessDeltaDb: -1.1,
      },
    });
    renderCard(plan());
    await user.click(
      screen.getByRole("button", { name: /apply to the unit/i }),
    );
    expect(doctorApply).toHaveBeenCalledWith(
      expect.objectContaining({
        listIndex: 0,
        name: "Test Preset",
        ops: plan().rx.ops,
        nodes: NODES,
        sceneOverlay: [],
      }),
    );
    expect(await screen.findByText("Listen & compare")).toBeInTheDocument();
    const measured = screen.getByTestId("measured-change");
    expect(measured).toHaveTextContent("Measured");
    expect(measured).toHaveTextContent("Low-mids −4.9");
    expect(measured).toHaveTextContent("Highs +0.9");
    expect(measured).not.toHaveTextContent("Lows");
    expect(measured).toHaveTextContent("−1.1 dB quieter");
  });

  it("hands the diagnosed scene's overlay to the apply job (empty by default)", async () => {
    const user = userEvent.setup();
    vi.mocked(doctorApply).mockResolvedValue({
      beforeClip: "data:audio/wav;base64,AAAA",
      afterClip: "data:audio/wav;base64,BBBB",
      measured: null,
    });
    renderCard(plan(), SCENE_OVERLAY);
    await user.click(
      screen.getByRole("button", { name: /apply to the unit/i }),
    );
    expect(doctorApply).toHaveBeenCalledWith(
      expect.objectContaining({ sceneOverlay: SCENE_OVERLAY }),
    );
    expect(await screen.findByText("Listen & compare")).toBeInTheDocument();
  });

  it("omits the measured line when the backend couldn't analyze a capture", async () => {
    const user = userEvent.setup();
    vi.mocked(doctorApply).mockResolvedValue({
      beforeClip: "data:audio/wav;base64,AAAA",
      afterClip: "data:audio/wav;base64,BBBB",
      measured: null,
    });
    renderCard(plan());
    await user.click(
      screen.getByRole("button", { name: /apply to the unit/i }),
    );
    expect(await screen.findByText("Listen & compare")).toBeInTheDocument();
    expect(screen.queryByTestId("measured-change")).not.toBeInTheDocument();
  });

  it("skips the loudness note for a sub-0.5 dB predicted change", () => {
    renderCard(plan({ loudnessDeltaDb: 0.2, remains: [] }));
    expect(screen.queryByText(/overall — re-level/)).not.toBeInTheDocument();
    expect(
      screen.getByText("Predicted to clear Muddy at any volume."),
    ).toBeInTheDocument();
  });
});

describe("sceneOverlayFor", () => {
  it("returns the preset's scene-indexed overlay, empty for base/FS or unknown", () => {
    const overlays = new Map([[4, [SCENE_OVERLAY, []]]]);
    expect(sceneOverlayFor(overlays, 4, 0)).toBe(SCENE_OVERLAY);
    expect(sceneOverlayFor(overlays, 4, 1)).toEqual([]);
    // A scene past the stored count (truncated row) and a base sound → empty.
    expect(sceneOverlayFor(overlays, 4, 7)).toEqual([]);
    expect(sceneOverlayFor(overlays, 4, null)).toEqual([]);
    expect(sceneOverlayFor(overlays, 9, 0)).toEqual([]);
  });
});

describe("planModel", () => {
  it("groups moves per block in first-seen order", () => {
    const g = groupByBlock(plan().moves);
    expect(g.map((x) => x.blockName)).toEqual([
      "'65 Twin Reverb",
      "Greenbox 8",
    ]);
    expect(g[0]?.rows.map((r) => r.param)).toEqual(["bass", "treb"]);
  });

  it("labels outcome keys from the sound's own diagnoses, capitalizing unknown keys", () => {
    const p = plan({
      clears: ["muddy", "fizzy"],
      remains: [{ key: "dark", fromLevel: "rehearsal" }],
    });
    expect(planOutcomeSentence(p, [MUDDY, DARK])).toBe(
      "Predicted to clear Muddy, Fizzy at any volume. Dark from rehearsal volume — still expected after this.",
    );
    expect(planOutcomeSentence(plan({ clears: [], remains: [] }), [])).toBe("");
  });

  it("measuredBandLine lists only bands past the 0.5 dB floor", () => {
    expect(
      measuredBandLine({
        bandLabels: LABELS,
        beforeBalanceDb: [],
        afterBalanceDb: [],
        deltaDb: [0.1, -2.25, 0.49, 0.5, 0, 3],
        loudnessDeltaDb: 0,
      }),
    ).toBe("Low-mids −2.3 · High-mids +0.5 · Air +3.0");
    expect(
      measuredBandLine({
        bandLabels: LABELS,
        beforeBalanceDb: [],
        afterBalanceDb: [],
        deltaDb: [0, 0, 0, 0, 0, 0],
        loudnessDeltaDb: 0,
      }),
    ).toBe("no band moved more than 0.5 dB");
  });
});
