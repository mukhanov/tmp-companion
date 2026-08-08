// src/views/level/paramClass.ts — parameter-classification mirror of the Rust
// `param_class` module. Single source of truth is the checked-in
// src/models/param-class.json; this file must stay semantically identical to
// src-tauri/src/param_class.rs (mirrored tests on both sides) so JSON drift
// breaks BOTH suites instead of silently diverging.
//
// This table ANNOTATES block parameters for the leveling picker; it never
// guesses intent. A param absent from both `defaults` and `blockOverrides`
// classifies as "other".
//
// Two proven traps drove the blockOverrides shape:
// - `level` on ACD_TMRumbleV3 is an amp KNOB, not a level control (see
//   notes/leveling.md) — it must never be swept for loudness even though
//   `level` is a level_linear default everywhere else.
// - Generic names collide across block families: `gain` means drive on most
//   amps/pedals, but ACD_Boost.gain is measured RAW dB (fw 1.8.45,
//   HW-verified): the block's base value 2.5 is +2.5 dB, writes of
//   0/2.5/5/7 were all accepted by `changeParameter`, and the captured
//   loudness tracked the write ~1:1 dB→LUFS.
//
// A third trap is generic across the whole amp family rather than one block:
// `volume` is a genuine output level on pedals (e.g. ACD_KingOfTone), but on
// an amp model it is the preamp/breakup knob — sweeping it changes tone, not
// just loudness (the same knob is spelled `gain` on other amps, already
// refused by omission from `defaults`). `reverb` is the same shape: a reverb
// depth knob on a reverb-carrying amp, but a genuine wet/mix send everywhere
// else. `ampOverrides` bars both on any block the catalog categorizes as an
// amp, without needing a per-amp `blockOverrides` entry for every model.
//
// `blockOverrides` and amp-ness both key on the BASE FenderId after
// collapsing device suffixes (cab/IR/convolution) — reuses the SAME
// check-first-then-strip helper `models/cpu.ts` uses for DSP-cost lookup, so
// this table doesn't grow its own copy of the suffix rule.
//
// Ranges on level_db entries are conservative and UNVERIFIED except where
// noted above.

import paramClassData from "../../models/param-class.json";
import { resolveDeviceId } from "../../models/blockArt";
import { MODELS, type ModelRecord } from "../../models/catalog";

export type ParamClass = "level_linear" | "level_db" | "wet_mix" | "other";

export interface ParamInfo {
  class: ParamClass;
  /** Meaningless when `class` is "other". */
  range: [number, number];
}

interface RawEntry {
  class: string;
  // The JSON's literal array type is plain `number[]` (a 2-tuple isn't
  // representable in a JSON-inferred type) — turned into a real `[number,
  // number]` by `toRange` below, which length-guards rather than trusting it.
  range?: number[];
}

interface RawTable {
  defaults: Partial<Record<string, RawEntry>>;
  ampOverrides: Partial<Record<string, RawEntry>>;
  blockOverrides: Partial<Record<string, Partial<Record<string, RawEntry>>>>;
}

const TABLE = paramClassData as RawTable;

function hasOverride(id: string): boolean {
  return Object.prototype.hasOwnProperty.call(TABLE.blockOverrides, id);
}

// Mirrors the Rust `is_amp_category`: the catalog's top-level categories that
// are amp models (as opposed to pedals/cabs/effects). Built from `MODELS`
// (catalog.ts's flattened, `available`-only row list) rather than re-parsing
// tmp-model-guide.json, so this table can't drift from what the Models tab
// itself renders as an amp.
const AMP_CATEGORIES = new Set([
  "Combo Amps",
  "Amp Heads",
  "Bass Amps",
  "Half Stacks",
]);

function isAmpRecord(r: ModelRecord): r is ModelRecord & { bid: string } {
  return r.bid !== null && AMP_CATEGORIES.has(r.cat);
}

const AMP_MODEL_IDS = new Set(MODELS.filter(isAmpRecord).map((r) => r.bid));

function isAmpId(id: string): boolean {
  return AMP_MODEL_IDS.has(id);
}

/** Whether `fenderId` (device suffixes collapsed) resolves to a catalogued
 *  amp model. Mirrors the Rust `is_amp_model_id`. */
function isAmpModelId(fenderId: string): boolean {
  return isAmpId(resolveDeviceId(fenderId, isAmpId));
}

function toRange(
  range: number[] | undefined,
  fallback: [number, number],
): [number, number] {
  if (range?.length !== 2) return fallback;
  const [lo, hi] = range;
  return [lo, hi];
}

function parseEntry(raw: RawEntry): ParamInfo {
  switch (raw.class) {
    case "level_linear":
      return { class: "level_linear", range: toRange(raw.range, [0, 1]) };
    case "level_db":
      return { class: "level_db", range: toRange(raw.range, [0, 0]) };
    case "wet_mix":
      return { class: "wet_mix", range: toRange(raw.range, [0, 1]) };
    default:
      return { class: "other", range: [0, 0] };
  }
}

/** Classify `param` on block `fenderId`. Precedence: `blockOverrides` (exact
 *  block, matched on the BASE FenderId, device suffixes collapsed) beats
 *  `ampOverrides` (block is a catalogued amp model) beats the param-name
 *  `defaults`; a param present in none of the three is "other". Mirrors the
 *  Rust `param_class::classify` exactly — see that module's tests. */
export function classify(fenderId: string, param: string): ParamInfo {
  const baseId = resolveDeviceId(fenderId, hasOverride);
  const overrideEntry = TABLE.blockOverrides[baseId]?.[param];
  if (overrideEntry) return parseEntry(overrideEntry);
  if (isAmpModelId(fenderId)) {
    const ampEntry = TABLE.ampOverrides[param];
    if (ampEntry) return parseEntry(ampEntry);
  }
  const defaultEntry = TABLE.defaults[param];
  return defaultEntry
    ? parseEntry(defaultEntry)
    : { class: "other", range: [0, 0] };
}
