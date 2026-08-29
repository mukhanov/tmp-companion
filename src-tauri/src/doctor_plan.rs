//! Doctor "balance plan" — rebalance a diagnosed sound with the TONE CONTROLS
//! THE PRESET ALREADY HAS (amp Bass/Mid/Treble/Presence, a drive pedal's Tone,
//! an EQ-10's bands, a parametric's peaks), instead of — or before — inserting
//! a new block. PURE: no device I/O, no Tauri; `commands/doctor.rs` calls
//! [`generate_plan`] once per diagnosed sound and ships the result as
//! `DoctorSoundResult.plan`.
//!
//! The pipeline:
//!
//! 1. **Discover** every drivable tone control on the sound's ACTIVE chain
//!    ([`discover_controls`]): which block, which `dspUnitParameters` key, its
//!    CURRENT value (from the graph's param allowlist — a control whose value
//!    we don't know is skipped, never written blind), its range, and its
//!    per-band **response** — how many dB each analysis band moves per unit of
//!    knob travel ([`NOMINAL`] shapes integrated over the family's band layout).
//! 2. **Solve** ([`solve`]) for the smallest set of moves that brings the
//!    sound's centered deviations back inside the rule gates (a box per band,
//!    derived from the SAME `Thresholds` the diagnosis fired on, minus a
//!    safety margin), flattens a dark/bright tilt, and tames a fizzy top —
//!    projected gradient descent on a dead-zone loss with a small-move
//!    regularizer, knob bounds and a per-control move cap.
//! 3. **Predict** honestly: the solved moves are pushed through the response
//!    model onto the measured band powers and the predicted profile is
//!    RE-DIAGNOSED with the real rules (`doctor::diagnose_levels`), so the card
//!    reports which findings the plan is predicted to clear and which remain —
//!    never "fixed" by assertion.
//!
//! # The response model is NOMINAL (provisional)
//!
//! The amp tone-stack and pedal-tone shapes in [`NOMINAL`] are textbook
//! passive-stack responses (Fender TMB / Marshall lineage), NOT per-model
//! hardware measurements: a '65 Twin's Bass and a JCM800's Bass do not move the
//! same dB, and a control ahead of a clipping stage moves the OUTPUT spectrum
//! less than its own curve says (pedal controls carry a damping factor for
//! that). Graphic/parametric EQ bands are the exception — their response is
//! the filter's own shape in dB, so those moves are close to exact. The plan is
//! therefore framed as an ESTIMATE in the UI, every apply goes through the
//! same A/B audition as every other prescription (and `doctor_apply` now
//! reports the MEASURED per-band change alongside the clips), and
//! `probe --doctor-knob-sweep <slot>` is the hardware arm that measures a real
//! preset's per-knob band responses so this table can be re-derived per amp
//! family. See `notes/doctor-calibration.md`.

use std::collections::HashMap;

use serde::Serialize;

use crate::doctor::{
    self, DoctorNode, DoctorOp, Family, LeveledDiag, Rx, RxKind, SoundProfile, StimulusKind,
};

// ─── controls ────────────────────────────────────────────────────────────────

/// How a control's value is shown and quantized.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ControlUnit {
    /// A 0..1 normalized knob, displayed on the 0–10 dial Pro Control shows.
    Knob,
    /// A dB gain (graphic/parametric EQ band).
    Db,
    /// A cut-filter corner in Hz (the cab's low/high cut). The SOLVER moves it
    /// in OCTAVES (`x` = log2 of the ratio to the current corner), because a
    /// filter's band effect is linear in octaves, not in Hz; `Control::at`
    /// converts back to the Hz value the device is written.
    Hz,
}

/// Where a control does most of its work — the semantic band its response is
/// largest in — so a remedy rule ("bright: cut the highs") can pick it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Region {
    Lows,
    LowMids,
    Mids,
    HighMids,
    Highs,
    Air,
}

/// What the remedy layer allows a control to do this round — see
/// [`assign_remedies`] and the module doc's "remedy knowledge" section.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum RemedyState {
    /// Any direction (no fired finding constrains it — the fallback when NO
    /// control on the chain matches a remedy).
    Free,
    /// Only moves whose sign is `dir` (+1 = raise the control, −1 = lower it),
    /// ranked 1 (the textbook remedy) .. 3 (a last resort); `why` is the
    /// player-facing rationale carried onto the plan's move row.
    Only {
        dir: f64,
        rank: u8,
        why: &'static str,
    },
    /// No remedy for the fired findings moves this way — held still.
    Frozen,
}

/// A nominal tone-control shape: dB response as a function of frequency, per
/// unit of the control's own unit (one full 0→1 knob travel, or one dB of EQ
/// gain). Sums of simple shelves/peaks — see the module doc for why nominal.
#[derive(Debug, Clone, Copy)]
enum Shape {
    /// Low shelf: `g / (1 + (f/fc)^k)` — `g` dB below `fc`, rolling to 0 above.
    LowShelf { fc: f64, k: f64, g: f64 },
    /// High shelf: `g / (1 + (fc/f)^k)` — `g` dB above `fc`, rolling to 0 below.
    HighShelf { fc: f64, k: f64, g: f64 },
    /// Peak: a Lorentzian in octaves, `g` at `fc`, half at ±`bw_oct/2`.
    Peak { fc: f64, bw_oct: f64, g: f64 },
}

impl Shape {
    fn db_at(self, f: f64) -> f64 {
        match self {
            Shape::LowShelf { fc, k, g } => g / (1.0 + (f / fc).powf(k)),
            Shape::HighShelf { fc, k, g } => g / (1.0 + (fc / f).powf(k)),
            Shape::Peak { fc, bw_oct, g } => {
                let x = 2.0 * (f / fc).log2() / bw_oct;
                g / (1.0 + x * x)
            }
        }
    }
}

/// The nominal control families. Each row: the `dspUnitParameters` keys that
/// name it on the device, its shapes, and the player-facing knob label.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ToneKind {
    AmpBass,
    AmpMid,
    AmpTreble,
    AmpPresence,
    /// A single "Tone" on an amp (tweed / plexi-style treble bleed).
    AmpTone,
    /// Vox-style "Cut" — MORE cut = LESS treble (the shape is negative).
    AmpCut,
    PedalTone,
    PedalTreble,
    PedalBass,
    PedalMid,
    PedalPresence,
}

/// Output-spectrum damping for a control that sits AHEAD of the amp's clipping
/// stage: a drive pedal's Tone re-shapes what gets clipped, and clipping
/// flattens spectral differences, so the OUTPUT moves less than the pedal's
/// own curve. Nominal.
const PEDAL_EFFICACY: f64 = 0.6;

/// The nominal shapes (dB per FULL 0→1 knob travel). Fender TMB lineage for
/// the amp stack: Bass ≈ 18 dB low shelf under ~300 Hz, Mid ≈ a broad +12 dB
/// hump at ~600 Hz riding on a +3 dB level lift (the TMB mid pot is nearly a
/// level control with a mid emphasis — the solver's mean-centering absorbs the
/// lift), Treble ≈ 18 dB high shelf above ~2 kHz, Presence ≈ 10 dB NFB shelf
/// above ~3.5 kHz. Pedal rows are the same families, smaller and damped.
fn nominal_shapes(kind: ToneKind) -> &'static [Shape] {
    match kind {
        ToneKind::AmpBass => &[Shape::LowShelf {
            fc: 300.0,
            k: 1.5,
            g: 18.0,
        }],
        ToneKind::AmpMid => &[
            Shape::Peak {
                fc: 600.0,
                bw_oct: 3.0,
                g: 12.0,
            },
            // The level lift: a very wide "peak" is the simplest flat term.
            Shape::Peak {
                fc: 1000.0,
                bw_oct: 1.0e6,
                g: 3.0,
            },
        ],
        ToneKind::AmpTreble => &[Shape::HighShelf {
            fc: 2000.0,
            k: 1.5,
            g: 18.0,
        }],
        ToneKind::AmpPresence => &[Shape::HighShelf {
            fc: 3500.0,
            k: 2.0,
            g: 10.0,
        }],
        ToneKind::AmpTone => &[
            Shape::HighShelf {
                fc: 1200.0,
                k: 1.5,
                g: 14.0,
            },
            Shape::LowShelf {
                fc: 300.0,
                k: 1.5,
                g: -2.0,
            },
        ],
        ToneKind::AmpCut => &[Shape::HighShelf {
            fc: 3000.0,
            k: 2.0,
            g: -12.0,
        }],
        ToneKind::PedalTone => &[
            Shape::HighShelf {
                fc: 1500.0,
                k: 1.5,
                g: 12.0,
            },
            Shape::LowShelf {
                fc: 250.0,
                k: 1.5,
                g: -2.0,
            },
        ],
        ToneKind::PedalTreble => &[Shape::HighShelf {
            fc: 2000.0,
            k: 1.5,
            g: 12.0,
        }],
        ToneKind::PedalBass => &[Shape::LowShelf {
            fc: 300.0,
            k: 1.5,
            g: 12.0,
        }],
        ToneKind::PedalMid => &[Shape::Peak {
            fc: 700.0,
            bw_oct: 2.5,
            g: 12.0,
        }],
        ToneKind::PedalPresence => &[Shape::HighShelf {
            fc: 3500.0,
            k: 2.0,
            g: 8.0,
        }],
    }
}

/// The NOMINAL table's identity, for the notes/calibration trail (see the
/// module doc). Bumped when a shape changes.
pub const NOMINAL: &str = "nominal-tonestack-v1";

fn knob_label(kind: ToneKind) -> &'static str {
    match kind {
        ToneKind::AmpBass | ToneKind::PedalBass => "Bass",
        ToneKind::AmpMid | ToneKind::PedalMid => "Mid",
        ToneKind::AmpTreble | ToneKind::PedalTreble => "Treble",
        ToneKind::AmpPresence | ToneKind::PedalPresence => "Presence",
        ToneKind::AmpTone | ToneKind::PedalTone => "Tone",
        ToneKind::AmpCut => "Cut",
    }
}

/// Map an AMP block's param key to its tone-control kind. Keys are the
/// device's exact lowercase `dspUnitParameters` names as they appear in real
/// presets: `bass`, `mid`/`middle`, `treb`/`treble`, `presence`/`pres`,
/// `tone`, `cut` (Vox). Anything else (gain, volumes, outputLevel, cab
/// params) is not a tone control here — `outputLevel` is the Level tab's.
fn amp_kind(key: &str) -> Option<ToneKind> {
    Some(match key {
        "bass" => ToneKind::AmpBass,
        "mid" | "middle" => ToneKind::AmpMid,
        "treb" | "treble" => ToneKind::AmpTreble,
        "presence" | "pres" => ToneKind::AmpPresence,
        "tone" => ToneKind::AmpTone,
        "cut" => ToneKind::AmpCut,
        _ => return None,
    })
}

