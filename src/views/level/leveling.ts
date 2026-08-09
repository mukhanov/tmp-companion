// src/views/level/leveling.ts — types + helpers for the unified leveling flow.
//
// The unit of leveling is a SCENE. A preset's BASE scene carries cross-preset
// loudness ("levels this preset against the others" → preset `presetLevel`); an FS
// scene is leveled within its preset ("levels this scene against the preset's base"
// → amp `outputLevel` in scene mode). The mechanism is never exposed: no block /
// parameter selector, the target is implicit and fixed.
//
// SELECTION lives in the list (the scene tree): the source of truth is a flat set of
// scene KEYS — Base = `p${slot}`, FS scene = `s${slot}:${idx}`. `chosenFrom` turns
// that set into the SetupOption[] the setup dialog configures (instrument + target).
//
// Flow (one persistent wizard, body swaps per stage): setup (set instrument + target
// for everything picked in the list; its footer's backup acknowledgment gates the
// commit) → run (steps the chosen scenes) → summary.

import type {
  FootswitchInfo,
  LevelJob,
  LevelParamCandidate,
  ParamClass,
  Profile,
  SceneInfo,
  SilenceHint,
} from "../../lib/types";
import type { PresetRow } from "../PresetList";
import type { PickOption } from "../overlays/Pick";
// `SceneHandlePick` is declared ONCE, as the `levelScenesApplyBatched` wire type in
// invoke.ts (its `SceneLevelJobWire.handle` field) — re-exported below so
// `SetupOption`/`RunItem` and the scene-handle-picker wiring can import it from
// either module without a second, driftable copy.
import type { SceneHandlePick } from "../../lib/invoke";
import { shortFallback } from "../../models/blockArt";
import { signedDb, slotLabel } from "../../lib/format";

export type { SceneHandlePick };

// ── selection scene-key helpers (shared by the list + the flow) ─────────────

/** The wire scene slot the device uses for a preset's BASE (a constant, NOT a `scenes[]`
 *  index — mirrors `session::BASE_SCENE_SLOT`). Redistribution levels the base amp at this
 *  slot alongside the FS scenes. Re-exported from the wire mirror so the value can't drift. */
export { BASE_SCENE_SLOT } from "../../lib/types";

/** The Base scene key for a preset slot (selecting the whole preset includes it). */
export const baseKey = (slot: number): string => `p${String(slot)}`;
/** The key for the i-th (0-based) footswitch scene of a preset slot. */
export const sceneKeyOf = (slot: number, i: number): string =>
  `s${String(slot)}:${String(i)}`;
/** The key for the i-th (0-based) levelable FOOTSWITCH of a preset slot. `i` indexes
 *  the SAME levelable footswitch list everywhere (the backup-cached, level-params-
 *  filtered one), so the key is stable across the list, selection, and the flow. */
export const fswKey = (slot: number, i: number): string =>
  `f${String(slot)}:${String(i)}`;
/** Every selectable child key for a preset: Base, then one per FS scene, then one per
 *  levelable footswitch. Scenes and footswitches share the key space (distinct prefix). */
export function childKeys(
  slot: number,
  scenes: SceneInfo[],
  footswitches: FootswitchInfo[],
): string[] {
  return [
    baseKey(slot),
    ...scenes.map((_, i) => sceneKeyOf(slot, i)),
    ...footswitches.map((_, i) => fswKey(slot, i)),
  ];
}

/** The leveling coordinates a footswitch row carries into `levelFootswitchesApply`:
 *  the `ftsw` switch index + (in LEVEL mode) the block param to solve. Built from
 *  `FootswitchInfo` (switch + a chosen level candidate).
 *
 *  A discriminated union on `mode`, not three independently-nullable `lev*` fields —
 *  the old shape let `{mode:"verify", levGroupId:"x"}` exist at the type level,
 *  forcing every consumer to triple-null-check instead of narrowing on `mode`. LEVEL
 *  mode's three coords are always ALL present together (mirrors the backend's
 *  `FsJobMode`, which never sends a partial handle either).
 *
 *  `switchIndex` stays populated in BOTH branches, so `it.footswitch != null` keeps
 *  meaning "this is a footswitch row" everywhere it's checked (`ceilingOf`, the run
 *  loop's dispatch) regardless of whether the row has a handle yet. */
