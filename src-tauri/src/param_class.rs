//! Parameter-classification table: annotates block parameters for the leveling
//! picker and gates which params may be swept for loudness. Single source of
//! truth is the checked-in `src/models/param-class.json` (repo-root, frontend
//! `src/`) — parsed once here and mirrored verbatim by
//! `src/views/level/paramClass.ts` on the TS side, so the two can't drift.
//!
//! **This table ANNOTATES; it never guesses intent.** A param absent from both
//! `defaults` and `blockOverrides` classifies as [`ParamClass::Other`] — silence
//! is the safe default, not an inferred class.
//!
//! Scope (honest): today the FOOTSWITCH lane (`footswitch::is_levelable_param`,
//! `leveller::FsParamTarget`) consumes this table. The preset block-knob lane
//! (`commands/level_preset.rs`) still infers ranges by value-sniffing
//! (`scene_bench::knob_bounds`) and carries no class gate — two range mechanisms
//! can answer differently for the same param until that lane migrates here.
//!
//! Two proven traps drove the `blockOverrides` shape:
//! - `level` on `ACD_TMRumbleV3` is an amp KNOB, not a level control — sweeping
//!   it for loudness is wrong even though `level` is a `level_linear` default
//!   everywhere else (`notes/leveling.md`: "`level` on `ACD_TMRumbleV3` is an amp
//!   knob but NOT the output leveling control — it must not be changed").
//! - Generic names collide across block families: `gain` means drive on most
//!   amps/pedals, but on `ACD_Boost` it is measured RAW dB (fw 1.8.45,
//!   HW-verified): the block's base value 2.5 is +2.5 dB, writes of
//!   0/2.5/5/7 were all accepted by `changeParameter`, and the captured loudness
//!   tracked the write ~1:1 dB→LUFS. That is why `ACD_Boost.gain` gets its own
//!   `blockOverrides` entry (`level_db`) instead of falling through to a
//!   `defaults` guess.
//!
//! A third trap is generic across the whole amp family rather than one block:
//! `volume` is a genuine output level on pedals (e.g. `ACD_KingOfTone`), but on
//! an amp model it is the preamp/breakup knob — sweeping it changes tone, not
//! just loudness (the same knob is spelled `gain` on other amps, already
//! refused by omission from `defaults`). `reverb` is the same shape: a reverb
//! depth knob on a reverb-carrying amp, but a genuine wet/mix send everywhere
//! else. `ampOverrides` bars both on any block [`is_amp_model_id`] accepts,
//! without needing a per-amp `blockOverrides` entry for every model.
//!
//! `blockOverrides` and amp-ness both key on the BASE FenderId after collapsing
//! device suffixes (cab/IR/convolution) via
//! [`crate::probe_api::scene_jobs::resolve_base_id`] — the same
//! check-first-then-strip helper `is_amp_model_id` uses, shared rather than
//! duplicated so the one suffix-collapse rule can't drift between amp
//! classification and parameter classification.
//!
//! **Ranges on `level_db` entries are conservative and UNVERIFIED** except
//! where noted above: `ACD_Boost.gain`'s `[0, 12]` extrapolates past the
//! HW-observed 0..7 sweep to leave sweep headroom, and `makeupgaindb`'s
//! `[0, 24]` is an unverified placeholder pending its own HW pass. Treat both
//! as safe-but-not-confirmed bounds, not measured ceilings.

use std::collections::HashMap;
use std::sync::OnceLock;

use serde::Deserialize;

use crate::is_amp_model_id;
use crate::probe_api::scene_jobs::resolve_base_id;

/// How a block parameter may be used by the leveling picker.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParamClass {
    /// Linear amplitude control (`captured_LUFS = 20*log10(v) + C`).
    LevelLinear,
    /// Raw decibel control with ~1:1 dB-to-loudness authority.
    LevelDb,
    /// Dry/wet blend — not a loudness control on its own.
    WetMix,
    /// Everything else, including params explicitly barred from leveling
    /// (e.g. `ACD_TMRumbleV3.level`). `range` is meaningless for this variant.
    Other,
}

/// A parameter's classification plus its usable range. `range` is meaningless
/// when `class` is [`ParamClass::Other`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ParamInfo {
    pub class: ParamClass,
    pub range: (f32, f32),
}