/// Map a DRIVE pedal's param key to its tone-control kind (same key
/// vocabulary, pedal shapes).
fn pedal_kind(key: &str) -> Option<ToneKind> {
    Some(match key {
        "tone" => ToneKind::PedalTone,
        "treb" | "treble" => ToneKind::PedalTreble,
        "bass" => ToneKind::PedalBass,
        "mid" | "middle" => ToneKind::PedalMid,
        "presence" | "pres" => ToneKind::PedalPresence,
        _ => return None,
    })
}

/// Parse a graphic-EQ band key (`gain250hz`, `gain1khz`, `gain16khz`) into its
/// center frequency in Hz. `None` for anything else (`overallgain`, …).
fn graphic_band_hz(key: &str) -> Option<f64> {
    let body = key.strip_prefix("gain")?.strip_suffix("hz")?;
    if let Some(k) = body.strip_suffix('k') {
        let v: f64 = k.parse().ok()?;
        return (v > 0.0).then_some(v * 1000.0);
    }
    let v: f64 = body.parse().ok()?;
    (v > 0.0).then_some(v)
}

/// Parse a parametric band key (`filter3gaindb`) into its band number.
fn parametric_gain_band(key: &str) -> Option<u32> {
    key.strip_prefix("filter")?
        .strip_suffix("gaindb")?
        .parse()
        .ok()
}

/// Whether a `dspUnitParameters` key is one the balance plan reads — the
/// extension of the graph param allowlist in `session::extract_active_graph`
/// (the reverb-mix / cab-cut / `gain*hz` keys stay listed there). Tone-stack
/// names + the parametric's per-band `filterN{frequency,gaindb,q,type}`.
pub fn is_tone_param_key(key: &str) -> bool {
    if amp_kind(key).is_some() || pedal_kind(key).is_some() {
        return true;
    }
    if let Some(rest) = key.strip_prefix("filter") {
        let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
        if digits.is_empty() {
            return false;
        }
        let tail = &rest[digits.len()..];
        return matches!(tail, "frequency" | "gaindb" | "q" | "type");
    }
    false
}

/// Parametric EQ `filterNtype` value for a PEAK band (the fixture's bands
/// 2–4; band 1 defaults to a high-pass (type 0) and band 5 to a low-pass
/// (type 4) — `notes/doctor.md`'s schema dump). Only peaks are modeled:
/// the shelf types' numbering is unverified, and a mis-modeled shelf would
/// push a move the wrong way.
const PARAMETRIC_PEAK_TYPE: f64 = 2.0;
/// Graphic-EQ band bandwidth (octaves): a 10-band is a constant-Q octave EQ
/// (Q ≈ 1.4); the 7-band Mustang EQ bands are wider but share the key
/// vocabulary — close enough for a nominal per-band coefficient.
const GRAPHIC_BW_OCT: f64 = 1.0;
/// EQ band gain range (dB) — mirrors `doctor.rs`'s `EQ10_BAND_RANGE_DB`
/// (±12 is the graphic standard; the parametric's fixture rides to 12.0 too).
const EQ_RANGE_DB: f64 = 12.0;

/// A discovered control: where it lives, what it is, its current value, its
/// bounds, and its per-band response (the family layout, dB per unit).
#[derive(Debug, Clone)]
pub struct Control {
    pub group_id: String,
    pub node_id: String,
    pub model: String,
    pub block_name: String,
    pub param: String,
    pub label: String,
    pub unit: ControlUnit,
    pub current: f64,
    pub lo: f64,
    pub hi: f64,
    /// dB change per unit of `param`, one entry per family band.
    pub response: Vec<f64>,
    /// Multiplier on the per-control move cap ([`Control::max_move`]) — 1.0 for
    /// the plan; a caller wanting a gentler step (the knob sweep) can lower it.
    pub cap: f64,
    /// The remedy layer's verdict for this round ([`assign_remedies`]).
    pub remedy: RemedyState,
}

impl Control {
    /// Per-unit regularizer scale: one dial step on a knob (0.1 of travel)
    /// costs the same as 1.5 dB of EQ or a quarter-octave of a cut corner —
    /// see [`solve`]'s loss.
    fn reg_unit(&self) -> f64 {
        match self.unit {
            ControlUnit::Knob => 0.1,
            ControlUnit::Db => 1.5,
            ControlUnit::Hz => 0.25,
        }
    }
    /// The solver's move `x` that lands the control on device value `to`.
    pub fn travel(&self, to: f64) -> f64 {
        match self.unit {
            ControlUnit::Hz => (to / self.current).log2(),
            _ => to - self.current,
        }
    }
    /// The device value after a solver move `x`, clamped to the range and, for
    /// a corner frequency, rounded to a value a player would dial (5 Hz steps
    /// below 1 kHz, 100 Hz steps above).
    pub fn at(&self, x: f64) -> f64 {
        match self.unit {
            ControlUnit::Hz => {
                let hz = (self.current * 2f64.powf(x)).clamp(self.lo, self.hi);
                let q = if hz >= 1000.0 { 100.0 } else { 5.0 };
                ((hz / q).round() * q).clamp(self.lo, self.hi)
            }
            _ => (self.current + x).clamp(self.lo, self.hi),
        }
    }
    /// The solver's room below/above the current value, in `x` units.
    fn room(&self) -> (f64, f64) {
        let (lo, hi) = (self.travel(self.lo), self.travel(self.hi));
        (lo.max(-self.max_move()), hi.min(self.max_move()))
    }
    /// The remedy's move-cost multiplier: the textbook remedy is cheapest, a
    /// last resort costs 4× — so the solver reaches for the right lever first.
    fn rank_cost(&self) -> f64 {
        match self.remedy {
            RemedyState::Only { rank, .. } => f64::from(1u32 << rank.saturating_sub(1)),
            _ => 1.0,
        }
    }
    /// Player-facing value label ("5.0" dial, "+2.0 dB", "6.4 kHz").
    pub fn value_label(&self, v: f64) -> String {
        match self.unit {
            ControlUnit::Knob => knob_dial(v),
            ControlUnit::Db => db_label(v),
            ControlUnit::Hz => hz_label(v),
        }
    }
    /// The largest move the plan may propose on this control — conservative,
    /// because the amp/pedal responses are nominal (a 2× model error on a
    /// small move is still a small error). Knobs: 3.5 dial steps; EQ: ±6 dB
    /// (the same cap the Match-reference moves use).
    pub fn max_move(&self) -> f64 {
        self.cap
            * match self.unit {
                ControlUnit::Knob => 0.35,
                ControlUnit::Db => 6.0,
                ControlUnit::Hz => 1.0,
            }
    }
    /// Display/quantization step.
    fn step(&self) -> f64 {
        match self.unit {
            ControlUnit::Knob => 0.05,
            ControlUnit::Db => 0.5,
            ControlUnit::Hz => 0.25,
        }
    }
    /// Below this, a move isn't worth a row (half a dial step / 1 dB / a
    /// quarter octave).
    pub fn min_move(&self) -> f64 {
        match self.unit {
            ControlUnit::Knob => 0.05,
            ControlUnit::Db => 1.0,
            ControlUnit::Hz => 0.25,
        }
    }
}

/// Mean dB of `shapes` over each family band, on a log-spaced grid (24 points
/// per band) — the width-integrated view the Doctor's band powers take.
fn band_response(shapes: &[Shape], scale: f64, family: Family) -> Vec<f64> {
    const POINTS: usize = 24;
    family
        .bands()
        .iter()
        .map(|&(lo, hi)| {
            let (lo, hi) = (f64::from(lo), f64::from(hi));
            let ratio = (hi / lo).ln();
            let sum: f64 = (0..POINTS)
                .map(|i| {
                    let f = lo * (ratio * (i as f64 + 0.5) / POINTS as f64).exp();
                    shapes.iter().map(|s| s.db_at(f)).sum::<f64>()
                })
                .sum();
            scale * sum / POINTS as f64
        })
        .collect()
}

/// 2nd-order (12 dB/oct) cut filter's own dB at `f` for corner `fc`: the cab
/// block's `hpf` (`low_cut`) / `lpf`. Nominal — the device's slope is
/// unpublished; a 12 dB/oct Butterworth is the modeler convention.
fn cut_filter_db(f: f64, fc: f64, low_cut: bool) -> f64 {
    let r = if low_cut { fc / f } else { f / fc };
    -10.0 * (1.0 + r.powi(4)).log10()
}

/// The cab cut's per-band response in dB per +1 OCTAVE of corner move,
/// linearized around the CURRENT corner `fc` (the +½- to −½-octave secant).
/// Raising a high cut opens the top (positive Highs/Air); raising a low cut
/// removes lows (negative Lows).
fn cut_response(fc: f64, low_cut: bool, family: Family) -> Vec<f64> {
    const POINTS: usize = 24;
    let (up, down) = (fc * 2f64.sqrt(), fc / 2f64.sqrt());
    family
        .bands()
        .iter()
        .map(|&(lo, hi)| {
            let (lo, hi) = (f64::from(lo), f64::from(hi));
            let ratio = (hi / lo).ln();
            let mean = |corner: f64| -> f64 {
                (0..POINTS)
                    .map(|i| {
                        let f = lo * (ratio * (i as f64 + 0.5) / POINTS as f64).exp();
                        cut_filter_db(f, corner, low_cut)
                    })
                    .sum::<f64>()
                    / POINTS as f64
            };
            mean(up) - mean(down)
        })
        .collect()
}

/// Plan-side bounds for the cab cuts (Hz). The device accepts hpf 20–500 /
/// lpf 1 k–20 k (schema-verified, `doctor::cut_move`); the plan keeps to the
/// range players actually dial — a low cut above ~250 Hz or a high cut below
/// ~3 kHz stops being "a cut" and becomes a re-voicing.
const HPF_RANGE_HZ: (f64, f64) = (40.0, 250.0);
const LPF_RANGE_HZ: (f64, f64) = (3000.0, 12000.0);

/// Peak bandwidth in octaves for a parametric `q` (the standard
/// `2·asinh(1/(2Q))/ln 2`).
fn q_to_bw_oct(q: f64) -> f64 {
    2.0 * (1.0 / (2.0 * q)).asinh() / std::f64::consts::LN_2
}