export type FootswitchTarget =
  | {
      /** "verify" — measure engaged vs disengaged, write nothing (the P2 default: a
       *  row is only WRITTEN once the user has explicitly given it a handle in Set
       *  up, never silently on a tone-safe auto-pick). */
      mode: "verify";
      /** 0-based `ftsw` array index (the wire footswitch address). */
      switchIndex: number;
    }
  | {
      /** "level" — solve + write the handle below (opt-in). */
      mode: "level";
      switchIndex: number;
      levGroupId: string;
      levNodeId: string;
      levParameterId: string;
    };

/** Rank a candidate's WIRE-CARRIED class for `defaultParamIndex`: a genuine level
 *  control (linear or dB) ranks above a wet/dry mix (which changes loudness but also
 *  the effect's presence). There is no "unclassified" rank to skip — the backend
 *  (`footswitch::level_candidates_for_node`) admits only params the classifier
 *  recognises, so every candidate the frontend ever sees already carries a real class. */
const CLASS_RANK: Record<ParamClass, number> = {
  level_linear: 0,
  level_db: 0,
  wet_mix: 1,
};

/** Friendly labels for the technical parameter ids (fallback: capitalize the id).
 *  Shared by `FsParamPick` and `SceneLevelPick` — the two "which block parameter"
 *  pickers — so the dictionary can't drift between them. */
const PARAM_LABELS: Partial<Record<string, string>> = {
  level: "Level",
  outputLevel: "Output level",
  output: "Output",
  mix: "Mix",
  volume: "Volume",
  gain: "Gain",
  drive: "Drive",
  tone: "Tone",
  fuzz: "Fuzz",
  treble: "Treble",
  bass: "Bass",
  presence: "Presence",
};
export function paramLabel(p: string): string {
  return PARAM_LABELS[p] ?? (p ? p.charAt(0).toUpperCase() + p.slice(1) : "");
}

/** The tone-safe default candidate index: the best-classified (level over wet-mix)
 *  candidate off the WIRE `class` field, tie-broken to the first match. Returns `-1`
 *  only for an EMPTY list — every candidate the backend offers already carries a real
 *  (never "other") class, so a non-empty list always has a valid default; callers with
 *  a non-empty `params` can treat the result as always `>= 0`. */
export function defaultParamIndex(params: LevelParamCandidate[]): number {
  let bestIdx = -1;
  let bestRank = Number.POSITIVE_INFINITY;
  params.forEach((c, i) => {
    const rank = CLASS_RANK[c.class];
    if (rank < bestRank) {
      bestRank = rank;
      bestIdx = i;
    }
  });
  return bestIdx;
}

/** The apply-to-all instrument's place on the good → better → best ladder that drives
 *  the Set up step's instrument nudge: `none` (no instrument → levels against the
 *  default reference) → `uncal` (instrument, no stored calibration) → `cal`
 *  (calibrated). An unknown / empty id is treated as `none`. */
export function instCalState(
  id: string,
  options: PickOption[],
): "none" | "uncal" | "cal" {
  if (!id || id === "none") return "none";
  const o = options.find((x) => x.id === id);
  if (!o) return "none";
  return o.calibrated ? "cal" : "uncal";
}

/** Build a LEVEL-mode target from a specific candidate (the user's explicit pick). The
 *  backend classifies bake vs assign from these ids. */
export function targetFromCandidate(
  switchIndex: number,
  c: LevelParamCandidate,
): FootswitchTarget {
  return {
    switchIndex,
    levGroupId: c.group_id,
    levNodeId: c.node_id,
    levParameterId: c.parameter_id,
    mode: "level",
  };
}

/** Build a VERIFY-mode target (no handle) — the row's default until the user opts in
 *  with an explicit pick. */
export function verifyFootswitchTarget(switchIndex: number): FootswitchTarget {
  return { switchIndex, mode: "verify" };
}

/** The display footswitch number for a switch index (human FS tag = index + 1 — the
 *  same +1 scene rows use, verified against `footswitch::scene_fs_map`). */
const fsTagOf = (switchIndex: number): string => `FS${String(switchIndex + 1)}`;

/** The instrument `Pick` options shared by the Level and Doctor setup steps:
 *  "None" (the no-instrument path — level/diagnose against the default reference)
 *  followed by each saved profile, calibrated ones flagged with their reference dB. */
