import type { Page } from "@playwright/test";
import { test, expect } from "../fixtures/test";
import {
  SCENARIO,
  clearScenario,
  ensureScenario,
  expectReampBalanced,
  invoke,
  isOnline,
  LEVEL_T,
  openLevel,
  reampCounters,
  reampOff,
  simEvents,
} from "../fixtures/scenario";

// New P4-B (rebuilt-fixture) coverage that lives at the SETUP step (before any run) or is
// provable via a raw command invoke, rather than through a post-run Summary render — the
// per-scene/per-footswitch Channel-streaming seam is not UI-observable offline; see
// .claude/rules/e2e.md's "The Channel-streaming seam" for the deliberate-seam rationale
// and the sanctioned raw-invoke + /sim/events observation path this file uses instead.
//
// Fixture map (e2e/fixtures/COVERAGE.md): SCENARIO[0] "E2E Rig" (400) carries the Other-
// class wah (WAH, sw8), the unlabeled raw-dB Boost (sw2), the wet-mix SPRING (sw3), and the
// isolated/shared_with_base/lowers_only scene-overlay spread across its 4 scenes.
//
// PR #144 REWORK: the verify-only footswitch default + its "Make level-neutral" opt-in,
// and the scene row's match/offset target-mode chip, are BOTH GONE — every row (Base/Scene/
// Footswitch) now levels against ONE user-chosen `BlockLevelPick` handle (D2). Base rows
// default to the "Preset level" pseudo-option, Scene rows to "Amp output level"; footswitch
// rows always carry a real pre-seeded handle (no pseudo-option). The picker's own trigger
// (`title="Choose this sound's leveling control"`) replaced the old target-mode chip trigger.

interface FootswitchLevelResult {
  switch: number;
  clamped: boolean;
  unconverged: boolean;
  clamp_reason: string | null;
  wet_floor: boolean;
  /** The clamp's cause from the shared taxonomy (mirrors `headroom_trade::ClampKind`) —
   *  see `src/lib/types.ts`'s `ClampKind`/`CLAMP_MESSAGES`. Null when not clamped. */
  clamp_kind: string | null;
  saved: boolean;
  final_value: number;
  predicted_lufs: number;
  method: string; // "baked" | "assigned"
}

interface SetFootswitchAssignmentEvent {
  SetFootswitchAssignment: {
    addr: number;
    index: number;
    function_json: string;
    swap: boolean;
  };
}
function isSetFootswitchAssignment(
  e: unknown,
): e is SetFootswitchAssignmentEvent {
  return typeof e === "object" && e !== null && "SetFootswitchAssignment" in e;
}

// openLevel now lives in ../fixtures/scenario.ts (shared with level-defaults.spec.ts).

/** Dismiss any currently-open Pick/FsParamPick/SceneLevelPick dropdown by clicking its
 *  own backdrop directly (`data-pick-backdrop`, PickPortalMenu.tsx) — a full-card
 *  `inset:0` div with no text content. Clicking a visible-text landmark instead does NOT
 *  work even though the backdrop visually covers it and would receive the click in
 *  effect: Playwright's own actionability check resolves the text locator to the element
 *  BENEATH the backdrop and then refuses to click through the thing covering it, retrying
 *  "<div></div> intercepts pointer events" for the full timeout instead of ever landing
 *  the click. Targeting the backdrop's own element sidesteps the check entirely. Needed
 *  between rows: a still-open menu's backdrop sits above every other row's own trigger and
 *  would otherwise swallow the next click. */
async function closeAnyOpenPicker(page: Page): Promise<void> {
  await page.locator("[data-pick-backdrop]").click();
}

