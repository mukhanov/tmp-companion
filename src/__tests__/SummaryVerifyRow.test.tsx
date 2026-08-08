// src/__tests__/SummaryVerifyRow.test.tsx — Summary's rendering of a VERIFY-only
// footswitch row (P2): nothing was written, only measured, so it must NEVER read as
// "done"/leveled — a compact ON-vs-OFF delta instead — and it groups separately so the
// leveled tally stays honest. Also covers the wet_floor by-ear cause and the scene
// Offset target-mode "kept +N LU" suffix.

import { describe, it, expect } from "vitest";
import { screen } from "@testing-library/react";

import type { RunItem } from "../views/level/leveling";
import { base, renderSummary } from "./summaryTestUtils";

const verifyRow = (over: Partial<RunItem>): RunItem => ({
  key: "f3:0",
  slot: 3,
  presetName: "Guitar",
  isBase: false,
  sceneSlot: null,
  sceneName: "Boost",
  tag: "FS1",
  footswitch: {
    switchIndex: 0,
    mode: "verify",
  },
  instId: "none",
  targetName: "Lead",
  status: "result",
  outcome: "verified",
  ...over,
});

describe("Summary verify-only row", () => {
  it("renders the ON/OFF delta, never a 'done'/LUFS number", () => {
    renderSummary([verifyRow({ verifyDeltaLu: 2.3 })]);
    expect(screen.getByText("+2.3 LU vs off")).toBeInTheDocument();
    expect(screen.queryByText(/LUFS/)).toBeNull();
  });

  it("shows the quieter direction with a minus sign", () => {
    renderSummary([verifyRow({ verifyDeltaLu: -1.1 })]);
    expect(screen.getByText("−1.1 LU vs off")).toBeInTheDocument();
  });

  it("groups under 'Verified', separate from 'Leveled', and titles a fully-clean mixed run as ALL checked, not partial", () => {
    renderSummary([
      base({}), // one genuinely leveled Base row
      verifyRow({ verifyDeltaLu: 0.4 }),
    ]);
    expect(screen.getByText("Verified")).toBeInTheDocument();
    expect(screen.getByText("Leveled")).toBeInTheDocument();
    // BUG→GATE: nothing in this run failed, so the title must read as a CLEAN run —
    // the old "2 of 2 sounds checked" read exactly like a failure count even though
    // both rows resolved cleanly (one leveled, one verified by design).
    expect(
      screen.getByText("All 2 sounds checked (1 measured only)"),
    ).toBeInTheDocument();
  });

  it("an all-leveled run (no verify rows) keeps the pre-existing 'All N leveled' copy", () => {
    renderSummary([base({}), base({ key: "p4", slot: 4 })]);
    expect(screen.getByText("All 2 sounds leveled")).toBeInTheDocument();
  });

  it("a run with a real failure alongside a clean verify row reads as partial, crediting the measured count separately", () => {
    renderSummary([
      base({}), // one genuinely leveled Base row
      verifyRow({ verifyDeltaLu: 0.4 }), // one cleanly verified row
      base({
        key: "p5",
        slot: 5,
        outcome: "clamped",
        value: -18.5,
      }), // one genuine failure — the run is NOT all-good
    ]);
    // Honest partial title: 1 of 3 leveled, and the verified row is credited
    // separately rather than silently folded into "not leveled".
    expect(screen.getByText("1 of 3 leveled, 1 measured")).toBeInTheDocument();
  });
});

describe("Summary wet_floor by-ear cause", () => {
  it("flags a wet-floored row with the by-ear chip and its own footnote line", () => {
    renderSummary([
      base({
        key: "f5:0",
        footswitch: {
          switchIndex: 0,
          levGroupId: "G1",
          levNodeId: "ped",
          levParameterId: "mix",
          mode: "level",
        },
        outcome: "clamped",
        value: -21,
        verifyByEar: "wet_floor",
      }),
    ]);
    // Two "by ear" chips render (the row's own + the footnote's icon), so assert
    // presence via getAllByText rather than a uniqueness-assuming getByText.
    expect(screen.getAllByText("by ear").length).toBeGreaterThan(0);
    expect(
      screen.getByText(/floored at 25% of its designed mix/),
    ).toBeInTheDocument();
  });
});

describe("Summary scene Offset target-mode suffix", () => {
  it("appends 'kept +N LU' to a done row's result when target_offset_lu is set", () => {
    renderSummary([
      base({
        sceneSlot: 1,
        isBase: false,
        targetMode: "offset",
        targetOffsetLu: 1.4,
        value: -22,
      }),
    ]);
    expect(screen.getByText("−22.0 LUFS · kept +1.4 LU")).toBeInTheDocument();
  });

  it("omits the suffix in Match mode (targetOffsetLu unset)", () => {
    renderSummary([base({ value: -22 })]);
    expect(screen.getByText("−22.0 LUFS")).toBeInTheDocument();
  });
});