export function instrumentOptions(
  profiles: Profile[] | undefined,
): PickOption[] {
  return [
    { id: "none", label: "None" },
    ...(profiles ?? []).map((p) => {
      const cal = p.calibration_lufs;
      return {
        id: p.id,
        label: p.name,
        sub: cal != null ? `${cal.toFixed(1)} dB` : undefined,
        calibrated: cal != null,
      };
    }),
  ];
}

/** Resolve an instrument profile id → its display name (the run-row chip); falls
 *  back to the raw id for an unknown/removed profile. */
export function instrumentName(
  profiles: Profile[] | undefined,
  id: string,
): string {
  return (profiles ?? []).find((p) => p.id === id)?.name ?? id;
}

/** The row name for a footswitch: the player's own `customLabel` when set, else the
 *  toggled block's friendly name (many presets leave the label blank — a nameless row
 *  is useless, so fall back to e.g. "Tube Screamer" from the leveled block's id).
 *
 *  NOT display-only: this string flows into `displayLabel`, which the backend writes
 *  as the switch's on-device `customLabel` on save — a wrong pick here is a WIRE WRITE,
 *  not cosmetic. So the fallback never reaches for an arbitrary candidate: it names the
 *  row after the tone-safe DEFAULT level param (the same one Set up recommends), and
 *  only falls further back to the switch's own toggled/adjusted block (its first
 *  function's `fender_id`) when there is no classifiable level param at all. */
/** The fallback row name for an UNLABELED switch, given the candidate that will actually
 *  be leveled — the block that candidate lives on.
 *
 *  Split out of [`footswitchName`] because the name is chosen TWICE at different moments:
 *  once at list-build time (`chosenFrom`, which only knows the tone-safe DEFAULT candidate)
 *  and again at run-start, if the user overrode that default in Set up. The second call is
 *  not cosmetic — the string reaches `displayLabel`, which the backend writes as the
 *  switch's on-device `customLabel` when an assign appends a function to an unlabeled
 *  switch. Naming it after the default while leveling something else is a WIRE WRITE that
 *  mislabels the player's pedalboard. */
export function footswitchNameForCandidate(c: LevelParamCandidate): string {
  return shortFallback(c.fender_id);
}

export function footswitchName(f: FootswitchInfo): string {
  const label = f.label.trim();
  if (label) return label;
  if (f.level_params.length > 0) {
    // Every candidate the backend offers already carries a real (never "other") class,
    // so `idx` is always >= 0 here in practice — the `undefined` guard exists only so a
    // future loosening of that backend guarantee fails to the function fallback below
    // instead of silently naming the row after `level_params[0]`.
    const idx = defaultParamIndex(f.level_params);
    const picked = idx >= 0 ? f.level_params[idx] : undefined;
    if (picked) return footswitchNameForCandidate(picked);
  }
  if (f.functions.length > 0) return shortFallback(f.functions[0].fender_id);
  return "Footswitch";
}

/** Resolve a levelable footswitch's DEFAULT row target: VERIFY (P2 — a row is only
 *  ever written once the user has explicitly given it a handle in Set up; the old
 *  auto-pick-and-write default is gone). `null` when the footswitch has no leveling
 *  candidate at all (it should have been filtered out upstream — nothing to verify OR
 *  level). */
function footswitchTarget(f: FootswitchInfo): FootswitchTarget | null {
  // Length-guard rather than `!candidate` — the array index type lies (no
  // noUncheckedIndexedAccess), so the truthiness check reads as "always truthy".
  if (f.level_params.length === 0) return null;
  return verifyFootswitchTarget(f.switch);
}

// ── setup: one selectable row (Base or an FS scene) ─────────────────────────