test.describe("Level Setup — Other-class filtering, unlabeled naming (list-level)", () => {
  test.afterEach(async ({ page }) => {
    await reampOff(page);
  });
  test.afterAll(async ({ browser }) => {
    const page = await browser.newPage();
    await clearScenario(page);
    await page.close();
  });

  // COVERAGE rows 22, 19 — unlabeled switch rendering, plus the UI manifestation of the
  // Other-class case.
  //
  // BUG→GATE (user-reported, 2026-08-19, "Friedman HBE"): a footswitch with no level-class
  // parameter used to be filtered out of the Level tab ENTIRELY — the user's "Phaser" switch
  // simply was not there, with nothing saying why. Hiding a control the player can see on the
  // unit is the bug, not the fix: the roster must show every block-acting switch, and a switch
  // that cannot be leveled must SAY SO instead of vanishing.
  //
  // So the tree now renders the FULL roster (`usePresetData`'s `footswitchRoster: "all"`,
  // which `LevelView` opts into) while SELECTABILITY still comes from the one shared
  // `footswitchLevelable` predicate — a non-levelable row is present, labelled "no level
  // control", and its checkbox is disabled. That keeps the danger.md Pick trap closed from the
  // other side: the row can never be picked, so it can never fall back to `options[0]`.
  // The Doctor's own list is deliberately NOT changed (it stays levelable-only — a
  // non-levelable switch has no sound of its own to diagnose); the separation is pinned by
  // `src/__tests__/footswitch-roster-separation.test.tsx`, and the count-vs-buildable
  // agreement by `src/__tests__/footswitch-no-level-control.test.ts`.
  test("400: WAH (Other-class) is SHOWN but not levelable; the unlabeled Boost switch names itself from its block", async ({
    page,
  }) => {
    test.skip(
      await isOnline(page),
      "offline: the fixture's levelable-set shape",
    );
    await ensureScenario(page);
    await openLevel(page);

    const filter = page.getByPlaceholder(/Filter by name or slot/i);
    await filter.fill(SCENARIO[0].name); // E2E Rig
    await page
      .getByTitle(/Show Base/)
      .first()
      .click();

    // The collapsed breakdown counts the WHOLE roster — all 6 block-acting switches, not the
    // 4 levelable ones (DRIVE, the unlabeled Boost, SPRING, VERB KILL). WAH and WAH SWEEP both
    // act on the all-Other-class ACD_CryBabyQ535 and carry zero level candidates, and they are
    // counted here precisely because the user must see that they exist.
    await expect(page.getByText("4 scenes · 6 footswitches")).toBeVisible({
      timeout: 60_000,
    });

    // The unlabeled switch (customLabel: "") falls back to its block's own short name
    // ("Boost", from ACD_Boost) — never a blank row, never an arbitrary options[0].
    await expect(page.getByText("Boost", { exact: true })).toBeVisible();

    // WAH IS present — the user-reported bug was that it was not.
    await expect(page.getByText("WAH", { exact: true })).toBeVisible();
    // Both Other-class switches (WAH sw8, WAH SWEEP) say WHY they cannot be leveled rather
    // than disappearing. Counted, so a regression that drops one row is caught too.
    await expect(page.getByText("no level control")).toHaveCount(2);
    // …and neither can be selected: no pick, so no `options[0]` fallback is reachable
    // (danger.md's Pick trap, closed from the absence side as before).
    await expect(page.getByRole("checkbox", { disabled: true })).toHaveCount(2);
  });
});