/// Player-facing block name from the model catalog (`pro_control_name`),
/// suffix-stripping the device id the same way amp classification does
/// (`CabIR`/`ConvRvb`/… → the catalog's bare bid). Falls back to the raw id
/// with its `ACD_` prefix dropped.
pub fn block_display_name(model: &str) -> String {
    static NAMES: std::sync::OnceLock<HashMap<String, String>> = std::sync::OnceLock::new();
    let names = NAMES.get_or_init(|| {
        let Ok(catalog) = serde_json::from_str::<serde_json::Value>(include_str!(
            "../../src/models/tmp-model-guide.json"
        )) else {
            return HashMap::new();
        };
        catalog
            .get("blocks")
            .and_then(|v| v.as_array())
            .map(|rows| {
                rows.iter()
                    .filter_map(|row| {
                        let id = row.get("block_id").and_then(|v| v.as_str())?;
                        let name = row.get("pro_control_name").and_then(|v| v.as_str())?;
                        // "(Amp Only)" is Pro Control's amp-without-cab marker,
                        // noise in a knob-move line.
                        let name = name.trim_end_matches(" (Amp Only)").trim();
                        Some((id.to_string(), name.to_string()))
                    })
                    .collect()
            })
            .unwrap_or_default()
    });
    const SUFFIXES: [&str; 5] = ["ConvRvb", "CabIR", "NoCab", "Cab", "IR"];
    let mut m = model;
    loop {
        if let Some(n) = names.get(m) {
            return n.clone();
        }
        match SUFFIXES.iter().find_map(|s| m.strip_suffix(s)) {
            Some(next) => m = next,
            None => {
                if !m.ends_with("NoFx") {
                    if let Some(n) = names.get(&format!("{m}NoFx")) {
                        return n.clone();
                    }
                }
                return model.strip_prefix("ACD_").unwrap_or(model).to_string();
            }
        }
    }
}

/// Compact frequency label for an EQ band: "250 Hz", "1 kHz", "2.6 kHz".
fn hz_label(hz: f64) -> String {
    if hz >= 1000.0 {
        let k = hz / 1000.0;
        if (k - k.round()).abs() < 0.05 {
            format!("{k:.0} kHz")
        } else {
            format!("{k:.1} kHz")
        }
    } else {
        format!("{hz:.0} Hz")
    }
}

/// Every drivable tone control on the sound's active guitar chain, in chain
/// order. Bypassed blocks and mic-lane (`M*`) groups are skipped; a control
/// whose current value the graph didn't carry is skipped (no blind writes).
pub fn discover_controls(nodes: &[DoctorNode], family: Family) -> Vec<Control> {
    let mut out = Vec::new();
    for n in nodes {
        if n.bypassed || !n.group_id.starts_with('G') {
            continue;
        }
        let model = n.model.as_str();
        let push = |out: &mut Vec<Control>,
                    param: &str,
                    label: String,
                    unit: ControlUnit,
                    current: f64,
                    (lo, hi): (f64, f64),
                    response: Vec<f64>| {
            out.push(Control {
                group_id: n.group_id.clone(),
                node_id: n.node_id.clone(),
                model: n.model.clone(),
                block_name: block_display_name(model),
                param: param.to_string(),
                label,
                unit,
                current,
                lo,
                hi,
                response,
                cap: 1.0,
                remedy: RemedyState::Free,
            });
        };
        let mut keys: Vec<&String> = n.params.keys().collect();
        keys.sort();
        // The cab's cuts — on the standalone CabSim and on a CabIR amp (the same
        // `hpf`/`lpf` keys ride the amp node). The most character-preserving
        // remedy a modeler has for boom, mud and fizz, so they are drivable here.
        let carries_cab = model == "ACD_CabSimTMS" || n.cab_sim_id.is_some();
        if carries_cab {
            for (key, low_cut, (lo, hi), label) in [
                ("hpf", true, HPF_RANGE_HZ, "Low cut"),
                ("lpf", false, LPF_RANGE_HZ, "High cut"),
            ] {
                let Some(&cur) = n.params.get(key) else {
                    continue;
                };
                if !(lo..=hi).contains(&cur) {
                    continue;
                }
                push(
                    &mut out,
                    key,
                    label.to_string(),
                    ControlUnit::Hz,
                    cur,
                    (lo, hi),
                    cut_response(cur, low_cut, family),
                );
            }
        }
        if crate::is_amp_model_id(model) {
            for key in keys {
                if let Some(kind) = amp_kind(key) {
                    let cur = n.params[key];
                    if !(0.0..=1.0).contains(&cur) {
                        continue;
                    }
                    push(
                        &mut out,
                        key,
                        knob_label(kind).to_string(),
                        ControlUnit::Knob,
                        cur,
                        (0.0, 1.0),
                        band_response(nominal_shapes(kind), 1.0, family),
                    );
                }
            }
        } else if doctor::is_drive_model(model) {
            for key in keys {
                if let Some(kind) = pedal_kind(key) {
                    let cur = n.params[key];
                    if !(0.0..=1.0).contains(&cur) {
                        continue;
                    }
                    push(
                        &mut out,
                        key,
                        knob_label(kind).to_string(),
                        ControlUnit::Knob,
                        cur,
                        (0.0, 1.0),
                        band_response(nominal_shapes(kind), PEDAL_EFFICACY, family),
                    );
                }
            }
        } else if doctor::is_graphic_eq_model(model) {
            for key in keys {
                if let Some(hz) = graphic_band_hz(key) {
                    let cur = n.params[key];
                    let shape = Shape::Peak {
                        fc: hz,
                        bw_oct: GRAPHIC_BW_OCT,
                        g: 1.0,
                    };
                    push(
                        &mut out,
                        key,
                        hz_label(hz),
                        ControlUnit::Db,
                        cur,
                        (-EQ_RANGE_DB, EQ_RANGE_DB),
                        band_response(&[shape], 1.0, family),
                    );
                }
            }
        } else if doctor::is_parametric_eq_model(model) {
            for key in keys {
                let Some(band) = parametric_gain_band(key) else {
                    continue;
                };
                let get = |suffix: &str| n.params.get(&format!("filter{band}{suffix}")).copied();
                let (Some(fc), Some(q), Some(ty)) = (get("frequency"), get("q"), get("type"))
                else {
                    continue;
                };
                if ty != PARAMETRIC_PEAK_TYPE || fc <= 0.0 || q <= 0.0 {
                    continue;
                }
                let cur = n.params[key];
                let shape = Shape::Peak {
                    fc,
                    bw_oct: q_to_bw_oct(q),
                    g: 1.0,
                };
                push(
                    &mut out,
                    key,
                    format!("Band {band} ({})", hz_label(fc)),
                    ControlUnit::Db,
                    cur,
                    (-EQ_RANGE_DB, EQ_RANGE_DB),
                    band_response(&[shape], 1.0, family),
                );
            }
        }
    }
    out
}

// ─── remedy knowledge ────────────────────────────────────────────────────────
//
// What a guitar sound's findings are FIXED with, in the order a mixer or a
// modeler player reaches for the levers — the domain rules the solver is
// constrained by. The spectrum regions follow the standard electric-guitar EQ
// map (60–120 boom, 120–400 mud/warmth, 400–1 k body/voice, 1–3 k presence and
// honk/harshness, 3–6 k bite/hiss, 6 k+ air/fizz) and the standing rule "cut
// before you boost": brightness and harshness are tamed from the TOP (treble,
// presence, the cab's high cut, an EQ cut at the offending band), never by
// piling on bass — that evens the balance on paper while adding mud and level
// (the HW round that motivated this: a bright Strat → JCM800 crunch where the
// unconstrained solve proposed Bass +1.5 and a 144 Hz boost, and the player
// heard "still bright"). See `notes/doctor.md` § "Remedy knowledge".

/// One remedy: to address a finding, move a control whose dominant [`Region`]
/// is `region` so its band goes DOWN (`cut`) or up. `rank` 1 is the textbook
/// lever, 3 a last resort; `why` is the rationale the plan row shows.
#[derive(Debug, Clone, Copy)]
struct RemedyRule {
    region: Region,
    cut: bool,
    rank: u8,
    why: &'static str,
}

const fn rule(region: Region, cut: bool, rank: u8, why: &'static str) -> RemedyRule {
    RemedyRule {
        region,
        cut,
        rank,
        why,
    }
}

/// The remedies for one tonal finding key, best lever first.
fn remedies_for(key: &str) -> Vec<RemedyRule> {
    match key {
        "bright" => vec![
            rule(
                Region::Highs,
                true,
                1,
                "tames the 3–6 kHz excess that reads bright",
            ),
            rule(Region::Air, true, 1, "rolls off the fizz above 6 kHz"),
            rule(Region::HighMids, true, 2, "eases the 1–3 kHz edge"),
        ],
        "harsh" => vec![
            rule(
                Region::HighMids,
                true,
                1,
                "cuts the 1–3 kHz edge that reads harsh",
            ),
            rule(Region::Highs, true, 2, "smooths the 3–6 kHz hiss"),
            rule(Region::Air, true, 3, "rolls off the top"),
        ],
        "fizzy" => vec![
            rule(Region::Air, true, 1, "rolls off the fizz above 6 kHz"),
            rule(Region::Highs, true, 2, "smooths the 3–6 kHz hiss"),
        ],
        "dark" => vec![
            rule(Region::Highs, false, 1, "opens the 3–6 kHz bite"),
            rule(Region::Air, false, 2, "adds air above 6 kHz"),
            rule(Region::HighMids, false, 2, "adds 1–3 kHz presence"),
            rule(Region::Lows, true, 2, "trims the low end so the top reads"),
            rule(
                Region::LowMids,
                true,
                3,
                "clears low-mid weight so the top reads",
            ),
        ],
        "muddy" => vec![
            rule(Region::LowMids, true, 1, "clears the 120–400 Hz mud"),
            rule(Region::Lows, true, 2, "tightens the low end"),
            rule(Region::Highs, false, 3, "adds top-end clarity over the mud"),
        ],
        "boomy" => vec![
            rule(Region::Lows, true, 1, "tightens the 60–120 Hz boom"),
            rule(Region::LowMids, true, 2, "clears low-mid weight"),
        ],
        "thin" | "buried" => vec![
            rule(Region::Lows, false, 1, "restores low-end body"),
            rule(Region::LowMids, false, 2, "adds 120–400 Hz warmth"),
        ],
        "lost" => vec![
            rule(Region::Mids, false, 1, "brings back the 400 Hz–1 kHz voice"),
            rule(Region::LowMids, true, 3, "clears mud around the voice"),
            rule(Region::Highs, true, 3, "trims the top so the mids read"),
        ],
        _ => vec![],
    }
}

/// A control's response per semantic region, `(region, dB per +1 unit)`.
fn region_responses(c: &Control, family: Family) -> [(Region, f64); 6] {
    let (lows, low_mids, mids, high_mids, highs, air) = family.semantic_bands();
    let at = |i: usize| c.response.get(i).copied().unwrap_or(0.0);
    [
        (Region::Lows, at(lows)),
        (Region::LowMids, at(low_mids)),
        (Region::Mids, at(mids)),
        (Region::HighMids, at(high_mids)),
        (Region::Highs, at(highs)),
        (Region::Air, at(air)),
    ]
}

/// The region a control's response is largest in, and the SIGN of that
/// response per +1 unit of the control (a high cut's corner raised = +Air; a
/// low cut's corner raised = −Lows; Vox Cut raised = −Highs).
pub fn dominant_region(c: &Control, family: Family) -> (Region, f64) {
    let mut best = (Region::Mids, 0.0f64);
    for (region, v) in region_responses(c, family) {
        if v.abs() > best.1.abs() {
            best = (region, v);
        }
    }
    (best.0, if best.1 < 0.0 { -1.0 } else { 1.0 })
}

