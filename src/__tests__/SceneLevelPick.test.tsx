// src/__tests__/SceneLevelPick.test.tsx — the scene row's combined target-mode +
// leveling-control picker (P2). Covers: the trigger's compact summary, the lazy-open
// contract, class grouping (Level vs Mix), a `shared_with_base` candidate rendered
// DISABLED with its reason visible (never hidden — the backend refuses that write), a
// `lowers_only` annotation, and the DANGER-rule guard for a stored handle the current
// candidate set no longer contains (must show a warning, never silently fall back to
// the amp default).

import { describe, it, expect, vi } from "vitest";
import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";

import { ThemeProvider } from "../theme/ThemeProvider";
import { SceneLevelPick } from "../views/overlays/SceneLevelPick";
import { WithCard } from "./pickCardTestUtils";
import type { HandleFetchState } from "../views/level/useSceneHandles";
import type { SceneHandleCandidate } from "../lib/types";

/** A resolved fetch state carrying `list` — the shape most tests want. */
const resolved = (list: SceneHandleCandidate[]): HandleFetchState => ({
  status: "resolved",
  candidates: list,
});

const levelCandidate: SceneHandleCandidate = {
  groupId: "G1",
  nodeId: "amp",
  fenderId: "ACD_TwinReverb65NoFx",
  parameterId: "outputLevel",
  class: "level_linear",
  range: [0, 1],
  current: 0.5,
  scope: "isolated",
  headroom: "full",
};

const sharedCandidate: SceneHandleCandidate = {
  groupId: "G1",
  nodeId: "amp2",
  fenderId: "ACD_HiwattDR103",
  parameterId: "outputLevel",
  class: "level_linear",
  range: [0, 1],
  current: 0.7,
  scope: "shared_with_base",
  headroom: "full",
};

const lowersOnlyCandidate: SceneHandleCandidate = {
  groupId: "G1",
  nodeId: "ped",
  fenderId: "ACD_KingOfTone",
  parameterId: "volume",
  class: "level_linear",
  range: [0, 1],
  current: 0.98,
  scope: "isolated",
  headroom: "lowers_only",
};

const wetCandidate: SceneHandleCandidate = {
  groupId: "G1",
  nodeId: "verb",
  fenderId: "ACD_ConvRvb",
  parameterId: "mix",
  class: "wet_mix",
  range: [0.25, 1],
  current: 0.5,
  scope: "isolated",
  headroom: "full",
};

function renderPick(
  over: Partial<React.ComponentProps<typeof SceneLevelPick>> = {},
) {
  const onTargetModeChange = vi.fn();
  const onHandleChange = vi.fn();
  const onOpen = vi.fn();
  render(
    <ThemeProvider>
      <WithCard>
        <SceneLevelPick
          targetMode="match"
          onTargetModeChange={onTargetModeChange}
          handle={null}
          onHandleChange={onHandleChange}
          candidates={{ status: "unfetched" }}
          onOpen={onOpen}
          {...over}
        />
      </WithCard>
    </ThemeProvider>,
  );
  return { onTargetModeChange, onHandleChange, onOpen };
}

describe("SceneLevelPick trigger", () => {
  it("shows the amp default + match mode when nothing is chosen", () => {
    renderPick();
    expect(screen.getByText("Amp · match target")).toBeInTheDocument();
  });

  it("shows the chosen handle's friendly name + offset mode", () => {
    renderPick({
      targetMode: "offset",
      handle: { groupId: "G1", nodeId: "verb", parameterId: "mix" },
      candidates: resolved([wetCandidate]),
    });
    expect(screen.getByText(/Mix · keep offset/)).toBeInTheDocument();
  });

  // DANGER-rule guard: a stored handle the CURRENT candidate list doesn't contain must
  // show a warning, never silently render as the amp default.
  it("flags a stored handle no longer in the candidate list, never silently reverting to Amp", () => {
    renderPick({
      handle: { groupId: "G1", nodeId: "gone", parameterId: "outputLevel" },
      candidates: resolved([levelCandidate]),
    });
    expect(screen.queryByText("Amp · match target")).toBeNull();
    expect(screen.getByText(/removed/)).toBeInTheDocument();
  });

  // BUG→GATE (item 8a): before the lazy fetch resolves, a carried-forward VALID
  // handle must render its plain param label — `status: "unfetched"` is not proof
  // the handle is gone, only that nothing has been checked yet. The old code
  // conflated the two (`Array.isArray(candidates) ? candidates : []` treated
  // "not fetched" exactly like "fetched, empty"), so this handle rendered
  // "(removed)" the instant Set up opened, before any device read happened.
  it("shows a stored handle's plain label while unfetched — no removed/warn state", () => {
    renderPick({
      handle: { groupId: "G1", nodeId: "amp", parameterId: "outputLevel" },
      candidates: { status: "unfetched" },
    });
    expect(screen.getByText(/Output level · match target/)).toBeInTheDocument();
    expect(screen.queryByText(/removed/)).toBeNull();
  });
});