test.describe("Level Setup — scene handle picker (isolated / shared_with_base / lowers_only)", () => {
  test.afterEach(async ({ page }) => {
    await reampOff(page);
  });
  test.afterAll(async ({ browser }) => {
    const page = await browser.newPage();
    await clearScenario(page);
    await page.close();
  });

  // COVERAGE rows 10, 12, 13 — all Setup-time (the picker's candidate annotations), never a
  // run. Row 9 (the old target-mode chip's offset mode) is GONE with the chip itself — PR
  // #144 replaced it with the combined D2 handle picker, so there is no more "match target
  // vs keep offset" choice to assert. 400's 4 scenes carry all three overlay scopes for the
  // ACD_Boost handle: Rhythm/Lead/Ceiling are FULL overlays (isolated), Shared is
  // bypass-only (shared_with_base); Ceiling's amp `outputLevel` overlay sits at 1.0 (the
  // range top) — the lowers_only headroom case.
  test("400: Rhythm is isolated, Shared warns shared_with_base, Ceiling annotates lowers_only; picking a handle updates the trigger", async ({
    page,
  }) => {
    test.skip(
      await isOnline(page),
      "offline: the scene overlays are fixture-authored",
    );
    await ensureScenario(page);
    await openLevel(page);

    const filter = page.getByPlaceholder(/Filter by name or slot/i);
    await filter.fill(SCENARIO[0].name);
    await page
      .getByTitle(/Show Base/)
      .first()
      .click();
    for (const scene of ["Rhythm", "Ceiling", "Shared"]) {
      await page.getByText(scene, { exact: true }).click();
    }
    await filter.fill("");

    await page.getByRole("button", { name: /Level 1 preset/ }).click();
    await page.getByText(/I.ve backed up with Pro Control/i).click();

    // Scoped by the row's OWN `data-setup-row` hook (`setupRowHookKey`, leveling.ts):
    // `s400:0` = Rhythm, `s400:2` = Ceiling, `s400:3` = Shared. A scene hook's index is
    // the `scenes[]` array order, which IS the wire sceneSlot already (`chosenFrom`'s
    // "the row index IS the 0-based wire sceneSlot" — see e2e/fixtures/scenario-
    // presets.json's slot-400 `scenes` list) — a true identity, stable under a
    // fixture edit, so it needs no translation the way a footswitch hook does
    // (`f<slot>:sw<n>`, below). The trigger is `BlockLevelPick`'s own (its fixed
    // `title`, not scene-name text — every unselected row's own picker also DEFAULTS
    // to the "Amp output level" pseudo-option, so a text filter on that label would
    // collide across rows too).
    const rowTrigger = (key: string) =>
      page.locator(
        `[data-setup-row="${key}"] div[title="Choose this sound's leveling control"]`,
      );

    // Rhythm (s400:0): ACD_Boost's OWN overlay in this scene is FULL (isolated) — its
    // candidate row must carry no shared_with_base warning. NOTE: the picker's candidate
    // list spans every level/wet-mix node in the graph, not just Boost — TubeScreamer and
    // TwinReverb are ALSO candidates (their own "level"/"outputLevel" params), and Rhythm's
    // overlay for both is bypass-only ({bypass, bypassType} only — see
    // scenario-presets.json's slot-400 scene 0), so THEIR rows legitimately DO warn
    // shared_with_base here. Scope the assertion to Boost's own row
    // (`data-block-param-pick="ACD_Boost:gain"`) rather than the whole menu.
    await rowTrigger("s400:0").click();
    const boostCandidate = page.locator(
      '[data-block-param-pick="ACD_Boost:gain"]',
    );
    // EXISTENCE FIRST. The warning assertion below is absence-only, and `toHaveCount(0)`
    // is equally satisfied by a Boost row that never rendered — a candidate-enumeration
    // regression, or a `data-block-param-pick` rename, would turn this into a test that
    // asserts nothing while staying green. Pin the row's presence, then its cleanliness.
    await expect(
      boostCandidate,
      "the Boost candidate row must be in the menu at all",
    ).toHaveCount(1);
    await expect(
      boostCandidate.getByText(/shared with the base preset/),
    ).toHaveCount(0);
    // Untouched (still the "Amp output level" pseudo-default — this row's own handle was
    // never picked).
    await expect(rowTrigger("s400:0")).toContainText("Amp output level");
    await closeAnyOpenPicker(page);

    // Shared (s400:3): ACD_Boost's overlay is bypass-only in this scene → its OWN row
    // warns. Scoped the same way as Rhythm above — TubeScreamer's overlay is ALSO
    // bypass-only here (scenario-presets.json's slot-400 scene 3: both Boost and
    // TubeScreamer carry only {bypass, bypassType}), so its candidate row legitimately
    // warns too and a whole-menu text assertion would hit a strict-mode collision.
    await rowTrigger("s400:3").click();
    await expect(
      boostCandidate.getByText(
        /shared with the base preset — changes every scene/,
      ),
    ).toBeVisible();
    await closeAnyOpenPicker(page);

    // Ceiling (s400:2): BOTH amps' outputLevel overlay sits at the range top (1.0) in this
    // scene (scenario-presets.json's slot-400 scene 2: ACD_JC120.outputLevel = 1.0 AND
    // ACD_TwinReverb65NoFx.outputLevel = 1.0 — TwinReverb is bypassed here but still
    // carries a full overlay) — so BOTH their candidate rows legitimately annotate "can
    // only lower" and a whole-menu text assertion hits a strict-mode collision. Scope to
    // JC120's own row (Boost's `gain` = 2.5, nowhere near its [0,12] top, is NOT
    // lowers_only here). Then PICK it (the D2 handle choice replacing the old target-mode
    // chip) and confirm the trigger updates to name the chosen block+param.
    await rowTrigger("s400:2").click();
    const jc120Candidate = page.locator(
      '[data-block-param-pick="ACD_JC120:outputLevel"]',
    );
    await expect(jc120Candidate.getByText("can only lower")).toBeVisible();
    await jc120Candidate.click(); // picking closes the menu itself — no closeAnyOpenPicker needed
    await expect(rowTrigger("s400:2")).toContainText("Output level");
  });
});