export interface SetupOption {
  /** Unique key: `p${slot}` for Base, `s${slot}:${idx}` for a scene. */
  key: string;
  /** 0-based list index of the owning preset. */
  slot: number;
  presetName: string;
  /** Base scene (cross-preset) vs an FS scene (within-preset). */
  isBase: boolean;
  /** The `loadScene` / `level_scenes_apply` wire slot (0-based scenes[] index);
   *  null for the Base/whole-preset row (which levels `presetLevel`). */
  sceneSlot: number | null;
  /** Display name: "Base Preset" / "Whole preset" / the scene name. */
  sceneName: string;
  /** Tag chip: "BASE" | `FS${n}` | null (em-dash for an untagged named scene). */
  tag: string | null;
  /** False ⇒ a scene-less preset, whose Base row renders "Whole preset". */
  hasScenes: boolean;
  /** Set ⇒ this row is a block-acting FOOTSWITCH (not Base/scene); carries the coords
   *  for `levelFootswitchesApply`. null/undefined for Base + scene rows. */
  footswitch?: FootswitchTarget | null;
  /** The footswitch's full levelable-parameter candidates (drives the Set up param
   *  picker). Present only on footswitch rows; the chosen one is baked into
   *  `footswitch` when the run starts. */
  levelParams?: LevelParamCandidate[];
  /** Footswitch rows only: the switch carries NO `customLabel` on the device, so
   *  `sceneName` above is a derived fallback naming the DEFAULT candidate's block — not
   *  the player's own name for the switch.
   *
   *  Load-bearing at run-start, not display: `sceneName` becomes `displayLabel`, which the
   *  backend writes as the switch's on-device `customLabel`. If the user picks a different
   *  candidate in Set up, the row must be RE-NAMED after the block actually being leveled
   *  (`SetupBody.start`), or the unit ends up labelled after a block the run never touched.
   *  A LABELED switch is never renamed: that string is the player's, not ours. */
  fsUnlabeled?: boolean;
  /** Scene rows only: this row's target-mode pick ("match" every scene solves to the
   *  named target — the default when absent; "offset" preserves the scene's authored
   *  loudness RELATIONSHIP). Undefined for Base/footswitch rows. */
  sceneTargetMode?: "match" | "offset";
  /** Scene rows only: the user's chosen leveling control, INSTEAD of the active amp's
   *  `outputLevel` — undefined/null = the amp default (every existing caller). */
  sceneHandle?: SceneHandlePick | null;
}

/** The e2e-hook identity for a setup row (`PresetOptionRow`'s `data-setup-row`) —
 *  DELIBERATELY DISTINCT from `SetupOption.key` (the SELECTION key: `sel`/`rows` Map
 *  lookups and the React list key, unchanged by this function). A footswitch's `key`
 *  is `fswKey`'s POSITION within the levelable-filtered footswitch list, so a fixture
 *  edit that adds/removes an earlier switch's level candidate silently shifts every
 *  LATER switch's position — and hence a spec's `f<slot>:<i>` selector, with no
 *  signal that it now points at a different row. The hook instead names the row by
 *  the DEVICE SWITCH NUMBER (`FootswitchTarget.switchIndex`, sourced from
 *  `FootswitchInfo.switch` — see `footswitchTarget`), which is stable under any
 *  filtered-list reshuffle. A scene row's hook stays `s<slot>:<sceneSlot>`:
 *  `sceneSlot` is already the wire `scenes[]` index (`chosenFrom`'s "the row index IS
 *  the 0-based wire sceneSlot"), i.e. already an IDENTITY, not a filtered-list
 *  position, so it needs no translation. Base rows keep `p<slot>` (nothing to
 *  disambiguate). */
export function setupRowHookKey(o: SetupOption): string {
  if (o.footswitch != null) {
    return `f${String(o.slot)}:sw${String(o.footswitch.switchIndex)}`;
  }
  if (o.sceneSlot != null) return sceneKeyOf(o.slot, o.sceneSlot);
  return baseKey(o.slot);
}

/** A chosen setup row + its resolved instrument id and target name (the setup
 *  dialog emits one per option on "Level"; the flow turns each into a RunItem). */
export interface SetupChoice {
  option: SetupOption;
  instId: string;
  targetName: string;
}

/** Resolve the scene keys SELECTED in the list into the setup rows to configure.
 *  Walks every non-empty preset (sorted, Base-first) and emits a SetupOption for
 *  each of its keys present in `sel`. Everything returned WILL be leveled — the
 *  setup dialog only sets each sound's instrument + target, never re-gates it. */
