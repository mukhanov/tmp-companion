// src/__tests__/FsParamPick.test.tsx — the footswitch-row param picker's DANGER-rule
// guard: an out-of-range `index` (in particular -1, "no classifiable default yet") must
// show an explicit "Choose a parameter" warning, never silently render `params[0]` as
// if it were selected — even with a single candidate, which now forces an explicit
// click instead of a silent auto-select.

import { describe, it, expect, vi } from "vitest";
import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";

import { ThemeProvider } from "../theme/ThemeProvider";
import { FsParamPick } from "../views/overlays/FsParamPick";
import { WithCard } from "./pickCardTestUtils";
import type { LevelParamCandidate } from "../lib/types";

// A single candidate. The `index=-1` these tests pass is the DANGER-rule case under
// test (an out-of-range/not-yet-chosen index the component must never silently render
// as `params[0]`) — it's driven by the `index` prop, not by what this candidate
// classifies as, so any real (non-"other") class exercises it.
const toneOnly: LevelParamCandidate[] = [
  {
    group_id: "G1",
    node_id: "ped",
    fender_id: "ACD_BluesDriver",
    parameter_id: "gain",
    current: 0.4,
    class: "wet_mix",
  },
];

const mixed: LevelParamCandidate[] = [
  ...toneOnly,
  {
    group_id: "G1",
    node_id: "ped",
    fender_id: "ACD_BluesDriver",
    parameter_id: "level",
    current: 0.6,
    class: "level_linear",
  },
];

describe("FsParamPick — no silent default", () => {
  it('shows "Choose a parameter" (warning state) for index=-1, never params[0]', () => {
    render(
      <ThemeProvider>
        <WithCard>
          <FsParamPick params={toneOnly} index={-1} onChange={vi.fn()} />
        </WithCard>
      </ThemeProvider>,
    );
    expect(screen.getByText("Choose a parameter")).toBeInTheDocument();
    // The old trap: this would have silently shown "Gain" as if chosen.
    expect(screen.queryByText("Gain")).toBeNull();
  });

  it("is interactive even with a SINGLE candidate when nothing is selected yet", async () => {
    const onChange = vi.fn();
    render(
      <ThemeProvider>
        <WithCard>
          <FsParamPick params={toneOnly} index={-1} onChange={onChange} />
        </WithCard>
      </ThemeProvider>,
    );
    const user = userEvent.setup();
    await user.click(screen.getByText("Choose a parameter"));
    await user.click(await screen.findByText("Gain"));
    expect(onChange).toHaveBeenCalledWith(0);
  });

  it("renders the selected candidate normally once a valid index is given", () => {
    render(
      <ThemeProvider>
        <WithCard>
          <FsParamPick params={mixed} index={1} onChange={vi.fn()} />
        </WithCard>
      </ThemeProvider>,
    );
    expect(screen.getByText("Level")).toBeInTheDocument();
    expect(screen.queryByText("Choose a parameter")).toBeNull();
  });
});