test.describe("Level Setup — footswitch rows pre-seed a real handle (verify-only removed)", () => {
  test.afterEach(async ({ page }) => {
    await reampOff(page);
  });
  test.afterAll(async ({ browser }) => {
    const page = await browser.newPage();
    await clearScenario(page);
    await page.close();
  });

  // COVERAGE rows 15, 16/17/20's Setup-time half, post-PR-#144: the backend dropped the
  // verify-only footswitch mode entirely, so there is no more "Verify only" tag or
  // "Make level-neutral" opt-in to flip — every row is PRE-SEEDED with the tone-safe
  // `defaultParamIndex` candidate (leveling.ts's `chosenFrom`) at Setup-open time, shown
  // non-interactively when it's the row's only option (mirrors BlockLevelPick's own
  // doc: "a `wet_mix` candidate is flagged...", "the single best candidate...").
  test("400: Boost pre-seeds Gain, SPRING pre-seeds Mix — no verify-only state exists", async ({
    page,
  }) => {
    test.skip(
      await isOnline(page),
      "offline: fixture-authored footswitch shape",
    );
    await ensureScenario(page);
    await openLevel(page);

    const filter = page.getByPlaceholder(/Filter by name or slot/i);
    await filter.fill(SCENARIO[0].name);
    await page
      .getByTitle(/Show Base/)
      .first()
      .click();
    await page.getByText("Boost", { exact: true }).click();
    await page.getByText("SPRING", { exact: true }).click();
    await filter.fill("");

    await page.getByRole("button", { name: /Level 1 preset/ }).click();
    await page.getByText(/I.ve backed up with Pro Control/i).click();

    // Footswitch hooks are keyed by DEVICE SWITCH NUMBER (`setupRowHookKey`,
    // leveling.ts), not filtered-list position: `f400:sw2` = Boost, `f400:sw3` =
    // SPRING (COVERAGE.md rows 20/18).
    const boostRow = page.locator('[data-setup-row="f400:sw2"]');
    const springRow = page.locator('[data-setup-row="f400:sw3"]');

    await expect(boostRow.getByText("Verify only")).toHaveCount(0);
    await expect(springRow.getByText("Verify only")).toHaveCount(0);
    // The D2 trigger names BOTH the switch and the pre-picked param ("BOOST · Gain"/
    // "SPRING · Mix") — never a bare param name, since a footswitch row's candidates can
    // span several nodes and the switch's own name disambiguates them.
    const boostTrigger = boostRow.locator(
      'div[title="Choose this sound\'s leveling control"]',
    );
    const springTrigger = springRow.locator(
      'div[title="Choose this sound\'s leveling control"]',
    );
    // Boost's sole candidate is `gain` — pre-picked.
    await expect(boostTrigger).toContainText("Gain");
    // SPRING's sole candidate is `mix` — pre-picked.
    await expect(springTrigger).toContainText("Mix");
  });
});