export function chosenFrom(
  sel: Set<string>,
  rows: PresetRow[],
  sceneInfo: Map<number, SceneInfo[]>,
  footswitchInfo: Map<number, FootswitchInfo[]>,
): SetupOption[] {
  const items: SetupOption[] = [];
  [...rows]
    .filter((r) => !r.empty)
    .sort((a, b) => a.slot - b.slot)
    .forEach((r) => {
      const scenes = sceneInfo.get(r.slot) ?? [];
      const footswitches = footswitchInfo.get(r.slot) ?? [];
      // A footswitch row reads like a scene (the user picks "a sound"), so a preset with
      // ONLY footswitches still shows "Base Preset" vs "Whole preset" as a true scene-less case.
      const hasChildren = scenes.length > 0 || footswitches.length > 0;
      if (sel.has(baseKey(r.slot))) {
        items.push({
          key: baseKey(r.slot),
          slot: r.slot,
          presetName: r.name,
          isBase: true,
          sceneSlot: null,
          sceneName: hasChildren ? "Base Preset" : "Whole preset",
          tag: hasChildren ? "BASE" : null,
          hasScenes: hasChildren,
        });
      }
      scenes.forEach((sc, i) => {
        if (sel.has(sceneKeyOf(r.slot, i))) {
          items.push({
            key: sceneKeyOf(r.slot, i),
            slot: r.slot,
            presetName: r.name,
            isBase: false,
            sceneSlot: i, // the row index IS the 0-based wire sceneSlot
            sceneName: sc.name,
            tag: sc.fs != null ? `FS${String(sc.fs)}` : "—",
            hasScenes: true,
          });
        }
      });
      footswitches.forEach((f, i) => {
        const target = footswitchTarget(f);
        if (target && sel.has(fswKey(r.slot, i))) {
          items.push({
            key: fswKey(r.slot, i),
            slot: r.slot,
            presetName: r.name,
            isBase: false,
            sceneSlot: null,
            sceneName: footswitchName(f),
            tag: fsTagOf(f.switch),
            hasScenes: true,
            footswitch: target,
            levelParams: f.level_params,
            fsUnlabeled: f.label.trim() === "",
          });
        }
      });
    });
  return items;
}

// ── run / summary: one item per chosen scene ────────────────────────────────

// `offbranch` is its OWN outcome (not a flavor of `clamped`): the amp doesn't reach the
// USB 1/2 capture, so re-leveling can't fix it — only a routing change on the unit can.
//
// `unconverged` is likewise its own outcome, and the distinction from `clamped` is the
// user's next action: a CLAMPED sound is at the end of its knob and cannot reach target
// however often it runs, while an UNCONVERGED one still had knob room and simply ran out
// of measurement captures — running it again improves it. Backed by
// `FootswitchLevelResult.unconverged` (footswitch rows only today). Folding it into
// `clamped` would also feed a non-ceiling into `ceilingOf` → the derived common target.
//
// `verified` is a footswitch VERIFY row (no handle chosen): nothing was solved or
// written, only measured (`FootswitchLevelResult.on_off_delta_lu` is the discriminator
// — see `useLevelingFlow`'s `outcomeOf`). It must NEVER read as "done" (which would
// claim a write that never happened).
export type Outcome =
  "done" | "clamped" | "unconverged" | "offbranch" | "skipped" | "verified";

/** Dynamics-spread flag threshold (LU): short-term-max − integrated above this
 *  marks a DYNAMIC sound — the gated reading understates its peaks vs a
 *  compressed one, so the leveled result deserves an ear-check. */
export const DYNAMIC_SPREAD_LU = 6;

