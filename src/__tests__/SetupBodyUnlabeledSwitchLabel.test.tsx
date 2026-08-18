// src/__tests__/SetupBodyUnlabeledSwitchLabel.test.tsx — label provenance for an
// UNLABELED footswitch.
//
// BUG→GATE, and a WIRE WRITE rather than a cosmetic one. A footswitch row's `sceneName`
// becomes `displayLabel` on the job (`useLevelingFlow` → `toFootswitchJobWire`), which the
// backend writes as the switch's on-device `customLabel` when an assign appends a function
// to a switch that has none — so the string ends up printed on the player's pedalboard.
//
// `chosenFrom` picks that name when the LIST is built, and at that moment the only
// candidate it knows about is the tone-safe DEFAULT (`defaultParamIndex`). A user who then
// overrides the pick in Set up used to have the switch renamed after the default
// candidate's block while the run leveled a different one. `SetupBody.start` now re-derives
// the name from the candidate actually chosen — but only for an unlabeled switch: a
// player's own label is theirs, and picking a different knob does not make it wrong.

import { describe, it, expect, vi } from "vitest";
import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";

import { ThemeProvider } from "../theme/ThemeProvider";
import { SetupBody, type SetupChoice } from "../views/overlays/SetupBody";
import { WithCard } from "./pickCardTestUtils";
import type { SetupOption } from "../views/level/leveling";
import type { LevelParamCandidate } from "../lib/types";
import type { PickOption } from "../views/overlays/Pick";

// Two level candidates on DIFFERENT blocks. The Boost is first, so `defaultParamIndex`
// (first best-ranked hit) makes it the default and the row is born named after it.
const boostGain: LevelParamCandidate = {
  group_id: "G1",
  node_id: "ACD_Boost",
  parameter_id: "gain",
  class: "level_db",
  current: 2.5,
  fender_id: "ACD_Boost",
};
const screamerLevel: LevelParamCandidate = {
  group_id: "G1",
  node_id: "ACD_TubeScreamer",
  parameter_id: "level",
  class: "level_linear",
  current: 0.5,
  fender_id: "ACD_TubeScreamer",
};

const instrumentOptions: PickOption[] = [{ id: "none", label: "None" }];
const targetOptions: PickOption[] = [{ id: "Rhythm", label: "Rhythm −18" }];

/** The row `chosenFrom` builds for an unlabeled block-acting switch: `sceneName` is the
 *  DEFAULT candidate's block, and `fsUnlabeled` records that it is a derived fallback. */
function fsOption(over: Partial<SetupOption> = {}): SetupOption {
  return {
    key: "f0:0",
    slot: 0,
    presetName: "Rig",
    isBase: false,
    sceneSlot: null,
    sceneName: "Boost",
    tag: "FS3",
    hasScenes: true,
    // D2: every footswitch row already carries a real handle — the tone-safe DEFAULT
    // (the Boost's `gain`, the block the row is currently named after), same as
    // `chosenFrom` seeds it. `sceneContext: null` (D3) = the base sound.
    footswitch: {
      switchIndex: 2,
      levGroupId: "G1",
      levNodeId: "ACD_Boost",
      levParameterId: "gain",
      sceneContext: null,
    },
    levelParams: [boostGain, screamerLevel],
    fsUnlabeled: true,
    ...over,
  };
}

async function pickTubeScreamerAndStart(option: SetupOption) {
  const onStart = vi.fn<(c: SetupChoice[]) => void>();
  render(
    <ThemeProvider>
      {/* The param picker is card-portaled (`usePickAnchor`), and in production SetupBody
          always lives inside the wizard's `<Dialog>` — supply the same card context. */}
      <WithCard>
        <SetupBody
          options={[option]}
          presetCount={1}
          isRelevel={false}
          instrumentOptions={instrumentOptions}
          targetOptions={targetOptions}
          defaultInst="none"
          defaultTarget="Rhythm"
          onCancel={vi.fn()}
          onStart={onStart}
        />
      </WithCard>
    </ThemeProvider>,
  );
  const user = userEvent.setup();
  // Every row levels now (D2 — no "Make level-neutral" opt-in any more). Open the
  // combined block+param picker, currently on the tone-safe DEFAULT (Boost · Gain)…
  await user.click(screen.getByTitle("Choose this sound's leveling control"));
  // …and override it with the OTHER block's `level`.
  await user.click(await screen.findByText("Level"));
  // The backup acknowledgment gates the primary button on a fresh run.
  await user.click(screen.getByText(/I.ve backed up with Pro Control/i));
  await user.click(screen.getByRole("button", { name: /level 1 sound/i }));
  return onStart;
}

describe("SetupBody — label provenance for an unlabeled footswitch", () => {
  it("renames the row after the PICKED block, so the customLabel write names what was leveled", async () => {
    const onStart = await pickTubeScreamerAndStart(fsOption());
    expect(onStart).toHaveBeenCalledTimes(1);
    const choice = onStart.mock.calls[0][0][0];
    // The submitted target is the overridden candidate…
    expect(choice.option.footswitch).toMatchObject({
      switchIndex: 2,
      levNodeId: "ACD_TubeScreamer",
      levParameterId: "level",
      sceneContext: null,
    });
    // …and the name that becomes `displayLabel` names THAT block, not the default Boost.
    expect(choice.option.sceneName).toBe("Tube Screamer");
    expect(choice.option.sceneName).not.toBe("Boost");
  });

  it("leaves a LABELED switch's own name alone whatever is picked", async () => {
    const onStart = await pickTubeScreamerAndStart(
      // The player named this switch "Solo" on the device; `chosenFrom` keeps that name
      // verbatim and records `fsUnlabeled: false`.
      fsOption({ sceneName: "Solo", fsUnlabeled: false }),
    );
    const choice = onStart.mock.calls[0][0][0];
    expect(choice.option.footswitch).toMatchObject({
      levNodeId: "ACD_TubeScreamer",
    });
    expect(choice.option.sceneName).toBe("Solo");
  });
});