test.describe("Level — footswitch opted-in write path (raw invoke, command-level)", () => {
  test.afterEach(async ({ page }) => {
    await reampOff(page);
  });
  test.afterAll(async ({ browser }) => {
    const page = await browser.newPage();
    await clearScenario(page);
    await page.close();
  });

  // COVERAGE rows 3, 17, 20's WRITE-PATH half — and row 3 ONLY that half. Stated plainly
  // because the honest scope is narrower than it looks: 400's `scenario-loudness.json`
  // entry declares `leveledParams` for `ACD_TMSpring63.mix` and NOTHING ELSE, so the
  // offline model is FLAT in `ACD_Boost.gain` — `model_lufs` returns the same C at every
  // probe of it. What that makes provable here is the plumbing: the solve terminates, a
  // resolved `valueA`/`valueB` reaches the wire through the Assign path, the fake confirms
  // by read-back, and the save persists. What it does NOT prove is that the solved gain
  // TRACKS loudness — any target converges against a flat response, so the dry run's
  // `predicted_lufs` is a constant this test then feeds back to itself. Row 3's
  // solve-tracking half is online-only. Do not read a green here as "the block-knob solve
  // is correct"; read it as "the block-knob WRITE lands and persists".
  //
  // 400/switch 2 (Boost) routes through
  // `FsLevelPlan::Assign` (`footswitch.rs`): `ACD_Boost.bypass = false` in base — it's
  // part of the base sound, so a bare block-value bake would change the ALWAYS-ON signal,
  // not just an engaged-only state. SimDevice implements `setFootswitchAssignment`(54) /
  // `clearFootswitchAssignment`(55) / `currentPresetDataRequest`(2) and confirms the
  // assign by READ-BACK (a `currentPresetDataRequest` re-prompt renders the working-copy
  // `ftsw` with the new `param` function — there is no dedicated field-54 echo, matching
  // the wire schema), so the save completes and persists. Mirrors
  // `e2e_server_tests.rs::assign_path_footswitch_confirms_by_readback_and_persists_its_value_a`
  // at the Playwright layer: the dry run learns Boost's reachable engaged loudness (400
  // declares no `leveledParams` for `gain`, so the offline model is flat in it — any signal
  // level converges), the save:true run must complete and persist, and the wire carries
  // the resolved `valueA`/`valueB` at the appended function index. Per this file's header,
  // the RENDERED Summary still can't show this per-row offline — the wire proof is
  // `/sim/events`.
  test("Boost's opted-in gain write reaches the fake via the Assign path, confirms by read-back, and is saved", async ({
    page,
  }) => {
    test.skip(
      await isOnline(page),
      "offline: pins the sim's Assign-confirm behavior",
    );
    await ensureScenario(page);
    const reampBase = await reampCounters(page);

    const apply = (targetLufs: number, save: boolean) =>
      invoke(
        page,
        "level_footswitches_apply",
        {
          slot: SCENARIO[0].slot,
          jobs: [
            {
              switch: 2,
              levGroupId: "G1",
              levNodeId: "ACD_Boost",
              levParameterId: "gain",
              targetLufs,
            },
          ],
          save,
          topologyId: "guitar-humbucker",
          calibrationLufs: null,
          profileId: null,
          onResult: "__CHANNEL__:1",
        },
        LEVEL_T,
      ) as Promise<FootswitchLevelResult[]>;

    const dry = await apply(-20, false);
    expect(dry[0].clamp_reason, "Boost's engaged capture has signal").toBe(
      null,
    );
    expect(Number.isFinite(dry[0].predicted_lufs)).toBe(true);
    expect(dry[0].saved, "a dry run must write nothing").toBe(false);

    const r = (await apply(dry[0].predicted_lufs, true))[0];
    expect(r.method).toBe("assigned");
    expect(r.clamp_reason, "ACD_Boost is on the trunk — no routing clamp").toBe(
      null,
    );
    expect(r.saved, "the Assign save must now complete and persist").toBe(true);
    // A raw-dB gain solve must reach the wire unclamped (the `[0,12]` range's own seed).
    expect(r.final_value).toBeGreaterThan(1);

    const events = await simEvents(page);
    const assigns = events
      .filter(isSetFootswitchAssignment)
      .map((e) => e.SetFootswitchAssignment);
    const boost = assigns.find((a) => a.addr === 2);
    if (!boost)
      throw new Error(
        `no field-54 write for switch 2: ${JSON.stringify(assigns)}`,
      );
    const func = JSON.parse(boost.function_json) as {
      func: string;
      nodeId: string;
      parameterId: string;
      valueA: number;
      valueB: number;
    };
    expect(func.func).toBe("param");
    expect(func.nodeId).toBe("ACD_Boost");
    expect(func.parameterId).toBe("gain");
    expect(Math.abs(func.valueA - r.final_value)).toBeLessThan(1e-3);
    expect(
      Math.abs(func.valueB - 2.5),
      "valueB must be the switch-OFF authored base gain",
    ).toBeLessThan(1e-3);

    await expectReampBalanced(page, reampBase);
  });
});