export interface RunItem {
  key: string;
  /** 0-based list index of the preset. */
  slot: number;
  presetName: string;
  isBase: boolean;
  /** 0-based scenes[] wire slot, or null for the Base/whole-preset step. */
  sceneSlot: number | null;
  sceneName: string;
  tag: string | null;
  /** Set ⇒ a block-acting FOOTSWITCH step (dispatched to `levelFootswitchesApply`);
   *  null/undefined ⇒ Base (`level_preset`) or FS scene (`level_scenes_apply_batched`). */
  footswitch?: FootswitchTarget | null;
  /** Chosen instrument profile id ("" when none). */
  instId: string;
  /** Chosen target name. */
  targetName: string;
  // live + final:
  status: "queued" | "active" | "result";
  /** Backend-supplied reason the row is active with no capture yet (e.g. the freshness
   *  barrier's "waiting for the device to commit the previous save…" — a same-slot load can
   *  land inside the TMP's lazy `saveCurrentPreset` commit window). Scene/footswitch channel
   *  items only; null/undefined falls back to the row's generic "connecting…". */
  activeMessage?: string | null;
  outcome?: Outcome;
  /** Measured loudness (verify/predicted), or null. */
  value?: number | null;
  /** VERIFY footswitch rows ONLY (`outcome === "verified"`): engaged − disengaged
   *  loudness (LU). Positive = engaging makes the preset louder. Undefined/null for
   *  every other row — distinct from `value`, which stays the plain measured LUFS. */
  verifyDeltaLu?: number | null;
  /** Scene rows in Offset target mode ONLY: how far the effective target was shifted
   *  from the requested one to preserve the scene's authored loudness relationship
   *  (LU). `null`/undefined in Match mode or on non-scene rows. */
  targetOffsetLu?: number | null;
  /** Scene rows only: this row's target-mode + handle pick, carried from Set up into
   *  the dispatch (mirrors `SetupOption.sceneTargetMode`/`sceneHandle`). */
  targetMode?: "match" | "offset";
  handle?: SceneHandlePick | null;
  /** Dynamics spread of the measure capture (LU); drives the "dynamic" by-ear cause. */
  spreadLu?: number | null;
  /** The preset's saved `presetLevel` before this run wrote it — enables the Summary
   *  "Restore original" (Base rows only; scene/footswitch writes aren't revertable). */
  previousLevel?: number | null;
  /** PREDICTED true peak (dBTP) at the leveled setting — an estimate, never a
   *  re-measurement. Only Base rows carry a value (undefined/null elsewhere); drives
   *  the Summary "may clip" chip when > −1 dBTP. */
  truePeakDbtp?: number | null;
  /** Cause of the "verify by ear" marker (undefined = no flag): `envelope` = the preset
   *  contains an envelope-follower effect, which tracks the synthetic stimulus differently
   *  than real playing (the measurement itself is suspect); `dynamic` = peaks ride
   *  above the gated average; `wet_floor` = a footswitch's wet-mix clamp is pinned at the
   *  25% floor, not headroom (`FootswitchLevelResult.wet_floor`); `rebalance` = shallow
   *  lane-mute isolation made the parallel balance approximate. Resolved to a single
   *  cause when the RunItem is built. */
  verifyByEar?: "envelope" | "dynamic" | "wet_floor" | "rebalance";
  /** The preset's backup-scan silence hint, stamped at item build — refines the
   *  offbranch row status (see `offbranchStatus`). */
  silenceHint?: SilenceHint;
  /** The sound's MEASURED raw ceiling (max-reachable LUFS), set on Base rows from the
   *  result's `constant_c`. Feeds the reachable-common-target derivation (a clamped row's
   *  ceiling is its measured `value` instead — it sits at max). Undefined until measured. */
  ceilingLufs?: number | null;
  /** Set by the reachable-common-target fallback: an explicit numeric target that OVERRIDES
   *  `targetLufsByName(targetName)` in the run loop's dispatch (pre-offset; the runner adds
   *  the playback offset). Normal runs never set it. */
  targetOverrideLufs?: number;
  /** Set by the reachable-common-target fallback for a row it does NOT re-level (off-branch,
   *  no ceiling): the run loop leaves the row's existing outcome untouched so it stays
   *  visible/counted in the Summary without wasting a re-capture on a signal-less sound. */
  skipRelevel?: boolean;
}

/** A finished row's MEASURED raw ceiling for the reachable-common-target derivation, or null
 *  when unknown. A CLAMPED row sits at max, so its measured `value` IS its ceiling; a done
 *  row's ceiling is `ceilingLufs` (Base rows carry `constant_c`; done scene/footswitch rows
 *  have none → excluded, their true ceiling is ≥ their reached target so they don't bind).
 *
 *  EXCEPT a clamped FOOTSWITCH row: preset/scene clamps are top-rail only (`LEVEL_MIN` is
 *  0.0 and `ideal = 10^x > 0`, so `ideal < LEVEL_MIN` is unreachable — a preset/scene can
 *  only clamp because it's TOO QUIET to reach target, never too loud), so their clamped
 *  `value` genuinely IS a ceiling. `measure_footswitch`'s clamp is direction-agnostic (a
 *  switch CAN clamp because it's too LOUD), so treating it the same way would feed a FLOOR
 *  into `min(ceiling)` and drag the whole library's derived common target down. Accepted
 *  loss: a genuinely quiet clamped switch stops binding the common target — it still shows
 *  its own clamped outcome, just doesn't drag every OTHER sound's target down with it. */
export const ceilingOf = (it: RunItem): number | null => {
  const c =
    it.outcome === "clamped" && !it.footswitch ? it.value : it.ceilingLufs;
  return c != null && Number.isFinite(c) ? c : null;
};

/** The offbranch ("silent capture") row status, refined by the preset's JSON-visible
 *  cause when the backup scan found one. Rendered verbatim in RunBody + SummaryBody. */