/// A control "works" a region when its response there is at least this share
/// of its largest response — real tone knobs are wide (a Fender Mid moves the
/// low-mids two-thirds as much as the mids), so a remedy can reach for them.
const REGION_SHARE: f64 = 0.5;

/// Every region a control works, with the sign of its response there.
fn regions_of(c: &Control, family: Family) -> Vec<(Region, f64)> {
    let rr = region_responses(c, family);
    let peak = rr.iter().map(|(_, v)| v.abs()).fold(0.0, f64::max);
    if peak <= 0.0 {
        return Vec::new();
    }
    rr.iter()
        .filter(|(_, v)| v.abs() >= REGION_SHARE * peak)
        .map(|&(r, v)| (r, if v < 0.0 { -1.0 } else { 1.0 }))
        .collect()
}

/// Stamp every control with the remedy layer's verdict: the fired findings'
/// [`remedies_for`] decide which direction each control may move and how
/// cheaply. A control no remedy names is FROZEN. If that would freeze the whole
/// chain (no matching lever at all — say a bright sound with only a Bass knob),
/// every control is left FREE instead: the old unconstrained search is still
/// better than nothing, and the player auditions the result.
pub fn assign_remedies(controls: &mut [Control], fired: &[&str], family: Family) {
    let rules: Vec<RemedyRule> = fired.iter().flat_map(|k| remedies_for(k)).collect();
    let mut any = false;
    for c in controls.iter_mut() {
        // The best-ranked rule any of the control's worked regions satisfies;
        // the move direction follows that region's response sign.
        let pick = regions_of(c, family)
            .into_iter()
            .filter_map(|(region, sign)| {
                rules
                    .iter()
                    .filter(|r| r.region == region)
                    .min_by_key(|r| r.rank)
                    .map(|r| (r, sign))
            })
            .min_by_key(|(r, _)| r.rank);
        c.remedy = match pick {
            Some((r, sign)) => {
                any = true;
                RemedyState::Only {
                    dir: if r.cut { -sign } else { sign },
                    rank: r.rank,
                    why: r.why,
                }
            }
            None => RemedyState::Frozen,
        };
    }
    if !any {
        for c in controls.iter_mut() {
            c.remedy = RemedyState::Free;
        }
    }
}

// ─── the solve ───────────────────────────────────────────────────────────────

/// One tonal rule's gate, in the rule's OWN space — a transcription of the
/// `doctor::apply_thresholds` gate math for the rules the plan can address.
/// The final word is still the real re-diagnosis in [`generate_plan`]; this
/// only steers the search (and `rule_gates_agree_with_the_diagnosis` pins the
/// transcription to the real rules).
#[derive(Debug, Clone)]
enum RuleGate {
    /// Band rule under the two-space CONSENSUS: fires when `sign·local >
    /// gate_tilt` AND `sign·centered > gate_centered`.
    Band {
        band: usize,
        sign: f64,
        gate_tilt: f64,
        gate_centered: f64,
    },
    /// Dark/bright: fires when `|Theil–Sen slope| > gate`.
    Tilt { gate: f64 },
    /// Fizzy: fires when `Air − Highs` (raw band dB) `> gate`.
    Fizzy { gate: f64 },
}

#[derive(Debug, Clone)]
struct Rule {
    /// The finding key — read by the gate-agreement test and the debug trace.
    #[cfg_attr(not(test), allow(dead_code))]
    key: &'static str,
    gate: RuleGate,
    /// [`FIRED_WEIGHT`] for a rule that fired (clear it); [`UNFIRED_WEIGHT`]
    /// for one that didn't (never introduce it).
    weight: f64,
}

/// What the solver is asked to achieve.
#[derive(Debug, Clone)]
pub struct Goal {
    /// Current per-band deviation from the authored target (anchored), family
    /// layout — the raw space [`doctor::deviations`] produces.
    dev: Vec<f64>,
    /// The sound's CURRENT `Air − Highs` (raw `band_db` space — the fizzy
    /// rule's own metric, which is NOT target-relative).
    air_minus_highs_db: f64,
    rules: Vec<Rule>,
    coverage: Option<Vec<bool>>,
}

/// The balance report's tilt tolerance (dB/oct) — a third of the dark/bright
/// gate; see [`balance_error_db`].
pub const BALANCE_TILT_DB_PER_OCT: f64 = 1.0;

/// Safety margin (dB) inside every gate, so a predicted "clear" isn't a
/// boundary coin-flip.
const MARGIN_DB: f64 = 0.5;
/// Weight of a FIRED rule's excess² (dB²) — clearing a finding is the point,
/// so the excess dominates the collateral/move costs until it reaches 0;
/// past that only those costs act, pulling the move back to the gate edge.
const FIRED_WEIGHT: f64 = 4.0;
/// Weight of an un-fired rule's excess — a plan must not trade one finding
/// for another, so introducing one costs more than clearing one earns.
const UNFIRED_WEIGHT: f64 = 12.0;
/// A tilt excess is in dB/oct; over the ~5.6-octave body it is worth ~3 dB
/// at each end, so it's scaled to band dB before weighting.
const TILT_DB_PER_OCT_SCALE: f64 = 3.0;
/// Collateral weight: per dB² of change in ANY band. Keeps the search from
/// re-voicing the whole tone to chase one band (a 5 dB swing costs 0.125 —
/// a tilt fix legitimately swings every band, so this stays light).
const COLLATERAL_WEIGHT: f64 = 0.005;
/// Move cost per (move / reg_unit)² — breaks ties toward the smaller move and
/// pulls a move back to the gate edge once the excess is gone. Scaled per
/// control by [`Control::rank_cost`] (the remedy's rank).
const MOVE_WEIGHT: f64 = 0.01;
/// "Cut before you boost": per dB² of POSITIVE band change, on top of the
/// symmetric collateral — a boost that evens the balance on paper adds level
/// and (in the lows) mud, so an equal-sized cut wins the tie. 4× the collateral.
/// A boost that IS the textbook remedy (rank 1 — "dark: open the treble") is
/// exempt: the rule is about equal alternatives, not about refusing the fix.
const BOOST_WEIGHT: f64 = 0.02;
const MAX_PASSES: usize = 30;

/// Derive the solver's goal from the family thresholds and the findings that
/// actually fired. A FIRED rule's gate is taken at the STAGE offsets (the
/// tightest of the three playback levels — firing sets are louder-suffixes,
/// so a finding that fires anywhere fires at Stage, and "cleared" means
/// cleared at every volume); an un-fired rule keeps its Rehearsal gate (the
/// anchor) and the [`UNFIRED_WEIGHT`], so the plan doesn't turn a clean band
/// into a new finding. Rules whose band the capture never excited
/// (`coverage`) are left out, exactly as the diagnosis skips them.
fn goal_for(
    dev: Vec<f64>,
    bdb: &[f64],
    family: Family,
    nodes: &[DoctorNode],
    diags: &[LeveledDiag],
    coverage: Option<&[bool]>,
) -> Goal {
    let t = family.thresholds();
    let (lows, low_mids, mids, high_mids, highs, air) = family.semantic_bands();
    let fired = |key: &str| diags.iter().any(|d| d.diag.key == key);
    let stage = doctor::playback_offsets(crate::profiles::PlaybackLevel::Stage);
    let covered = |i: usize| coverage.is_none_or(|c| c.get(i).copied().unwrap_or(false));
    let has_drive = nodes
        .iter()
        .any(|n| !n.bypassed && doctor::is_drive_model(&n.model));
    let mut rules = Vec::new();
    let mut push = |key: &'static str, gate: RuleGate| {
        rules.push(Rule {
            key,
            gate,
            weight: if fired(key) {
                FIRED_WEIGHT
            } else {
                UNFIRED_WEIGHT
            },
        });
    };
    let low_off = |key: &str| if fired(key) { stage.low_end_db } else { 0.0 };
    if covered(low_mids) {
        push(
            "muddy",
            RuleGate::Band {
                band: low_mids,
                sign: 1.0,
                gate_tilt: t.muddy_db + low_off("muddy"),
                gate_centered: t.muddy_centered_db + low_off("muddy"),
            },
        );
    }
    if covered(lows) {
        push(
            "boomy",
            RuleGate::Band {
                band: lows,
                sign: 1.0,
                gate_tilt: t.boomy_db + low_off("boomy"),
                gate_centered: t.boomy_centered_db + low_off("boomy"),
            },
        );
        if family == Family::Guitar {
            push(
                "thin",
                RuleGate::Band {
                    band: lows,
                    sign: -1.0,
                    gate_tilt: t.thin_db,
                    gate_centered: t.thin_centered_db,
                },
            );
        }
        if matches!(family, Family::Bass | Family::BassVi) && has_drive {
            push(
                "buried",
                RuleGate::Band {
                    band: lows,
                    sign: -1.0,
                    gate_tilt: t.buried_lows_db,
                    gate_centered: t.buried_centered_db,
                },
            );
        }
    }
    if covered(high_mids) {
        push(
            "harsh",
            RuleGate::Band {
                band: high_mids,
                sign: 1.0,
                gate_tilt: t.harsh_db,
                gate_centered: t.harsh_centered_db,
            },
        );
    }
    if covered(mids) {
        push(
            "lost",
            RuleGate::Band {
                band: mids,
                sign: -1.0,
                gate_tilt: t.lost_db,
                gate_centered: t.lost_centered_db,
            },
        );
    }
    // dark and bright share one |slope| gate; the weight follows either.
    let tilt_key = if fired("bright") || fired("dark") {
        if fired("bright") {
            "bright"
        } else {
            "dark"
        }
    } else {
        "dark"
    };
    push(
        tilt_key,
        RuleGate::Tilt {
            gate: t.tilt_db_per_oct,
        },
    );
    if covered(air) && covered(highs) {
        let off = if fired("fizzy") { stage.fizzy_db } else { 0.0 };
        push(
            "fizzy",
            RuleGate::Fizzy {
                gate: t.fizzy_db + off,
            },
        );
    }
    Goal {
        dev,
        air_minus_highs_db: bdb[air] - bdb[highs],
        rules,
        coverage: coverage.map(<[bool]>::to_vec),
    }
}

/// Dead-zone residual: distance outside `[lo, hi]`, 0 inside.
fn outside(v: f64, lo: f64, hi: f64) -> f64 {
    if v > hi {
        v - hi
    } else if v < lo {
        v - lo
    } else {
        0.0
    }
}

/// The balance error of a deviation vector (dB): the largest centered body-band
/// deviation beyond `tol`, and the Theil–Sen slope beyond the tilt tolerance
/// scaled to band dB — 0 when the balance sits inside the tolerance. A
/// REPORT for the card ("distance to the reference balance"), not a solver term.
pub fn balance_error_db(dev: &[f64], family: Family, tol: f64, coverage: Option<&[bool]>) -> f64 {
    let (lows, _, _, _, highs, _) = family.semantic_bands();
    let centered = doctor::centered_deviations(dev, family);
    let band = (lows..=highs)
        .map(|i| outside(centered[i], -tol, tol).abs())
        .fold(0.0, f64::max);
    let (slope, _) = doctor::tilt_split(dev, family, coverage);
    let tilt = slope.map_or(0.0, |s| {
        outside(s, -BALANCE_TILT_DB_PER_OCT, BALANCE_TILT_DB_PER_OCT).abs() * TILT_DB_PER_OCT_SCALE
    });
    band.max(tilt)
}