#[derive(Debug, Deserialize)]
struct RawEntry {
    class: String,
    #[serde(default)]
    range: Option<[f32; 2]>,
}

#[derive(Debug, Deserialize)]
struct RawTable {
    defaults: HashMap<String, RawEntry>,
    #[serde(rename = "ampOverrides")]
    amp_overrides: HashMap<String, RawEntry>,
    #[serde(rename = "blockOverrides")]
    block_overrides: HashMap<String, HashMap<String, RawEntry>>,
}

fn table() -> &'static RawTable {
    static TABLE: OnceLock<RawTable> = OnceLock::new();
    TABLE.get_or_init(|| {
        let t: RawTable = serde_json::from_str(include_str!("../../src/models/param-class.json"))
            .expect(
                "param-class.json is a checked-in fixture; include_str! embeds it at compile \
                 time but this parse is lazy — a malformed table panics at RUNTIME on first \
                 classify() call, and it's the Rust and TS test suites that catch it",
            );
        // A dB param has NO universal range (`ACD_Boost.gain` is [0,12], `makeupgaindb`
        // [0,24]) — a missing `range` would otherwise fall back to a degenerate (0,0),
        // placing every seed and bracket point at 0.0: a silently dead solve instead of a
        // loud table error. `[0,1]` IS the universal range for the normalized classes, so
        // only `level_db` requires the field.
        for (key, raw) in t
            .defaults
            .iter()
            .chain(t.amp_overrides.iter())
            .chain(t.block_overrides.values().flatten())
        {
            assert!(
                raw.class != "level_db" || raw.range.is_some(),
                "param-class.json: `{key}` is level_db without a range — dB params have no \
                 default range"
            );
        }
        t
    })
}

fn parse_entry(raw: &RawEntry) -> ParamInfo {
    match raw.class.as_str() {
        "level_linear" => ParamInfo {
            class: ParamClass::LevelLinear,
            range: raw.range.map_or((0.0, 1.0), |[lo, hi]| (lo, hi)),
        },
        "level_db" => ParamInfo {
            class: ParamClass::LevelDb,
            // `range` presence is asserted at table load (`table()`); no degenerate default.
            range: raw
                .range
                .map(|[lo, hi]| (lo, hi))
                .expect("validated at load"),
        },
        "wet_mix" => ParamInfo {
            class: ParamClass::WetMix,
            range: raw.range.map_or((0.0, 1.0), |[lo, hi]| (lo, hi)),
        },
        _ => ParamInfo {
            class: ParamClass::Other,
            range: (0.0, 0.0),
        },
    }
}