export function offbranchStatus(hint: SilenceHint | undefined): string {
  if (hint === "amp_zero") return "amp output at zero";
  if (hint === "exp_mute") return "exp pedal may mute";
  return "not on USB 1/2";
}

/** Verify-row result prose (footswitch rows only, `outcome === "verified"`): a compact
 *  ON-vs-OFF delta, e.g. "+2.3 LU vs off" / "−1.1 LU vs off" — honest (states direction
 *  + magnitude) without claiming a write that never happened. Rendered verbatim in
 *  RunBody + SummaryBody. */
export function verifyDeltaText(deltaLu: number | null | undefined): string {
  if (deltaLu == null || !Number.isFinite(deltaLu)) return "verified";
  return `${signedDb(deltaLu)} LU vs off`;
}

/** Scene Offset-mode result suffix, e.g. " · kept +1.4 LU" — empty when there's nothing
 *  to report (Match mode, or a shift too small to matter). Rendered verbatim in RunBody
 *  + SummaryBody. */
export function targetOffsetSuffix(
  offsetLu: number | null | undefined,
): string {
  if (
    offsetLu == null ||
    !Number.isFinite(offsetLu) ||
    Math.abs(offsetLu) < 0.05
  )
    return "";
  return ` · kept ${signedDb(offsetLu)} LU`;
}

/** The sound's preset line — the mono sub-line under its name. Rendered verbatim in
 *  RunBody + SummaryBody, so it lives here rather than being retyped on both. */
export const presetLine = (it: RunItem): string =>
  `${slotLabel(it.slot)} · ${it.presetName}`;

/** The LUFS a row is ACTUALLY aiming at. The reachable-common-target fallback stamps an
 *  explicit override that wins over the named target — the run loop's dispatch and the
 *  run table's Target cell must resolve it the same way, so both call this. */
export const resolvedTargetLufs = (
  it: RunItem,
  targetLufsByName: (name: string | null) => number,
): number => it.targetOverrideLufs ?? targetLufsByName(it.targetName);

/** Turn a checked setup row into a run item with its resolved instrument + target. */
export function optionToRunItem(
  o: SetupOption,
  instId: string,
  targetName: string,
): RunItem {
  return {
    key: o.key,
    slot: o.slot,
    presetName: o.presetName,
    isBase: o.isBase,
    sceneSlot: o.sceneSlot,
    sceneName: o.sceneName,
    tag: o.tag,
    footswitch: o.footswitch ?? null,
    instId,
    targetName,
    status: "queued",
    targetMode: o.sceneTargetMode,
    handle: o.sceneHandle,
  };
}

/** Rebuild a setup row from a (clamped) run item — for "Re-level clamped…", which
 *  reopens setup pre-loaded with just the clamped scenes, all checked, no scan.
 *  ponytail: the RunItem doesn't carry `levelParams`, so a re-leveled footswitch keeps
 *  its already-chosen param but can't be re-picked (the param column renders empty).
 *  Add `levelParams` to RunItem if re-pick-on-relevel is ever wanted. */
export function runItemToOption(it: RunItem): SetupOption {
  return {
    key: it.key,
    slot: it.slot,
    presetName: it.presetName,
    isBase: it.isBase,
    sceneSlot: it.sceneSlot,
    sceneName: it.sceneName,
    tag: it.tag,
    hasScenes: !it.isBase || it.tag != null,
    footswitch: it.footswitch ?? null,
    sceneTargetMode: it.targetMode,
    sceneHandle: it.handle,
  };
}

// The wizard's stage machine + run state now live in the flow hook
// (useLevelingFlow → Stage / RunState); this module just owns the per-scene types.

// ── the preset-level (Base) job builder ─────────────────────────────────────

/** Build a `level_preset` job (Base / whole-preset leveling via `presetLevel`).
 *  FS scenes use `level_scenes_apply_batched` instead (amp `outputLevel`). */
export function buildLevelJob(
  slot: number,
  targetLufs: number,
  profile: Profile | null,
  save: boolean,
): LevelJob {
  return {
    slot,
    target_lufs: targetLufs,
    save,
    topology_id: profile?.topology_id ?? null,
    calibration_lufs: profile?.calibration_lufs ?? null,
    profile_id: profile?.id ?? null,
    block_group_id: null,
    block_node_id: null,
    block_parameter_id: null,
    block_value: null,
  };
}