/// Per-rule excess (dB past the gate, + the safety margin; 0 when safely
/// clear) for a candidate deviation vector — the rule's own consensus /
/// slope / Air−Highs math. `delta` is the per-band change the moves made
/// (for the fizzy raw-space metric).
fn rule_excesses(goal: &Goal, family: Family, dev_after: &[f64], delta: &[f64]) -> Vec<f64> {
    let (_, _, _, _, highs, air) = family.semantic_bands();
    let (slope, locals) = doctor::tilt_split(dev_after, family, goal.coverage.as_deref());
    let centered = doctor::centered_deviations(dev_after, family);
    goal.rules
        .iter()
        .map(|r| {
            let margin = match r.gate {
                RuleGate::Band {
                    band,
                    sign,
                    gate_tilt,
                    gate_centered,
                } => (sign * locals[band] - gate_tilt).min(sign * centered[band] - gate_centered),
                RuleGate::Tilt { gate } => slope.map_or(f64::NEG_INFINITY, |s| {
                    (s.abs() - gate) * TILT_DB_PER_OCT_SCALE
                }),
                RuleGate::Fizzy { gate } => {
                    goal.air_minus_highs_db + delta[air] - delta[highs] - gate
                }
            };
            (margin + MARGIN_DB).max(0.0)
        })
        .collect()
}

fn apply_moves(goal: &Goal, controls: &[Control], x: &[f64]) -> (Vec<f64>, Vec<f64>) {
    let n = goal.dev.len();
    let mut delta = vec![0.0; n];
    for (c, &xi) in controls.iter().zip(x) {
        if xi != 0.0 {
            for (d, r) in delta.iter_mut().zip(&c.response) {
                *d += r * xi;
            }
        }
    }
    let dev_after = goal.dev.iter().zip(&delta).map(|(d, x)| d + x).collect();
    (dev_after, delta)
}

/// The search objective — see the weight consts.
fn objective(goal: &Goal, family: Family, controls: &[Control], x: &[f64]) -> f64 {
    let (dev_after, delta) = apply_moves(goal, controls, x);
    let excess = rule_excesses(goal, family, &dev_after, &delta);
    let mut j: f64 = goal
        .rules
        .iter()
        .zip(&excess)
        .map(|(r, e)| r.weight * e * e)
        .sum();
    j += COLLATERAL_WEIGHT * delta.iter().map(|d| d * d).sum::<f64>();
    // The positive band change from every NON-exempt control (see BOOST_WEIGHT).
    let mut boost = vec![0.0; delta.len()];
    for (c, &xi) in controls.iter().zip(x) {
        let exempt = matches!(c.remedy, RemedyState::Only { rank: 1, dir, .. } if dir * xi > 0.0);
        if xi != 0.0 && !exempt {
            for (b, r) in boost.iter_mut().zip(&c.response) {
                *b += (r * xi).max(0.0);
            }
        }
    }
    j += BOOST_WEIGHT * boost.iter().map(|d| d * d).sum::<f64>();
    j += MOVE_WEIGHT
        * controls
            .iter()
            .zip(x)
            .map(|(c, xi)| {
                let u = c.reg_unit();
                c.rank_cost() * (xi / u) * (xi / u)
            })
            .sum::<f64>();
    j
}

/// Coordinate descent over each control's QUANTIZED move grid (its step,
/// within its range and move cap): per pass, every control tries every value
/// on its grid against the objective and keeps the best; passes repeat until
/// none changes. Deterministic, gradient-free (the rule margins are
/// piecewise-linear with medians inside), and the result is already on the
/// display grid — returns the per-control moves in each control's own unit,
/// relative to its current value.
pub fn solve(goal: &Goal, family: Family, controls: &[Control]) -> Vec<f64> {
    let m = controls.len();
    let mut x = vec![0.0; m];
    if m == 0 {
        return x;
    }
    let grids: Vec<Vec<f64>> = controls
        .iter()
        .map(|c| {
            let step = c.step();
            let (lo, hi) = c.room();
            let k_lo = (lo / step - 1e-9).ceil() as i64;
            let k_hi = (hi / step + 1e-9).floor() as i64;
            (k_lo..=k_hi)
                .map(|k| k as f64 * step)
                // The remedy layer's direction: a frozen control stays put, a
                // directed one only moves the way its remedy points.
                .filter(|&v| match c.remedy {
                    RemedyState::Free => true,
                    RemedyState::Only { dir, .. } => v == 0.0 || v * dir > 0.0,
                    RemedyState::Frozen => v == 0.0,
                })
                .collect()
        })
        .collect();
    let mut best = objective(goal, family, controls, &x);
    for _ in 0..MAX_PASSES {
        let mut changed = false;
        for j in 0..m {
            let keep = x[j];
            let mut best_v = keep;
            for &v in &grids[j] {
                if v == keep {
                    continue;
                }
                x[j] = v;
                let cand = objective(goal, family, controls, &x);
                if cand < best - 1e-12 {
                    best = cand;
                    best_v = v;
                }
            }
            x[j] = best_v;
            if best_v != keep {
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }
    x
}

// ─── plan assembly ───────────────────────────────────────────────────────────

/// One knob/band move the plan proposes.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PlanMove {
    pub group_id: String,
    pub node_id: String,
    pub model: String,
    /// Catalog display name of the block ("'65 Twin Reverb").
    pub block_name: String,
    pub param: String,
    /// "Bass", "Treble", "250 Hz", "Band 3 (2.6 kHz)".
    pub control_label: String,
    pub unit: ControlUnit,
    /// Raw device values (0..1 knob, or dB).
    pub from: f64,
    pub to: f64,
    /// Player-facing values ("5.0" → "3.5" on the dial; "+2.0" → "−1.0" dB).
    pub from_label: String,
    pub to_label: String,
    /// The remedy rationale ("tames the 3–6 kHz excess that reads bright") —
    /// empty for an unconstrained (fallback) move.
    #[serde(default)]
    pub why: String,
}

/// The balance plan for one diagnosed sound — see the module doc.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TonePlan {
    pub moves: Vec<PlanMove>,
    /// Median-centered deviation per band NOW (the diagnosis's own
    /// `centered_deviations` space), family layout.
    pub before_db: Vec<f64>,
    /// The same, PREDICTED after the moves.
    pub predicted_db: Vec<f64>,
    pub band_labels: Vec<String>,
    /// Finding keys fired now that the re-diagnosis predicts will clear.
    pub clears: Vec<String>,
    /// Findings predicted to still fire after the plan, each with the
    /// quietest level it's predicted to fire at (a finding that moved from
    /// "any volume" to "stage only" is progress the UI can show).
    pub remains: Vec<doctor::LeveledKey>,
    /// Predicted broadband loudness change (dB, band-power sum) — a plan
    /// that moves tone knobs moves level too; the Level tab re-levels.
    pub loudness_delta_db: f64,
    /// [`balance_error_db`] (±1 dB tolerance) before and predicted after — the
    /// distance to the authored balance, for the card.
    pub balance_error_before_db: f64,
    pub balance_error_after_db: f64,
    /// The one-click: every move as a `DoctorOp::Param`, through the standard
    /// apply → A/B → save flow.
    pub rx: Rx,
    /// The response-model identity ([`NOMINAL`]) — the honesty tag the UI
    /// renders as "estimated".
    pub model: &'static str,
}

/// Findings the plan can address — the band/tilt/fizz rules. Localized
/// (resonant/boxy), time-domain (washed/spiky) and the bass `buried` rule
/// have their own prescriptions and are ignored here.
pub const TONAL_KEYS: [&str; 8] = [
    "muddy", "boomy", "harsh", "lost", "thin", "dark", "bright", "fizzy",
];

pub fn is_tonal(key: &str) -> bool {
    TONAL_KEYS.contains(&key)
}

fn knob_dial(v: f64) -> String {
    format!("{:.1}", v * 10.0)
}

fn db_label(v: f64) -> String {
    format!("{}{:.1} dB", if v < 0.0 { "−" } else { "+" }, v.abs())
}

/// Build the balance plan for one diagnosed sound, or `None` when there is
/// nothing to do: no tonal finding fired, the chain has no drivable tone
/// control with a known value, or the solved moves wouldn't clear a finding
/// nor pull a diagnosed band ≥ 2 dB closer to its box.
pub fn generate_plan(
    profile: &SoundProfile,
    nodes: &[DoctorNode],
    family: Family,
    kind: StimulusKind,
    coverage: Option<&[bool]>,
    diags: &[LeveledDiag],
) -> Option<TonePlan> {
    if nodes.is_empty() {
        return None;
    }
    let controls = discover_controls(nodes, family);
    generate_plan_with(profile, nodes, family, kind, coverage, diags, controls)
}

/// The balance report's per-band tolerance (dB around the authored balance) —
/// see [`balance_error_db`].
pub const BALANCE_TOL_DB: f64 = 1.0;
/// Below this severity a predicted new finding is a gate coin-flip, not an
/// introduction the plan must refuse (see `generate_plan_with`).
const INTRODUCE_MIN_SEVERITY: f64 = 0.05;
/// A remaining finding predicted at no more than this fraction of its current
/// severity counts as EASED (see `generate_plan_with`).
const EASED_SEVERITY_RATIO: f64 = 0.5;

