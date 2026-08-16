import { test, expect } from "../fixtures/test";
import {
  SCENARIO,
  clearScenario,
  ensureScenario,
  expectReampBalanced,
  isOnline,
  reampCounters,
  reampOff,
} from "../fixtures/scenario";

// Doctor journey — runs identically offline (fake re-amp: the sim's physics model
// scales the stimulus by each sound's sidecar C, so every sound measures finite —
// levels differ per scene, spectra don't) and online (real re-amp captures,
// ~15 s/sound). The oracle is the FLOW: select → set up → run →
// auto-advance → a Results page that renders every checked preset with either
// diagnosis cards or "All clear". Diagnosis CONTENT is sound-dependent, so the spec
// never asserts a specific tag; the prescription-content regressions (existing-comp
// advisory, presetLevel preservation) are backend-validated in doctor.rs unit tests
// and the probe/HW lane, not here.
//
// ONLINE seeding note: scripts/e2e.sh seeds the scenario via `probe --seed-scenario`
// BEFORE the server starts (fresh-process seeding dodges the in-process 0xe00002c5
// open lockout that aborted in-spec seeds) and POSTs `e2e_mark_seeded`, which arms the
// server's verified-seed flag; `ensureScenario` here always calls `e2e_seed_scenario`
// online, which fast-no-ops on that flag and only pays the full ownership-verified
// in-process seed on direct playwright runs (or after a clear). If the runner's seed
// fails all attempts, check nothing else holds the device (Pro Control, a stale
// server/app), rest a minute, rerun.
test.describe("Doctor — select, check, results", () => {
  test.afterEach(async ({ page }) => {
    // Re-amp OFF rescue FIRST — a mid-test failure before the balance gate must not strand
    // the unit input-muted (clearScenario's own reampOff is last, after minutes of clears).
    await reampOff(page);
    await clearScenario(page);
  });

  // COVERAGE row 28 — Doctor's as-played scenes (400/401/402 all selected, exercising
  // the scene/footswitch doctor paths).
  test("checks three presets end to end and lands on Results", async ({
    page,
  }) => {
    // ~29 real sounds across the three intact fixtures (E2E Rig + Pedalboard + Edge, base +
    // scenes + footswitches) at ~12-18 s/capture online, plus the pristine-check reseed and
    // backup scan before the run even starts — worst case ≈ 900 s, matching the terminal
    // wait below; the budget here adds headroom on top.
    test.setTimeout(1_200_000);
    await ensureScenario(page);
    const reampBase = await reampCounters(page);

    await page.goto("/");
    await page.getByRole("button", { name: /backed up/i }).click(); // startup disclaimer
    await expect(page.getByText(/connected · \d+\.\d+/)).toBeVisible({
      timeout: 20_000,
    });

    await page.getByRole("button", { name: "Doctor" }).click();

    // Select E2E Pedalboard + E2E Edge (401/402) AND E2E Rig (400, base + 4 scenes +
    // block-acting footswitches) so the run exercises the scene/footswitch doctor paths
    // too — the sound count is scenario-shape-dependent, so the buttons match on
    // /\d+ sounds/.
    const filter = page.getByPlaceholder(/Filter by name or slot/i);
    const picked = [SCENARIO[0], SCENARIO[1], SCENARIO[2]];
    for (const p of picked) {
      await filter.fill(p.name);
      await page.getByTitle("Select preset to check").first().click();
    }
    await filter.fill("");

    await page.getByRole("button", { name: /Check \d+ sounds/ }).click();

    // Set up: keep the defaults, run.
    await page.getByRole("button", { name: /Run check on \d+ sounds/ }).click();

    // The run auto-advances to Results on a natural finish. Progress events don't stream
    // over the bridge, so the only signal is the terminal Results page — but a rejected run
    // (backend/IPC failure) renders LoadErrorPane's "The check couldn't finish: …" within
    // seconds instead (DoctorView.tsx). The error pane is a fast-fail: match it in the wait
    // so a dropped device reports in seconds, then assert it wasn't the branch we took.
    await expect(
      page
        .getByText(/presets? need a look|All clear|check couldn.t finish/)
        .first(),
    ).toBeVisible({ timeout: 900_000 });
    await expect(page.getByText(/check couldn.t finish/)).toHaveCount(0);

    // The default "Needs a look" filter HIDES fully-clean presets (DoctorResults
    // `shown`), so flip to "Everything" first when the filter strip is present (it
    // only renders when there is a clean preset to hide; on all-clear every card
    // already shows). The pill is a SegmentedControl → role="radio", not "button".
    const everything = page.getByRole("radio", { name: "Everything" });
    if (await everything.isVisible().catch(() => false)) {
      await everything.click();
    }

    // Every checked preset renders on Results — a card, flagged or clean.
    for (const p of picked) {
      await expect(page.getByText(p.name).first()).toBeVisible();
    }

    // STRICT diagnosis oracle (online-only — the offline fake capture is a
    // stimulus passthrough, so no real spectrum exists to diagnose): the checked
    // set carries one KNOWN defect and two known-healthy controls, and the
    // Doctor must call both sides correctly.
    //  * "E2E Edge" (402) ships a fixture-injected post-cab EQ-5 parametric with
    //    two stacked +12 dB Q14 peaks at 2.6 kHz (preserved byte-verbatim from the
    //    pre-P4-A "E2E Target 2") — the calibration suite's `resonant_peq`
    //    DefectRecipe (its Q14 saturation ceiling ≈17 dB clears the resonant
    //    height gate on any chain) — and MUST be flagged with the localized
    //    "Rings at N kHz" resonant chip. Resonant is the tilt-robust oracle: it
    //    fires on the transfer's LOCAL octave-median-envelope excess, so the
    //    bright/scooped broadband sweep every scenario chain measures (which
    //    silences broadband verdicts like Muddy) cannot mask it. The anchored
    //    regex tolerates PSD peak-fit wobble in the decimal and skips the Rx
    //    title (which embeds the same phrase); broadband side-effects of the
    //    +12 dB stack (harsh/fizzy) may co-fire and stay unasserted.
    //  * AS-PLAYED SEMANTICS: the EQ ring lives in E2E Edge's BASE graph, so it is
    //    present in every one of Edge's played sounds — base, all 8 scenes, and its
    //    footswitch rows — not one isolated row. A page-wide `toHaveCount(1)` oracle
    //    predates the as-played rework (when Doctor forced every diagnosis to a single
    //    all-off baseline) and is stale under it: the chip legitimately renders once
    //    PER Edge sound. The oracle below instead asserts (a) ZERO ring chips outside
    //    Edge's own card — scoped via `data-preset-card`, the same e2e hook
    //    `copy.spec.ts` uses for per-target scoping — and (b) INSIDE Edge's card the
    //    chip count equals the count of `data-sound-row`s AFTER expanding the
    //    collapsed "N sounds check out" healthy bucket (`PresetResultCard.tsx`'s
    //    `showHealthy` strip, present whenever any sound on the card IS flagged): a
    //    healthy sound renders NO `data-sound-row` while collapsed, so without the
    //    expand step a sound that stops ringing would drop out of BOTH sides of the
    //    equality and a 1–2 sound regression would pass silently. The denominator is
    //    therefore ALL of Edge's played sounds, not a hardcoded number — any sound
    //    that goes un-flagged breaks the equality.
    //  * Opening the defect preset's own sound row must surface the RIGHT
    //    prescription — the cut at the EQ-10 band nearest the MEASURED ring
    //    (2.5–2.7 kHz all map log-nearest to the 2 kHz band; the chain owns a
    //    parametric EQ, so the Rx is the point-at-your-EQ advisory).
    if (await isOnline(page)) {
      // Hedged accepted too: severity < 1.0 renders "Possible Rings at N kHz" via
      // possibleLabel() (severity.ts), and TubeScreamer-engaged Edge rows can erode the
      // margin into hedge territory — a hedged detection is still a detection.
      const ringChip = /^(?:Possible )?Rings at 2\.\d kHz$/;
      const edgeCard = page.locator(`[data-preset-card="${picked[2].name}"]`);
      for (const p of [picked[0], picked[1]]) {
        const card = page.locator(`[data-preset-card="${p.name}"]`);
        // Non-vacuous: an attribute/name drift must not make the zero-count check
        // below pass trivially against a card that doesn't exist.
        await expect(card).toHaveCount(1);
        await expect(card.getByText(ringChip)).toHaveCount(0);
      }
      // Expand the collapsed healthy bucket (if the strip is present) BEFORE
      // counting — see the AS-PLAYED SEMANTICS note above.
      const healthySummary = edgeCard.getByText(/\d+ sounds? checks? out/);
      if (await healthySummary.isVisible().catch(() => false)) {
        await healthySummary.click();
      }
      const edgeRowCount = await edgeCard.locator("[data-sound-row]").count();
      expect(edgeRowCount).toBeGreaterThanOrEqual(9);
      await expect(edgeCard.getByText(ringChip)).toHaveCount(edgeRowCount);

      // Click the ROW HEADER's label, scoped inside a [data-sound-row] — a bare
      // page-wide getByText(name).last() resolves into the "Level jumps" advisory
      // panel below the rows (its body text repeats "E2E Edge · SCREAMER"), which
      // expands nothing (HW run 2026-08-09). The panel renders wherever authored
      // scene deltas exceed the 3 dB jump threshold — in BOTH tiers (the offline
      // sim models per-scene C from scenario-loudness.json), so it is a permanent
      // feature of intact-Edge results. Keep the click on the label INSIDE the
      // header: the toggle handler lives on the inner header div, and once the row
      // is open the outer [data-sound-row]'s center lands in the expanded body.
      const edgeRingRow = edgeCard.locator("[data-sound-row]").first();
      await edgeRingRow.getByText(picked[2].name).first().click();
      await expect(
        page.getByText(/Rings at 2\.\d kHz — cut the 2 kHz band/).first(),
      ).toBeVisible();
      // Collapse the defect row again so the cut-through assertion below can only
      // resolve against picked[1]'s freshly expanded row, never this one.
      await edgeRingRow.getByText(picked[2].name).first().click();
      await expect(
        page.getByText(/Rings at 2\.\d kHz — cut the 2 kHz band/),
      ).toHaveCount(0);
    }

    // Expanding any measured sound row surfaces the cut-through estimate —
    // `cutThrough` is non-null for every successful guitar capture, so this is
    // deterministic (unlike diagnosis content, which stays unasserted). Scope the
    // click inside the card's first [data-sound-row] header — a page-wide
    // getByText(name) can resolve into advisory-panel body text (see the Edge
    // ring-row click above for the incident).
    await page
      .locator(`[data-preset-card="${picked[1].name}"] [data-sound-row]`)
      .first()
      .getByText(picked[1].name)
      .first()
      .click();
    await expect(
      page.getByText("Cut-through (estimated)").first(),
    ).toBeVisible();

    // Standing re-amp-OFF safety gate — the doctor capture path must disengage re-amp
    // at least as often as it engages (checked before any teardown rescue).
    await expectReampBalanced(page, reampBase);
  });

  // COVERAGE rows 32, 33 (leveling-damage advisories). Deterministic offline:
  // `LevelingDamageRow`'s hints are a BACKUP-SCAN read (zero device captures — see
  // src/views/doctor/LevelingDamageRow.tsx's own header), not diagnosis content, so this is
  // an exception to this file's "diagnosis content is sound-dependent, never asserted
  // directly" rule.
  //
  // MATRIX MISMATCH (row 35, the SNR "some checks skipped" badge): COVERAGE.md lists it
  // Offline, on the premise that 402's "Quiet" scene (sidecar C=-48) reads quiet enough
  // offline to trip the coverage gate. Empirically it doesn't — an actual offline run
  // here renders a normal diagnosis on the Quiet scene ("Gets lost in the mix"), no
  // coverage-gated badge. The cause is NOT a missing C model (the offline capture DOES
  // ride scenario-loudness.json via the same audio::reamp_capture seam): coverage
  // compares the body PSD against the SAME capture's pre-onset floor, and the offline
  // scale_stimulus scales body and preamble uniformly, so coverage is level-invariant
  // offline however quiet the scene. This row is backend/online only.
  test("400 surfaces its two leveling-damage advisories (backup-scan-only, zero captures)", async ({
    page,
  }) => {
    await ensureScenario(page);
    const reampBase = await reampCounters(page);

    await page.goto("/");
    await page.getByRole("button", { name: /backed up/i }).click();
    await expect(page.getByText(/connected · \d+\.\d+/)).toBeVisible({
      timeout: 20_000,
    });

    await page.getByRole("button", { name: "Doctor" }).click();
    const filter = page.getByPlaceholder(/Filter by name or slot/i);
    await filter.fill(SCENARIO[0].name); // E2E Rig
    await page.getByTitle("Select preset to check").first().click();
    await filter.fill("");

    await page.getByRole("button", { name: /Check \d+ sounds/ }).click();
    await page.getByRole("button", { name: /Run check on \d+ sounds/ }).click();
    await expect(
      page.getByText(/presets? need a look|All clear/).first(),
    ).toBeVisible({ timeout: 240_000 });

    const everything = page.getByRole("radio", { name: "Everything" });
    if (await everything.isVisible().catch(() => false)) {
      await everything.click();
    }

    // E2E Rig's card: the "Leveling damage" finding row, its 2-assignment chip, and — once
    // expanded — both signatures' factual copy (VERB KILL's deletedEffect, WAH SWEEP's
    // sweptOther). This is read straight from the backup scan, not from any capture.
    await expect(page.getByText("Leveling damage").first()).toBeVisible();
    await expect(
      page.getByText(/2 assignments worth checking/).first(),
    ).toBeVisible();
    await page.getByText("Leveling damage").first().click();
    await expect(
      page.getByText(/drops to ~0 when engaged — the effect goes silent/),
    ).toBeVisible();
    await expect(
      page.getByText(/isn.t a level control — engaging it changes tone/),
    ).toBeVisible();

    await expectReampBalanced(page, reampBase);
  });
});
