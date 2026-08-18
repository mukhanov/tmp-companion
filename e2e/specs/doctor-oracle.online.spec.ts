import { test, expect } from "../fixtures/test";
import {
  SCENARIO,
  clearScenario,
  ensureScenario,
  expectReampBalanced,
  invoke,
  isOnline,
  reampCounters,
  reampOff,
} from "../fixtures/scenario";

// COVERAGE row 40 — the executable consumer of the Doctor spectral-check oracle's
// verdict table. E2E Doctor Oracle (407) carries 14 mixed-shape footswitches, one
// per Doctor spectral check, all bypassed/neutral in base. Today the oracle's
// canonical firing verdicts live only in prose (this file's COVERAGE.md row) and in
// the STRUCTURAL fixture-shape gate `fx_doctor_oracle_fires_nothing_in_base_and_
// carries_its_defect_table` (src-tauri/src/lib.rs) — that gate pins the fixture's
// param/node/value identities offline but never engages re-amp, so it cannot prove
// a single switch actually DIAGNOSES anything on the real device. This spec is that
// missing proof: one `doctor_check` batch (base + all 14 switches — the product's
// own one-load-many-sounds shape, not 15 separate runs) asserted against the
// HW-established verdict map (2026-08-17/18, fw 1.8.45, V17 oracle).
//
// Seam-only rows (BOOMY/HARSH/FIZZY/BRIGHT/SPIKY): the lib.rs gate's per-row
// comments record why each one's param/on-off write is accepted by the wire seam
// but produces no positive verdict on the synthetic stimulus through this chain.
// This spec asserts only that those rows come back as a real, unerrored capture
// (a finite `integratedLufs`) — never a spectral verdict.
//
// Scene rows (15/16, "SCENE JUMP") are deliberately OUT of scope: the verdict map
// was tuned for base + footswitch sounds only (COVERAGE.md row 40's own note).
//
// `footswitch` values below are the raw 0-based `ftsw` array indices — the SAME
// row numbers the lib.rs gate's defect table indexes with (`ftsw[idx]`), no
// row-minus-one translation (`DoctorInput.footswitch`'s doc: "0-based `ftsw`
// array index"; the fixture's ftsw[0] is an EMPTY row, its rows start at 1).
//
// Doctor forces presetLevel 0.5 at capture (doctor_check's per-capture reference
// level); base measures ≈ −16.45 LUFS at that level (HW note, informational only —
// this spec does not assert an exact LUFS, only diag kinds and "did it capture").
test.describe("Doctor oracle — spectral verdicts (E2E Doctor Oracle, 407)", () => {
  test.afterEach(async ({ page }) => {
    // Re-amp OFF rescue FIRST — a mid-test failure before the balance gate must not
    // strand the unit input-muted (clearScenario's own reampOff runs last).
    await reampOff(page);
  });
  test.afterAll(async ({ browser }) => {
    const page = await browser.newPage();
    await clearScenario(page);
    await page.close();
  });

  const ORACLE = SCENARIO[7]; // E2E Doctor Oracle, slot 407 — verified against fixtures/scenario.ts

  interface DoctorSoundResult {
    key: string;
    diags: { key: string }[];
    integratedLufs: number;
    error: string | null;
  }
  interface DoctorPresetResult {
    listIndex: number;
    sounds: DoctorSoundResult[];
  }
  interface DoctorCheckResult {
    presets: DoctorPresetResult[];
    stopped: boolean;
  }

  type Verdict =
    | { kind: "zero" } // negative control: diags must be empty
    | { kind: "contains"; diagKey: string } // must fire this kind (co-fire allowed)
    | { kind: "seam" }; // wire-seam-only: assert a real, unerrored capture only

  // The 14 switches, in `ftsw` row order (== the 0-based `footswitch` value to
  // send, per the index conclusion above). One entry per
  // src-tauri/src/lib.rs's `fx_doctor_oracle_fires_nothing_in_base_and_carries_
  // its_defect_table` `table` row.
  const SWITCHES: { row: number; label: string; verdict: Verdict }[] = [
    { row: 1, label: "CONTROL", verdict: { kind: "zero" } },
    { row: 2, label: "MUDDY", verdict: { kind: "contains", diagKey: "muddy" } },
    { row: 3, label: "BOOMY", verdict: { kind: "seam" } },
    { row: 4, label: "HARSH", verdict: { kind: "seam" } },
    { row: 5, label: "FIZZY", verdict: { kind: "seam" } },
    { row: 6, label: "LOST", verdict: { kind: "contains", diagKey: "lost" } },
    { row: 7, label: "BRIGHT", verdict: { kind: "seam" } },
    { row: 8, label: "CUTTHRU", verdict: { kind: "zero" } },
    {
      row: 9,
      label: "RESONANT",
      verdict: { kind: "contains", diagKey: "resonant" }, // harsh may co-fire
    },
    { row: 10, label: "BOXY", verdict: { kind: "contains", diagKey: "boxy" } },
    { row: 11, label: "THIN", verdict: { kind: "contains", diagKey: "thin" } },
    { row: 12, label: "DARK", verdict: { kind: "contains", diagKey: "dark" } },
    {
      row: 13,
      label: "WASHED",
      verdict: { kind: "contains", diagKey: "washed" }, // lost may co-fire
    },
    { row: 14, label: "SPIKY", verdict: { kind: "seam" } },
  ];

  test("base fires nothing; each switch's own spectral verdict fires; seam-only rows just capture", async ({
    page,
  }) => {
    test.skip(
      !(await isOnline(page)),
      "online-only: the oracle's verdicts need real audio",
    );
    // Budget arithmetic (mirrors level-rerun.spec.ts:193's style):
    //   15 sounds (1 base + 14 switches) x 18 s/capture (doctor.spec.ts's own
    //   documented 12-18 s/capture online range, worst end) = 270 s.
    //   + up to 3 floor-suspect retries (FLOOR_RETRY_GAP_MS 5 s + one recapture
    //     18 s each, leveller.rs) = 69 s.
    //   + ONE live field-8 isolation read for the whole run (nodes/footswitches
    //     are sent empty below, so every sound falls to the cached-per-list-index
    //     legacy read) + its RECONNECT_GAP_MS settle = ~5 s.
    //   + a cold `ensureScenario` seed, worst case (its own 240_000 ms request
    //     timeout) = 240 s.
    //   + the run-end restore-active-preset reload = ~5 s.
    //   Baseline ~= 270 + 69 + 5 + 240 + 5 = 589 s. >=2x headroom, PLUS the ceiling
    // must exceed worst-case ensureScenario (240 s) + the request budget below
    // (1080 s) = 1320 s, or a cold-seed run would die on the vaguer test timeout
    // before the request timeout could name the hang => 1_500_000 ms.
    test.setTimeout(1_500_000);
    // Single request timeout for the one doctor_check call, kept below the test's
    // own ceiling so a genuine hang reports as a clear "request timeout" rather
    // than the vaguer "Test timeout exceeded", with slack left for
    // ensureScenario/teardown either side of it.
    const DOCTOR_T = 1_080_000;

    await ensureScenario(page);
    const reampBase = await reampCounters(page);

    // Nodes/footswitches sent empty: `resolve_sound_isolation`
    // (src-tauri/src/commands/doctor.rs) falls back to the legacy live field-8
    // read (cached per list index) whenever a sound's `nodes` is empty — this
    // spec raw-invokes the command directly rather than replaying a backup scan,
    // so it exercises that fallback path deliberately, same as
    // doctor-apply.online.spec.ts's raw jobs.
    const baseItem = {
      key: "base",
      listIndex: ORACLE.slot,
      scene: null as number | null,
      footswitch: null as number | null,
      label: "Base",
      tag: null as string | null,
      topologyId: "guitar-humbucker",
      calibrationLufs: null as number | null,
      profileId: null as string | null,
      nodes: [] as unknown[],
      footswitches: [] as unknown[],
    };
    const items = [
      baseItem,
      ...SWITCHES.map((s) => ({
        ...baseItem,
        key: `fs${String(s.row)}`,
        footswitch: s.row,
        label: s.label,
      })),
    ];

    const result = (await invoke(
      page,
      "doctor_check",
      {
        items,
        restoreListIndex: null,
        onResult: "__CHANNEL__:1",
      },
      DOCTOR_T,
    )) as DoctorCheckResult;

    expect(result.stopped, "the run must complete, not stop early").toBe(false);
    expect(result.presets.length, "exactly one preset was checked").toBe(1);
    const preset = result.presets[0];
    expect(preset, "preset result must be present").toBeDefined();
    expect(preset.listIndex).toBe(ORACLE.slot);

    // Non-vacuous: every requested sound must come back, in the order requested
    // (doctor_check preserves original item order — see its own "Preserve the
    // original sound/preset order" comment) — a mid-run drop must fail here,
    // mirroring level-strict.spec.ts's "no silent mid-batch drop" style.
    expect(
      preset.sounds.map((s) => s.key),
      "all 15 requested sounds must come back, none dropped mid-run",
    ).toEqual(items.map((i) => i.key));

    // No sound may have failed its capture — a failure would make its diags
    // vacuously empty and silently pass a "zero diags" row.
    for (const sound of preset.sounds) {
      expect(sound.error, `${sound.key} must not have errored`).toBeNull();
    }

    // The order-equality assertion above proves positional identity, so the sounds
    // are indexed directly: [0] = base, [i + 1] = SWITCHES[i].
    const diagKeys = (s: DoctorSoundResult): string[] =>
      s.diags.map((d) => d.key);

    // Base: zero diags (the fixture's whole point — every defect block rides
    // enabled-neutral in base).
    expect(diagKeys(preset.sounds[0]), "base must fire nothing").toEqual([]);

    for (const [i, s] of SWITCHES.entries()) {
      const sound = preset.sounds[i + 1];
      const keys = diagKeys(sound);
      switch (s.verdict.kind) {
        case "zero":
          expect(
            keys,
            `${s.label} (switch ${String(s.row)}) is a negative control — zero diags`,
          ).toEqual([]);
          break;
        case "contains":
          expect(
            keys.includes(s.verdict.diagKey),
            `${s.label} (switch ${String(s.row)}) diags [${keys.join(", ")}] must contain "${s.verdict.diagKey}"`,
          ).toBe(true);
          break;
        case "seam":
          // Seam-only: the write lands, but no spectral verdict is expected —
          // assert only that a real capture happened (finite LUFS, no error —
          // error already checked above). See this file's header for why each
          // of these five is inert/gate-blocked on the synthetic stimulus.
          expect(
            Number.isFinite(sound.integratedLufs),
            `${s.label} (switch ${String(s.row)}) must have captured real audio (finite integratedLufs)`,
          ).toBe(true);
          break;
      }
    }

    await expectReampBalanced(page, reampBase);
  });
});