/// Diagnostic (probe --doctor-plan-dry): run the solve and report the RAW
/// per-control move it wanted (pre-quantize), the energy before/after, and the
/// per-control objective gradient sign — so "the search proposes nothing" can
/// be traced to the control, not guessed. Never used in production.
pub fn dry_solve_report(
    profile: &SoundProfile,
    nodes: &[DoctorNode],
    family: Family,
    coverage: Option<&[bool]>,
    diags: &[LeveledDiag],
) -> String {
    let mut controls = discover_controls(nodes, family);
    let bdb = doctor::band_db(&profile.bands);
    let dev = doctor::anchor_deviations(
        doctor::deviations(&bdb, family),
        profile.stim_bands.as_deref(),
        family,
    );
    let fired_keys: Vec<&str> = diags
        .iter()
        .map(|d| d.diag.key)
        .filter(|k| is_tonal(k))
        .collect();
    assign_remedies(&mut controls, &fired_keys, family);
    let goal = goal_for(dev.clone(), &bdb, family, nodes, diags, coverage);
    let raw = solve(&goal, family, &controls);
    let (dev_after, _) = apply_moves(&goal, &controls, &raw);
    let e0 = balance_error_db(&dev, family, BALANCE_TOL_DB, coverage);
    let e1 = balance_error_db(&dev_after, family, BALANCE_TOL_DB, coverage);
    let mut out = format!(
        "  dry solve: balance error {e0:.2} → {e1:.2} dB
"
    );
    for (c, x) in controls.iter().zip(&raw) {
        let (lo, hi) = c.room();
        let (region, sign) = dominant_region(c, family);
        let remedy = match c.remedy {
            RemedyState::Free => "free".to_string(),
            RemedyState::Frozen => "frozen (no remedy names it)".to_string(),
            RemedyState::Only { dir, rank, why } => format!(
                "{} only, rank {rank} — {why}",
                if dir > 0.0 { "raise" } else { "lower" }
            ),
        };
        out += &format!(
            "    {} · {}: raw move {x:+.3} (cap ±{:.2}, min {:.2}, room lo {lo:.2} hi {hi:.2}) · {region:?}{} · {remedy}
",
            c.block_name,
            c.label,
            c.max_move(),
            c.min_move(),
            if sign > 0.0 { "+" } else { "−" },
        );
    }
    // Replicate generate_plan_with's gate to show WHICH clause blocks it.
    let moves: Vec<(usize, f64)> = controls
        .iter()
        .enumerate()
        .filter(|(j, c)| raw[*j].abs() + 1e-9 >= c.min_move())
        .map(|(j, c)| (j, c.at(raw[j])))
        .collect();
    let delta_db: Vec<f64> = (0..dev.len())
        .map(|i| {
            moves
                .iter()
                .map(|&(j, to)| controls[j].response[i] * controls[j].travel(to))
                .sum()
        })
        .collect();
    let mut predicted = profile.clone();
    predicted.bands = profile
        .bands
        .iter()
        .zip(&delta_db)
        .map(|(p, d)| p * 10f64.powf(d / 10.0))
        .collect();
    predicted.peaks.clear();
    let fired: Vec<&str> = diags
        .iter()
        .map(|d| d.diag.key)
        .filter(|k| is_tonal(k))
        .collect();
    let after = doctor::diagnose_levels(
        &predicted,
        Some(nodes),
        family,
        StimulusKind::Synthetic,
        coverage,
    );
    let after_keys: Vec<&str> = after
        .iter()
        .map(|d| d.diag.key)
        .filter(|k| is_tonal(k))
        .collect();
    let introduces: Vec<&str> = after_keys
        .iter()
        .filter(|k| !fired.contains(k))
        .copied()
        .collect();
    out += &format!(
        "  gate: survivors {} · fired-before {:?} · fired-after {:?} · introduces {:?}
",
        moves.len(),
        fired,
        after_keys,
        introduces
    );
    out
}