test.describe("Level — wet-mix footswitch outcome (SPRING, raw invoke)", () => {
  test.afterEach(async ({ page }) => {
    await reampOff(page);
  });
  test.afterAll(async ({ browser }) => {
    const page = await browser.newPage();
    await clearScenario(page);
    await page.close();
  });

  // COVERAGE row 18 — the wet-floor outcome, now offline-provable. It needed two things
  // landed together: `scenario-loudness.json`'s `leveledParams` entry for
  // `400/G1/ACD_TMSpring63/mix` on the `wetMix` curve (so the sim's capture model gives the
  // param real authority instead of reading flat), and `model_lufs`'s widened activation
  // predicate (an Assign's isolation leaves the LEVELED block's own bypass untouched, so
  // the old `bypass_writes[node] == Some(false)` predicate never fired for it). Mirrors
  // `e2e_server_tests.rs::wet_mix_footswitch_pins_at_the_wet_floor_on_an_unreachable_target`
  // + `..._converges_and_stays_off_the_floor_on_a_reachable_target` at the Playwright layer.
  test("an unreachable target pins at the wet floor honestly; a reachable one (learned, not hard-coded) converges and saves", async ({
    page,
  }) => {
    test.skip(await isOnline(page), "offline: pins the sim's wetMix curve");
    await ensureScenario(page);
    const reampBase = await reampCounters(page);

    const apply = (targetLufs: number, save: boolean) =>
      invoke(
        page,
        "level_footswitches_apply",
        {
          slot: SCENARIO[0].slot,
          jobs: [
            {
              switch: 3,
              levGroupId: "G1",
              levNodeId: "ACD_TMSpring63",
              levParameterId: "mix",
              targetLufs,
            },
          ],
          save,
          topologyId: "guitar-humbucker",
          calibrationLufs: null,
          profileId: null,
          onResult: "__CHANNEL__:1",
        },
        LEVEL_T,
      ) as Promise<FootswitchLevelResult[]>;

    // Unreachable (-70, far below the curve's -12.02..-17.18 span): the solve pins at
    // WET_FLOOR_FRACTION x the authored mix (0.25 x 0.42 = 0.105) and reports the honest
    // "quieter ON than OFF, verify by ear" outcome — never a routing clamp (`clamp_reason`
    // stays null; that field's contract is "no signal on USB 1/2", which this capture has).
    // save:false — nothing worth persisting at a floor the target itself never asked for.
    const unreachable = (await apply(-70, false))[0];
    expect(unreachable.method).toBe("assigned");
    expect(unreachable.clamped, "an unreachable target must clamp").toBe(true);
    expect(
      unreachable.wet_floor,
      `the clamp's cause must be the wet floor: ${JSON.stringify(unreachable)}`,
    ).toBe(true);
    expect(unreachable.clamp_reason).toBe(null);
    // The shared ClampKind taxonomy names the SAME cause (CLAMP_MESSAGES.wet_floor in
    // src/lib/types.ts renders this verbatim wherever a row's clamp is UI-observable —
    // this wire-level check is the twin the Channel seam allows offline; see this file's
    // header).
    expect(
      unreachable.clamp_kind,
      `clamp_kind must name the wet floor too: ${JSON.stringify(unreachable)}`,
    ).toBe("wet_floor");
    expect(
      Math.abs(unreachable.final_value - 0.105),
      `the written value must BE the floor: ${JSON.stringify(unreachable)}`,
    ).toBeLessThan(1e-3);

    // Reachable target: LEARNED from a dry run, never hard-coded. `presetLevel` shifts
    // SPRING's whole curve across a run (scenario-loudness.json's own note on the wet-mix
    // row), so a fixed LUFS picked in advance could clamp for reasons unrelated to what
    // this half proves. -16 is only the SEED for the secant search (the level-rerun.spec.ts
    // idiom): what actually gets applied is that seed's own converged/clamped
    // `predicted_lufs`, so this asks "does converging off the floor work", not "does -16
    // happen to still be reachable this run".
    const probe = (await apply(-16, false))[0];
    const target = probe.clamped ? probe.predicted_lufs : -16;
    const reachable = (await apply(target, true))[0];
    expect(reachable.method).toBe("assigned");
    expect(
      reachable.clamped,
      `must actually solve, not clamp: ${JSON.stringify(reachable)}`,
    ).toBe(false);
    expect(reachable.unconverged).toBe(false);
    expect(
      reachable.wet_floor,
      "wet_floor tracks the OUTCOME, not the param's class",
    ).toBe(false);
    expect(
      reachable.clamp_kind,
      "an unclamped row carries no clamp cause",
    ).toBe(null);
    expect(reachable.saved, "an in-range target must persist").toBe(true);

    const events = await simEvents(page);
    const assigns = events
      .filter(isSetFootswitchAssignment)
      .map((e) => e.SetFootswitchAssignment);
    const spring = assigns.find((a) => a.addr === 3);
    if (!spring) {
      throw new Error(
        `SPRING's opted-in mix write must reach the fake: ${JSON.stringify(assigns)}`,
      );
    }
    const func = JSON.parse(spring.function_json) as {
      func: string;
      nodeId: string;
      parameterId: string;
      valueA: number;
    };
    expect(func.func).toBe("param");
    expect(func.nodeId).toBe("ACD_TMSpring63");
    expect(func.parameterId).toBe("mix");
    expect(Math.abs(func.valueA - reachable.final_value)).toBeLessThan(1e-3);

    await expectReampBalanced(page, reampBase);
  });
});