describe("SceneLevelPick menu", () => {
  it("fires onOpen (lazy fetch trigger) on first click", async () => {
    const { onOpen } = renderPick();
    const user = userEvent.setup();
    await user.click(screen.getByText("Amp · match target"));
    expect(onOpen).toHaveBeenCalledTimes(1);
  });

  it("picks a target mode without touching the handle", async () => {
    const { onTargetModeChange, onHandleChange } = renderPick({
      candidates: resolved([levelCandidate]),
    });
    const user = userEvent.setup();
    await user.click(screen.getByText("Amp · match target"));
    await user.click(await screen.findByText("Keep its offset"));
    expect(onTargetModeChange).toHaveBeenCalledWith("offset");
    expect(onHandleChange).not.toHaveBeenCalled();
  });

  it("groups candidates by class (Level vs Mix)", async () => {
    renderPick({ candidates: resolved([levelCandidate, wetCandidate]) });
    const user = userEvent.setup();
    await user.click(screen.getByText("Amp · match target"));
    expect(await screen.findByText("Level")).toBeInTheDocument();
    // "Mix" also appears as the wet candidate's own param label — assert the section
    // header exists rather than assuming uniqueness.
    expect(screen.getAllByText("Mix").length).toBeGreaterThan(0);
  });

  it("picks an isolated candidate and closes the menu", async () => {
    const { onHandleChange } = renderPick({
      candidates: resolved([levelCandidate]),
    });
    const user = userEvent.setup();
    await user.click(screen.getByText("Amp · match target"));
    await user.click(await screen.findByText("Output level"));
    expect(onHandleChange).toHaveBeenCalledWith({
      groupId: "G1",
      nodeId: "amp",
      parameterId: "outputLevel",
    });
  });

  // The backend REFUSES a shared_with_base write — the picker must show it, not hide
  // it, but disable selecting it and state the reason inline.
  it("shows a shared_with_base candidate DISABLED with its reason, never selectable", async () => {
    const { onHandleChange } = renderPick({
      candidates: resolved([sharedCandidate]),
    });
    const user = userEvent.setup();
    await user.click(screen.getByText("Amp · match target"));
    expect(
      await screen.findByText(
        "shared with the base preset — changes every scene sharing it",
      ),
    ).toBeInTheDocument();
    await user.click(screen.getByText("Output level"));
    expect(onHandleChange).not.toHaveBeenCalled();
  });

  it("annotates a lowers_only candidate without disabling it", async () => {
    const { onHandleChange } = renderPick({
      candidates: resolved([lowersOnlyCandidate]),
    });
    const user = userEvent.setup();
    await user.click(screen.getByText("Amp · match target"));
    expect(await screen.findByText("can only lower")).toBeInTheDocument();
    await user.click(screen.getByText("Volume"));
    expect(onHandleChange).toHaveBeenCalledWith({
      groupId: "G1",
      nodeId: "ped",
      parameterId: "volume",
    });
  });

  it("shows a loading state while the lazy fetch is in flight", async () => {
    renderPick({ candidates: { status: "loading" } });
    const user = userEvent.setup();
    await user.click(screen.getByText("Amp · match target"));
    expect(await screen.findByText("Loading controls…")).toBeInTheDocument();
  });
});