/// [`generate_plan`] over a caller-supplied control set (the knob sweep's
/// dry-run and the tests): a plan needs a fired tonal finding and must clear
/// or ease one; it never trades one finding for another.
pub fn generate_plan_with(
    profile: &SoundProfile,
    nodes: &[DoctorNode],
    family: Family,
    kind: StimulusKind,
    coverage: Option<&[bool]>,
    diags: &[LeveledDiag],
    mut controls: Vec<Control>,
) -> Option<TonePlan> {
    let fired: Vec<&str> = diags
        .iter()
        .map(|d| d.diag.key)
        .filter(|k| is_tonal(k))
        .collect();
    if fired.is_empty() || nodes.is_empty() || controls.is_empty() {
        return None;
    }
    let bdb = doctor::band_db(&profile.bands);
    let dev = doctor::anchor_deviations(
        doctor::deviations(&bdb, family),
        profile.stim_bands.as_deref(),
        family,
    );
    // The remedy layer decides WHICH way each lever may move for these findings
    // before the search runs — see the "remedy knowledge" section.
    assign_remedies(&mut controls, &fired, family);
    let goal = goal_for(dev.clone(), &bdb, family, nodes, diags, coverage);
    let raw = solve(&goal, family, &controls);

    // One candidate at a fraction `scale` of the solved move: the quantized
    // surviving moves, their model delta, the re-diagnosed key sets, and whether
    // it's worth shipping. `None` = nothing survives quantization.
    struct Candidate {
        moves: Vec<(usize, f64)>,
        delta_db: Vec<f64>,
        after: Vec<LeveledDiag>,
        clears: Vec<String>,
        remains: Vec<String>,
        introduces: Vec<String>,
        eased: Vec<String>,
    }
    let level_rank = |l: crate::profiles::PlaybackLevel| match l {
        crate::profiles::PlaybackLevel::Quiet => 0,
        crate::profiles::PlaybackLevel::Rehearsal => 1,
        crate::profiles::PlaybackLevel::Stage => 2,
    };
    let evaluate_from = |vec: &[f64]| -> Option<Candidate> {
        let moves: Vec<(usize, f64)> = controls
            .iter()
            .enumerate()
            .filter(|(j, c)| vec[*j].abs() + 1e-9 >= c.min_move())
            .map(|(j, c)| (j, c.at(vec[j])))
            .collect();
        if moves.is_empty() {
            return None;
        }
        let delta_db: Vec<f64> = (0..dev.len())
            .map(|i| {
                moves
                    .iter()
                    .map(|&(j, to)| controls[j].response[i] * controls[j].travel(to))
                    .sum()
            })
            .collect();
        let mut predicted = profile.clone();
        predicted.bands = profile
            .bands
            .iter()
            .zip(&delta_db)
            .map(|(p, d)| p * 10f64.powf(d / 10.0))
            .collect();
        predicted.peaks.clear();
        let after = doctor::diagnose_levels(&predicted, Some(nodes), family, kind, coverage);
        let after_keys: Vec<&str> = after
            .iter()
            .map(|d| d.diag.key)
            .filter(|k| is_tonal(k))
            .collect();
        let clears: Vec<String> = fired
            .iter()
            .filter(|k| !after_keys.contains(k))
            .map(|k| (*k).to_string())
            .collect();
        let remains: Vec<String> = fired
            .iter()
            .filter(|k| after_keys.contains(k))
            .map(|k| (*k).to_string())
            .collect();
        // A finding predicted to appear at a severity that rounds to zero is a
        // gate coin-flip (the median re-centering lands a band exactly on its
        // gate), not a regression — MARGIN_DB can't cover the centered space.
        let introduces: Vec<String> = after_keys
            .iter()
            .filter(|k| !fired.contains(k))
            .filter(|k| {
                after
                    .iter()
                    .filter(|d| &d.diag.key == *k)
                    .any(|d| d.diag.severity >= INTRODUCE_MIN_SEVERITY)
            })
            .map(|k| (*k).to_string())
            .collect();
        // EASED: the finding still fires, but only at a louder level than before,
        // or at no more than half its severity — a capped move set (±3.5 dial
        // steps, one octave of cut) legitimately takes a big tilt down in steps,
        // and a plan that halves a finding is worth auditioning, not withholding.
        let eased: Vec<String> = remains
            .iter()
            .filter(|k| {
                let before = diags.iter().find(|d| d.diag.key == k.as_str());
                let aft = after.iter().find(|d| d.diag.key == k.as_str());
                matches!((before, aft), (Some(b), Some(a))
                    if level_rank(a.from_level) > level_rank(b.from_level)
                        || a.diag.severity <= EASED_SEVERITY_RATIO * b.diag.severity)
            })
            .cloned()
            .collect();
        Some(Candidate {
            moves,
            delta_db,
            after,
            clears,
            remains,
            introduces,
            eased,
        })
    };

    let _ = &evaluate_from;
    // The bands an introduced finding lives in (for the greedy drop below).
    let finding_bands = |keys: &[String]| -> Vec<usize> {
        let (lows, low_mids, mids, high_mids, highs, air) = family.semantic_bands();
        let mut out = Vec::new();
        for k in keys {
            out.extend(match k.as_str() {
                "muddy" => vec![low_mids],
                "boomy" | "thin" | "buried" => vec![lows],
                "harsh" => vec![high_mids],
                "lost" => vec![mids],
                "fizzy" => vec![air],
                "dark" | "bright" => vec![highs, air],
                _ => vec![],
            });
        }
        out
    };
    // GREEDY back-off: the full solved move can over-correct along ONE lever and
    // INTRODUCE a finding while another lever was doing the real work (HW
    // 2026-08-23: a bright Strat's polish cut the highs — good — AND boosted the
    // lows into `muddy`; scaling the whole vector down shrinks the useful cut
    // too). So keep the full move, then while it introduces a finding, DROP the
    // single move that pushes hardest into that finding's band, and re-diagnose —
    // the character-preserving cut survives, the offending boost is dropped.
    let mut keep: Vec<usize> = (0..controls.len()).collect();
    let mut cand: Option<Candidate> = None;
    for _ in 0..=controls.len() {
        let mut scaled = vec![0.0; controls.len()];
        for &j in &keep {
            scaled[j] = raw[j];
        }
        let Some(c) = evaluate_from(&scaled) else {
            break;
        };
        // The plan is unattended, so it drops on ANY introduction.
        let block: Vec<String> = c.introduces.clone();
        if block.is_empty() {
            cand = Some(c);
            break;
        }
        // Drop the kept move that pushes the blocking finding's band(s) hardest
        // in the firing direction (a boost into muddy/boomy, a cut into lost…).
        let bands = finding_bands(&block);
        let worst = keep
            .iter()
            .copied()
            .filter(|&j| raw[j].abs() >= controls[j].min_move())
            .max_by(|&a, &b| {
                let push = |j: usize| {
                    bands
                        .iter()
                        .map(|&band| controls[j].response[band] * raw[j])
                        .sum::<f64>()
                        .abs()
                };
                push(a).total_cmp(&push(b))
            });
        match worst {
            Some(j) => keep.retain(|&k| k != j),
            None => break,
        }
    }
    let cand =
        cand.filter(|c| c.introduces.is_empty() && (!c.clears.is_empty() || !c.eased.is_empty()))?;

    let Candidate {
        moves,
        delta_db,
        after,
        clears,
        remains,
        introduces: _,
        eased: _,
    } = cand;
    let dev_after: Vec<f64> = dev.iter().zip(&delta_db).map(|(d, x)| d + x).collect();
    let balance_error_before_db = balance_error_db(&dev, family, BALANCE_TOL_DB, coverage);
    let balance_error_after_db = balance_error_db(&dev_after, family, BALANCE_TOL_DB, coverage);
    let mut predicted = profile.clone();
    predicted.bands = profile
        .bands
        .iter()
        .zip(&delta_db)
        .map(|(p, d)| p * 10f64.powf(d / 10.0))
        .collect();
    let remains: Vec<doctor::LeveledKey> = after
        .iter()
        .filter(|d| remains.iter().any(|k| k == d.diag.key))
        .map(|d| doctor::LeveledKey {
            key: d.diag.key.to_string(),
            from_level: d.from_level,
        })
        .collect();
    let before_c = doctor::centered_deviations(&dev, family);
    let after_c = doctor::centered_deviations(&dev_after, family);

    let loudness_delta_db = {
        let before: f64 = profile.bands.iter().sum();
        let after: f64 = predicted.bands.iter().sum();
        if before > 0.0 && after > 0.0 {
            10.0 * (after / before).log10()
        } else {
            0.0
        }
    };

    let plan_moves: Vec<PlanMove> = moves
        .iter()
        .map(|&(j, to)| {
            let c = &controls[j];
            let (from_label, to_label) = (c.value_label(c.current), c.value_label(to));
            let why = match c.remedy {
                RemedyState::Only { why, .. } => why.to_string(),
                _ => String::new(),
            };
            PlanMove {
                group_id: c.group_id.clone(),
                node_id: c.node_id.clone(),
                model: c.model.clone(),
                block_name: c.block_name.clone(),
                param: c.param.clone(),
                control_label: c.label.clone(),
                unit: c.unit,
                from: c.current,
                to,
                from_label,
                to_label,
                why,
            }
        })
        .collect();

    // One line per block: "'65 Twin Reverb: Bass 5.0 → 3.5, Treble 5.0 → 6.0".
    let mut by_block: Vec<(String, Vec<String>)> = Vec::new();
    for m in &plan_moves {
        let line = format!("{} {} → {}", m.control_label, m.from_label, m.to_label);
        match by_block.iter_mut().find(|(name, _)| *name == m.block_name) {
            Some((_, lines)) => lines.push(line),
            None => by_block.push((m.block_name.clone(), vec![line])),
        }
    }
    let detail = by_block
        .iter()
        .map(|(name, lines)| format!("{name}: {}", lines.join(", ")))
        .collect::<Vec<_>>()
        .join(" · ");
    let rx = Rx {
        kind: RxKind::OneClick,
        title: "Rebalance with the blocks you have".to_string(),
        detail,
        cpu_note: "no CPU change".to_string(),
        ops: plan_moves
            .iter()
            .map(|m| DoctorOp::Param {
                group_id: m.group_id.clone(),
                node_id: m.node_id.clone(),
                param: m.param.clone(),
                value: m.to,
            })
            .collect(),
        chain: None,
    };
    Some(TonePlan {
        moves: plan_moves,
        before_db: before_c,
        predicted_db: after_c,
        band_labels: family.labels_owned(),
        clears,
        remains,
        loudness_delta_db,
        balance_error_before_db,
        balance_error_after_db,
        rx,
        model: NOMINAL,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn node(group: &str, model: &str, params: &[(&str, f64)]) -> DoctorNode {
        DoctorNode {
            group_id: group.to_string(),
            node_id: model.to_string(),
            model: model.to_string(),
            bypassed: false,
            cab_sim_id: None,
            cab_sim2_enabled: None,
            params: params.iter().map(|(k, v)| ((*k).to_string(), *v)).collect(),
        }
    }

    fn twin(bass: f64, mid: f64, treb: f64) -> DoctorNode {
        node(
            "G1",
            "ACD_TwinReverb65NoFx",
            &[
                ("bass", bass),
                ("mid", mid),
                ("treb", treb),
                ("gain", 0.5),
                ("outputLevel", 0.5),
            ],
        )
    }

    /// A profile sitting on the guitar target plus a per-band delta (dB).
    fn profile_with(delta: [f64; 6]) -> SoundProfile {
        let target = doctor::target_curve(Family::Guitar);
        SoundProfile {
            bands: target
                .iter()
                .zip(delta)
                .map(|(t, d)| 10f64.powf((t + d) / 10.0))
                .collect(),
            integrated_lufs: -18.0,
            spread_lu: 0.0,
            tail_ratio_db: -80.0,
            air_flatness: 0.5,
            peaks: Vec::new(),
            stim_bands: None,
        }
    }

    fn diags_for(profile: &SoundProfile, nodes: &[DoctorNode]) -> Vec<LeveledDiag> {
        doctor::diagnose_levels(
            profile,
            Some(nodes),
            Family::Guitar,
            StimulusKind::Synthetic,
            None,
        )
    }

    fn keys(d: &[LeveledDiag]) -> Vec<&'static str> {
        d.iter().map(|d| d.diag.key).collect()
    }

    #[test]
    fn tone_param_keys_are_the_tone_stack_and_parametric_bands() {
        for k in [
            "bass",
            "mid",
            "middle",
            "treb",
            "treble",
            "presence",
            "pres",
            "tone",
            "cut",
            "filter3gaindb",
            "filter3frequency",
            "filter3q",
            "filter1type",
        ] {
            assert!(is_tone_param_key(k), "{k} must be allowlisted");
        }
        for k in [
            "gain",
            "outputLevel",
            "volume",
            "mastervolume",
            "bypass",
            "filterorder",
            "filter3order",
            "leveldb",
            "cabsimid",
        ] {
            assert!(!is_tone_param_key(k), "{k} must NOT be allowlisted");
        }
    }

    #[test]
    fn graphic_band_keys_parse_to_hz() {
        assert_eq!(graphic_band_hz("gain250hz"), Some(250.0));
        assert_eq!(graphic_band_hz("gain1khz"), Some(1000.0));
        assert_eq!(graphic_band_hz("gain16khz"), Some(16000.0));
        assert_eq!(graphic_band_hz("overallgain"), None);
        assert_eq!(graphic_band_hz("gainhz"), None);
    }

    #[test]
    fn amp_bass_response_is_a_low_shelf() {
        let r = band_response(nominal_shapes(ToneKind::AmpBass), 1.0, Family::Guitar);
        assert!(r[0] > 12.0, "lows {r:?}");
        assert!(r[1] > r[2] && r[2] > r[3], "monotone down {r:?}");
        assert!(r[5] < 0.5, "air untouched {r:?}");
    }

    #[test]
    fn amp_treble_response_is_a_high_shelf() {
        let r = band_response(nominal_shapes(ToneKind::AmpTreble), 1.0, Family::Guitar);
        assert!(r[0] < 0.5, "lows untouched {r:?}");
        assert!(r[3] > 5.0 && r[4] > r[3] && r[5] > r[4], "{r:?}");
    }

    #[test]
    fn vox_cut_pulls_the_top_down() {
        let r = band_response(nominal_shapes(ToneKind::AmpCut), 1.0, Family::Guitar);
        assert!(r[4] < -3.0 && r[5] < r[4], "{r:?}");
        assert!(r[0].abs() < 0.1);
    }

    #[test]
    fn graphic_band_response_centers_on_its_band() {
        let shape = Shape::Peak {
            fc: 250.0,
            bw_oct: GRAPHIC_BW_OCT,
            g: 1.0,
        };
        let r = band_response(&[shape], 1.0, Family::Guitar);
        // The 120–400 Hz band takes most of a 250 Hz band's dB.
        assert!(r[1] > 0.5, "{r:?}");
        assert!(r[1] > r[0] && r[1] > r[2], "{r:?}");
        assert!(r[4].abs() < 0.05, "{r:?}");
    }

    #[test]
    fn discover_reads_amp_knobs_with_known_values_only() {
        let nodes = vec![
            node("G1", "ACD_TubeScreamer", &[("tone", 0.5), ("level", 0.6)]),
            twin(0.5, 0.5, 0.5),
            // Bypassed drive: skipped entirely.
            DoctorNode {
                bypassed: true,
                ..node("G1", "ACD_Rat", &[("tone", 0.2)])
            },
            // Mic lane: skipped.
            node("M1", "ACD_TwinReverb65NoFx", &[("bass", 0.5)]),
        ];
        let c = discover_controls(&nodes, Family::Guitar);
        let names: Vec<String> = c
            .iter()
            .map(|c| format!("{}:{}", c.node_id, c.param))
            .collect();
        assert_eq!(
            names,
            vec![
                "ACD_TubeScreamer:tone",
                "ACD_TwinReverb65NoFx:bass",
                "ACD_TwinReverb65NoFx:mid",
                "ACD_TwinReverb65NoFx:treb",
            ]
        );
        assert_eq!(c[1].block_name, "'65 Twin Reverb");
        assert_eq!(c[1].label, "Bass");
        assert_eq!(c[0].unit, ControlUnit::Knob);
    }

    #[test]
    fn discover_reads_parametric_peaks_but_not_the_pass_bands() {
        let nodes = vec![node(
            "G2",
            "ACD_FiveBandParamEQ",
            &[
                ("filter1frequency", 45.0),
                ("filter1gaindb", 0.0),
                ("filter1q", 0.71),
                ("filter1type", 0.0),
                ("filter3frequency", 2600.0),
                ("filter3gaindb", 12.0),
                ("filter3q", 14.0),
                ("filter3type", 2.0),
            ],
        )];
        let c = discover_controls(&nodes, Family::Guitar);
        assert_eq!(c.len(), 1);
        assert_eq!(c[0].param, "filter3gaindb");
        assert_eq!(c[0].label, "Band 3 (2.6 kHz)");
        assert_eq!(c[0].unit, ControlUnit::Db);
        // A Q-14 peak at 2.6 kHz lives in High-mids only.
        assert!(
            c[0].response[3] > 0.0 && c[0].response[3] < 0.5,
            "{:?}",
            c[0].response
        );
        assert!(c[0].response[1].abs() < 1e-3);
    }

    #[test]
    fn muddy_twin_gets_a_bass_cut_and_is_predicted_clear() {
        // +7 dB low-mid bump on the target — the showcase muddy case shape.
        let profile = profile_with([0.0, 7.0, 0.0, 0.0, 0.0, 0.0]);
        let nodes = vec![twin(0.6, 0.5, 0.5)];
        let diags = diags_for(&profile, &nodes);
        assert!(keys(&diags).contains(&"muddy"), "{:?}", keys(&diags));
        let plan = generate_plan(
            &profile,
            &nodes,
            Family::Guitar,
            StimulusKind::Synthetic,
            None,
            &diags,
        )
        .expect("a muddy Twin with a Bass knob must get a plan");
        let bass = plan
            .moves
            .iter()
            .find(|m| m.param == "bass")
            .expect("the plan turns Bass down");
        assert!(bass.to < bass.from, "{bass:?}");
        assert!(plan.clears.iter().any(|k| k == "muddy"), "{plan:?}");
        assert!(plan.remains.is_empty(), "{plan:?}");
        assert!(plan.rx.ops.len() == plan.moves.len());
        assert!(
            plan.rx.detail.contains("'65 Twin Reverb"),
            "{}",
            plan.rx.detail
        );
        assert!(plan.rx.detail.contains("Bass 6.0 →"), "{}", plan.rx.detail);
        // Every move is inside the knob's range and the move cap.
        for m in &plan.moves {
            assert!((0.0..=1.0).contains(&m.to));
            assert!((m.to - m.from).abs() <= 0.35 + 1e-9);
        }
    }

    #[test]
    fn dark_preset_opens_treble() {
        // A −4 dB/oct dark tilt (relative to the target) across the body.
        let xs: Vec<f64> = Family::Guitar
            .band_centers()
            .iter()
            .map(|c| c.log2())
            .collect();
        let mut delta = [0.0; 6];
        for i in 0..6 {
            delta[i] = -4.0 * (xs[i] - xs[2]);
        }
        let profile = profile_with(delta);
        let nodes = vec![twin(0.5, 0.5, 0.4)];
        let diags = diags_for(&profile, &nodes);
        assert!(keys(&diags).contains(&"dark"), "{:?}", keys(&diags));
        let plan = generate_plan(
            &profile,
            &nodes,
            Family::Guitar,
            StimulusKind::Synthetic,
            None,
            &diags,
        )
        .expect("plan");
        let treb = plan
            .moves
            .iter()
            .find(|m| m.param == "treb")
            .expect("treble up");
        assert!(treb.to > treb.from, "{treb:?}");
    }

    fn cab(hpf: f64, lpf: f64) -> DoctorNode {
        node("G3", "ACD_CabSimTMS", &[("hpf", hpf), ("lpf", lpf)])
    }

    /// A +4 dB/oct bright tilt across the body — the HW shape (a bright Strat →
    /// JCM800 crunch, 2026-08-28) whose unconstrained solve boosted Bass and the
    /// 144 Hz band to even the balance and left the player hearing "still bright".
    fn bright_profile() -> SoundProfile {
        let xs: Vec<f64> = Family::Guitar
            .band_centers()
            .iter()
            .map(|c| c.log2())
            .collect();
        let mut delta = [0.0; 6];
        for i in 0..6 {
            delta[i] = 4.0 * (xs[i] - xs[2]);
        }
        profile_with(delta)
    }

    #[test]
    fn a_bright_sound_is_tamed_from_the_top_never_by_a_bass_boost() {
        let profile = bright_profile();
        let nodes = vec![twin(0.5, 0.5, 0.6), cab(140.0, 9000.0)];
        let diags = diags_for(&profile, &nodes);
        assert!(keys(&diags).contains(&"bright"), "{:?}", keys(&diags));
        {
            let controls = discover_controls(&nodes, Family::Guitar);
            let plan = generate_plan_with(
                &profile,
                &nodes,
                Family::Guitar,
                StimulusKind::Synthetic,
                None,
                &diags,
                controls,
            )
            .expect("a bright Twin + cab gets a plan");
            // Every move is a CUT of the top (treble down, high cut lowered, …) —
            // never a bass boost to "even it out".
            for m in &plan.moves {
                match m.param.as_str() {
                    "bass" | "hpf" => panic!("a bright fix must not touch the low end: {m:?}"),
                    "treb" | "presence" | "tone" => assert!(m.to < m.from, "{m:?}"),
                    "lpf" => assert!(m.to < m.from, "{m:?}"),
                    _ => {}
                }
                assert!(
                    !m.why.is_empty(),
                    "every remedy move carries its rationale: {m:?}"
                );
            }
            assert!(
                plan.moves
                    .iter()
                    .any(|m| m.param == "treb" || m.param == "lpf"),
                "{:?}",
                plan.moves
            );
        }
    }

    #[test]
    fn a_muddy_sound_reaches_for_the_cab_low_cut() {
        let profile = profile_with([3.0, 7.0, 0.0, 0.0, 0.0, 0.0]);
        let nodes = vec![twin(0.5, 0.5, 0.5), cab(80.0, 9000.0)];
        let diags = diags_for(&profile, &nodes);
        assert!(keys(&diags).contains(&"muddy"), "{:?}", keys(&diags));
        let plan = generate_plan(
            &profile,
            &nodes,
            Family::Guitar,
            StimulusKind::Synthetic,
            None,
            &diags,
        )
        .expect("plan");
        for m in &plan.moves {
            match m.param.as_str() {
                "hpf" => assert!(m.to > m.from, "the low cut is RAISED to tighten: {m:?}"),
                "lpf" => panic!("mud is not fixed at the top: {m:?}"),
                "bass" | "mid" => assert!(m.to < m.from, "{m:?}"),
                _ => {}
            }
        }
        // The cab's cut is discovered as a Hz control with an octave-scaled move
        // that lands on a dial-able value.
        let hpf = discover_controls(&nodes, Family::Guitar)
            .into_iter()
            .find(|c| c.param == "hpf")
            .expect("hpf discovered");
        assert_eq!(hpf.unit, ControlUnit::Hz);
        assert!(
            (hpf.travel(160.0) - 1.0).abs() < 1e-9,
            "80 → 160 Hz is +1 octave"
        );
        assert_eq!(hpf.at(0.5), 115.0, "80·√2 = 113 rounds to a 5 Hz step");
        assert!(
            hpf.response[0] < 0.0 && hpf.response[5].abs() < 0.1,
            "{:?}",
            hpf.response
        );
    }

    #[test]
    fn the_remedy_layer_frees_every_control_when_none_matches() {
        // A bright sound whose only lever is a Bass knob: nothing cuts the top, so
        // the chain falls back to the unconstrained search rather than proposing
        // nothing at all.
        let profile = bright_profile();
        let nodes = vec![node(
            "G1",
            "ACD_TwinReverb65NoFx",
            &[("bass", 0.5), ("gain", 0.5), ("outputLevel", 0.5)],
        )];
        let diags = diags_for(&profile, &nodes);
        let mut controls = discover_controls(&nodes, Family::Guitar);
        assert_eq!(controls.len(), 1);
        let fired: Vec<&str> = diags.iter().map(|d| d.diag.key).collect();
        assign_remedies(&mut controls, &fired, Family::Guitar);
        assert_eq!(controls[0].remedy, RemedyState::Free);
    }

    #[test]
    fn no_tonal_finding_means_no_plan() {
        let profile = profile_with([0.0; 6]);
        let nodes = vec![twin(0.5, 0.5, 0.5)];
        let diags = diags_for(&profile, &nodes);
        assert!(diags.is_empty());
        assert!(generate_plan(
            &profile,
            &nodes,
            Family::Guitar,
            StimulusKind::Synthetic,
            None,
            &diags
        )
        .is_none());
    }

    #[test]
    fn no_drivable_control_means_no_plan() {
        let profile = profile_with([0.0, 7.0, 0.0, 0.0, 0.0, 0.0]);
        // A cab + reverb only: nothing to turn.
        let nodes = vec![
            node("G1", "ACD_CabSimTMS", &[("hpf", 80.0)]),
            node("G2", "ACD_TMSpring63", &[("mix", 0.2)]),
        ];
        let diags = diags_for(&profile, &nodes);
        assert!(!diags.is_empty());
        assert!(generate_plan(
            &profile,
            &nodes,
            Family::Guitar,
            StimulusKind::Synthetic,
            None,
            &diags
        )
        .is_none());
    }

    #[test]
    fn a_knob_already_at_its_floor_cannot_go_lower() {
        let profile = profile_with([0.0, 7.0, 0.0, 0.0, 0.0, 0.0]);
        let nodes = vec![twin(0.0, 0.5, 0.5)];
        let diags = diags_for(&profile, &nodes);
        let plan = generate_plan(
            &profile,
            &nodes,
            Family::Guitar,
            StimulusKind::Synthetic,
            None,
            &diags,
        );
        if let Some(plan) = plan {
            assert!(plan.moves.iter().all(|m| m.param != "bass"), "{plan:?}");
            for m in &plan.moves {
                assert!(m.to >= 0.0 && m.to <= 1.0);
            }
        }
    }

    #[test]
    fn eq10_band_cut_is_preferred_for_a_narrow_low_mid_bump() {
        let profile = profile_with([0.0, 7.0, 0.0, 0.0, 0.0, 0.0]);
        let eq: Vec<(&str, f64)> = [
            "gain31hz",
            "gain62hz",
            "gain125hz",
            "gain250hz",
            "gain500hz",
            "gain1khz",
            "gain2khz",
            "gain4khz",
            "gain8khz",
            "gain16khz",
        ]
        .iter()
        .map(|k| (*k, 0.0))
        .collect();
        let nodes = vec![node("G3", "ACD_TenBandEQStereo", &eq)];
        let diags = diags_for(&profile, &nodes);
        let plan = generate_plan(
            &profile,
            &nodes,
            Family::Guitar,
            StimulusKind::Synthetic,
            None,
            &diags,
        )
        .expect("plan");
        let cut = plan
            .moves
            .iter()
            .find(|m| m.param == "gain250hz")
            .expect("250 Hz cut");
        assert!(cut.to < 0.0, "{cut:?}");
        assert_eq!(cut.unit, ControlUnit::Db);
        assert!(cut.to_label.starts_with('−'), "{}", cut.to_label);
        assert!(
            ((cut.to * 2.0).round() - cut.to * 2.0).abs() < 1e-9,
            "half-dB steps: {cut:?}"
        );
        assert!(plan.clears.iter().any(|k| k == "muddy"));
    }

    /// The solver's gate transcription must fire exactly when the real rules
    /// do (rehearsal offsets, no fired findings → rehearsal gates everywhere)
    /// — pinned over a deterministic spread of band vectors.
    #[test]
    fn rule_gates_agree_with_the_diagnosis() {
        let mut seed: u64 = 0x9e37_79b9_7f4a_7c15;
        let mut next = || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            (seed % 2401) as f64 / 100.0 - 12.0
        };
        let mut checked = 0;
        for _ in 0..400 {
            let delta = [next(), next(), next(), next(), next(), next()];
            let profile = profile_with(delta);
            let real: Vec<&str> = doctor::diagnose_kind(
                &profile,
                None,
                Family::Guitar,
                StimulusKind::Synthetic,
                None,
                doctor::PlaybackOffsets::NONE,
            )
            .iter()
            .map(|d| d.key)
            .filter(|k| is_tonal(k))
            .collect();
            let bdb = doctor::band_db(&profile.bands);
            let dev = doctor::deviations(&bdb, Family::Guitar);
            let goal = goal_for(dev.clone(), &bdb, Family::Guitar, &[], &[], None);
            let delta0 = vec![0.0; 6];
            let excess = rule_excesses(&goal, Family::Guitar, &dev, &delta0);
            let mine: Vec<&str> = goal
                .rules
                .iter()
                .zip(&excess)
                .filter(|(_, e)| **e > MARGIN_DB + 1e-9)
                .map(|(r, _)| r.key)
                .collect();
            let mut real_keys: Vec<&str> = real
                .iter()
                .map(|k| if *k == "bright" { "dark" } else { k })
                .collect();
            real_keys.sort_unstable();
            let mut mine_keys = mine.clone();
            mine_keys.sort_unstable();
            assert_eq!(mine_keys, real_keys, "delta {delta:?}");
            checked += usize::from(!real.is_empty());
        }
        assert!(
            checked > 50,
            "the spread must exercise firing cases: {checked}"
        );
    }

    #[test]
    fn block_names_resolve_through_suffix_stripping() {
        assert_eq!(
            block_display_name("ACD_DeluxeReverb65BlondeVibratoNoFxCabIR"),
            "'65 Deluxe Reverb Blonde NBC".to_string()
        );
        assert_eq!(block_display_name("ACD_TubeScreamer"), "Greenbox 8");
        assert_eq!(block_display_name("ACD_NoSuchThing"), "NoSuchThing");
    }
}
