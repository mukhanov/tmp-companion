// src/__tests__/BlockLevelPick.test.tsx — the DANGER-rule guard for the combined
// block+param leveling-handle picker (D2), ported from the deleted
// SceneLevelPick.test.tsx (whose own component is gone — BlockLevelPick replaced it
// for all three row kinds). A stored `handle` the current candidate set doesn't cover
// must render VERBATIM + a warning, NEVER silently fall back to the pseudo-option or
// `candidates[0]` (danger.md's Pick/BlockPick trap) — and "not yet fetched" must not
// be conflated with "fetched, this handle is gone" (BUG→GATE, item 8a).

import { describe, it, expect } from "vitest";
import { render, screen } from "@testing-library/react";

import { ThemeProvider } from "../theme/ThemeProvider";
import { BlockLevelPick } from "../views/overlays/BlockLevelPick";
import { WithCard } from "./pickCardTestUtils";
import type { BlockLevelCandidate } from "../views/overlays/BlockLevelPick";

const levelCandidate: BlockLevelCandidate = {
  groupId: "G1",
  nodeId: "amp",
  fenderId: "ACD_TwinReverb65NoFx",
  parameterId: "outputLevel",
  paramClass: "level_linear",
};

describe("BlockLevelPick — DANGER-rule guard for a stale stored handle", () => {
  it("flags a stored handle no longer in the (resolved) candidate list, never silently reverting to the pseudo-option", () => {
    render(
      <ThemeProvider>
        <WithCard>
          <BlockLevelPick
            pseudoLabel="Amp output level"
            handle={{
              groupId: "G1",
              nodeId: "gone",
              parameterId: "outputLevel",
            }}
            onHandleChange={() => undefined}
            candidates={{ status: "resolved", list: [levelCandidate] }}
            onOpen={() => undefined}
          />
        </WithCard>
      </ThemeProvider>,
    );
    expect(screen.queryByText("Amp output level")).toBeNull();
    expect(screen.getByText(/removed/)).toBeInTheDocument();
  });

  // BUG→GATE (item 8a): before the lazy fetch resolves, a carried-forward VALID
  // handle must render its plain param label — `status: "unfetched"` is not proof
  // the handle is gone, only that nothing has been checked yet.
  it("shows a stored handle's plain label while unfetched — no removed/warn state", () => {
    render(
      <ThemeProvider>
        <WithCard>
          <BlockLevelPick
            pseudoLabel="Amp output level"
            handle={{
              groupId: "G1",
              nodeId: "amp",
              parameterId: "outputLevel",
            }}
            onHandleChange={() => undefined}
            candidates={{ status: "unfetched" }}
            onOpen={() => undefined}
          />
        </WithCard>
      </ThemeProvider>,
    );
    expect(screen.getByText("Output level")).toBeInTheDocument();
    expect(screen.queryByText(/removed/)).toBeNull();
  });
});