/// Classify `param` on block `fender_id`. Precedence: `blockOverrides` (exact
/// block, matched on the BASE FenderId, suffixes collapsed) beats
/// `ampOverrides` (block is an amp model per [`is_amp_model_id`]) beats the
/// param-name `defaults`; a param present in none of the three is
/// [`ParamClass::Other`].
pub fn classify(fender_id: &str, param: &str) -> ParamInfo {
    let t = table();
    let override_hit = resolve_base_id(fender_id, |m| t.block_overrides.contains_key(m))
        .and_then(|base| t.block_overrides.get(&base))
        .and_then(|params| params.get(param));
    if let Some(raw) = override_hit {
        return parse_entry(raw);
    }
    if is_amp_model_id(fender_id) {
        if let Some(raw) = t.amp_overrides.get(param) {
            return parse_entry(raw);
        }
    }
    t.defaults.get(param).map_or(
        ParamInfo {
            class: ParamClass::Other,
            range: (0.0, 0.0),
        },
        parse_entry,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_level_linear() {
        let info = classify("ACD_SomeBlock", "outputLevel");
        assert_eq!(info.class, ParamClass::LevelLinear);
        assert_eq!(info.range, (0.0, 1.0));
    }

    #[test]
    fn default_level_db() {
        let info = classify("ACD_SomeBlock", "makeupgaindb");
        assert_eq!(info.class, ParamClass::LevelDb);
        assert_eq!(info.range, (0.0, 24.0));
    }

    #[test]
    fn default_wet_mix() {
        let info = classify("ACD_SomeBlock", "mix");
        assert_eq!(info.class, ParamClass::WetMix);
        assert_eq!(info.range, (0.0, 1.0));
    }

    #[test]
    fn unknown_param_is_other() {
        let info = classify("ACD_SomeBlock", "totallyUnknownParam");
        assert_eq!(info.class, ParamClass::Other);
    }

    #[test]
    fn override_wins_over_default_tmrumble_level_trap() {
        // `level` is a level_linear default everywhere EXCEPT ACD_TMRumbleV3,
        // where it's an amp knob that must never be swept for loudness.
        let default = classify("ACD_SomeOtherBlock", "level");
        assert_eq!(default.class, ParamClass::LevelLinear);

        let trapped = classify("ACD_TMRumbleV3", "level");
        assert_eq!(trapped.class, ParamClass::Other);
    }

    #[test]
    fn override_only_shadows_its_own_param() {
        // ACD_Boost overrides `gain`, but its other params still fall through
        // to the param-name defaults.
        let level = classify("ACD_Boost", "level");
        assert_eq!(level.class, ParamClass::LevelLinear);
    }

    #[test]
    fn block_override_gain_is_raw_db() {
        let info = classify("ACD_Boost", "gain");
        assert_eq!(info.class, ParamClass::LevelDb);
        assert_eq!(info.range, (0.0, 12.0));
    }

    #[test]
    fn override_matches_after_suffix_collapse() {
        // A hypothetical device id carrying a merged cab/IR suffix must still
        // collapse to the base "ACD_TMRumbleV3" override.
        let info = classify("ACD_TMRumbleV3CabIR", "level");
        assert_eq!(info.class, ParamClass::Other);
    }

    #[test]
    fn amp_volume_is_the_breakup_knob_not_a_level_control() {
        // ACD_MarshallPlexi (Half Stacks / Amp Heads): `volume` is the
        // preamp/breakup knob, not an output level — sweeping it changes tone.
        let info = classify("ACD_MarshallPlexi", "volume");
        assert_eq!(info.class, ParamClass::Other);
    }

    #[test]
    fn pedal_volume_is_a_genuine_level_control() {
        // ACD_KingOfTone (Effects): `volume` is a real output level, unlike the
        // same param name on an amp model.
        let info = classify("ACD_KingOfTone", "volume");
        assert_eq!(info.class, ParamClass::LevelLinear);
    }

    #[test]
    fn block_override_beats_amp_override() {
        // ACD_TMRumbleV3 is a Bass Amp AND carries its own `level` override —
        // the exact-block override must still win, unaffected by ampOverrides
        // (which doesn't even define "level").
        let info = classify("ACD_TMRumbleV3", "level");
        assert_eq!(info.class, ParamClass::Other);
    }

    #[test]
    fn amp_override_matches_after_suffix_collapse() {
        // A suffixed amp device id (CabIR-style) must still reach the amp
        // check and bar `volume`.
        let info = classify("ACD_MarshallPlexiCabIR", "volume");
        assert_eq!(info.class, ParamClass::Other);
    }

    // Drift gate, mirrored VERBATIM in `paramClass.test.ts`: one canonical id per amp
    // category `scene_jobs::is_amp_category` names (the TS side re-types that category
    // list, and neither language can read the other's — so both suites pin the same ids
    // and a category drift fails one of them). Catalog caveat, recorded honestly: every
    // "Combo Amps"/"Half Stacks" id in `tmp-model-guide.json` also carries "Amp Heads",
    // so only a dropped "Amp Heads" or "Bass Amps" category is actually caught today.
    #[test]
    fn amp_volume_bar_covers_every_amp_category_canonical_ids() {
        for amp in [
            "ACD_Princeton6G2",  // Combo Amps (+ Amp Heads)
            "ACD_StudioPreamp",  // Amp Heads only
            "ACD_TMRumbleV3",    // Bass Amps only
            "ACD_MarshallPlexi", // Half Stacks (+ Amp Heads)
        ] {
            assert_eq!(
                classify(amp, "volume").class,
                ParamClass::Other,
                "{amp}: amp `volume` is the breakup knob, never a level control"
            );
        }
        // The paired non-amp: pedal `volume` IS a genuine output level.
        assert_eq!(
            classify("ACD_KingOfTone", "volume").class,
            ParamClass::LevelLinear
        );
    }
}
