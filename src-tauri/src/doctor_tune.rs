//! Doctor "tune loop" — the closed-loop balance search behind the Results
//! page's "Search for a better balance" button. PURE: no device I/O, no Tauri;
//! `commands/doctor_tune.rs` owns the device rounds and the per-sound session.
//!
//! One ROUND = propose a set of knob/band moves from the current baseline →
//! apply them live → capture → diagnose → show the player the A/B, the
//! measured band change and the findings that cleared. The player then says
//! **better** (the candidate becomes the new baseline and the next round starts
//! from it), **not better** (the candidate is rejected and the next round
//! proposes a DIFFERENT move set — see [`exclusions`] / [`cap_for_variant`]),
//! **save** or **stop**. Rounds continue until no tonal finding remains, the
//! model has nothing left to move, or the player stops.
//!
//! What makes it a SEARCH rather than the one-shot plan repeated: every round's
//! measured band change calibrates the response model ([`calibrate`]) — the
//! nominal tone-stack shapes (`doctor_plan::NOMINAL`) are scaled per control by
//! what THIS preset's amp actually did, so round 2 already moves with the
//! device's own sensitivities (a Twin whose Bass moved 9 dB where the model
//! said 16 gets a 0.56 scale on Bass and keeps its shape). Ridge-regularized
//! toward 1.0 so a single noisy round can't throw the model; clamped to
//! [0.25, 4]. Rejections are data too — a rejected round's measurement still
//! calibrates; only the player's TASTE is what "not better" records, and that is
//! honored by excluding the rejected moves' controls from the next proposal.

use std::collections::{HashMap, HashSet};

use serde::{Deserialize, Serialize};

use crate::doctor::{DoctorNode, DoctorOp, Family, LeveledDiag, SoundProfile, StimulusKind};
use crate::doctor_plan::{self, Control, PlanMove, TonePlan};

/// One measured round: the moves it made FROM ITS BASELINE, the cumulative ops
/// (absolute values relative to the SAVED preset) that reproduce it, what the
/// device measured, and the player's verdict.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Trial {
    pub moves: Vec<PlanMove>,
    pub ops: Vec<DoctorOp>,
    /// Mean-removed band dB of the candidate capture (`doctor::balance`).
    pub balance_db: Vec<f64>,
    /// `candidate − baseline` per band (dB, balance space).
    pub delta_db: Vec<f64>,
    pub accepted: bool,
}

/// Identity of a control across rounds.
fn key_of(node_id: &str, param: &str) -> String {
    format!("{node_id}\u{1f}{param}")
}

/// Ridge strength toward scale 1.0 (in units of (dB·unit)²): a round that moved
/// a control by one full cap and measured a clean 6-band response pulls the
/// scale ~halfway toward the observed ratio; a second agreeing round most of
/// the rest of the way.
const RIDGE_LAMBDA: f64 = 30.0;
const SCALE_MIN: f64 = 0.25;
const SCALE_MAX: f64 = 4.0;

/// Per-control response scale factors from the measured rounds — the least
/// squares fit of `Δbalance ≈ Σ_c s_c · (r_c − mean r_c) · Δx_c` over every
/// trial (accepted or not), ridge-pulled toward 1. Controls no round moved keep
/// 1.0. Returns `(control key, scale)` for the moved ones.
pub fn calibrate(controls: &[Control], trials: &[Trial]) -> Vec<(String, f64)> {
    // Mean-removed responses (balance space is mean-removed over ALL bands).
    let centered: HashMap<String, Vec<f64>> = controls
        .iter()
        .map(|c| {
            let m = c.response.iter().sum::<f64>() / c.response.len().max(1) as f64;
            (
                key_of(&c.node_id, &c.param),
                c.response.iter().map(|r| r - m).collect(),
            )
        })
        .collect();
    let mut cols: Vec<String> = Vec::new();
    for t in trials {
        for m in &t.moves {
            let k = key_of(&m.node_id, &m.param);
            if centered.contains_key(&k) && !cols.contains(&k) {
                cols.push(k);
            }
        }
    }
    if cols.is_empty() {
        return Vec::new();
    }
    let n = cols.len();
    // Normal equations: (AᵀA + λI) s = Aᵀb + λ·1.
    let mut ata = vec![vec![0.0; n]; n];
    let mut atb = vec![0.0; n];
    for t in trials {
        let bands = t.delta_db.len();
        for i in 0..bands {
            // Row: Δb_i = Σ_c s_c · r_c,i · Δx_c
            let row: Vec<f64> = cols
                .iter()
                .map(|k| {
                    t.moves
                        .iter()
                        .filter(|m| key_of(&m.node_id, &m.param) == *k)
                        .map(|m| centered[k].get(i).copied().unwrap_or(0.0) * (m.to - m.from))
                        .sum::<f64>()
                })
                .collect();
            for a in 0..n {
                atb[a] += row[a] * t.delta_db[i];
                for b in 0..n {
                    ata[a][b] += row[a] * row[b];
                }
            }
        }
    }
    for a in 0..n {
        ata[a][a] += RIDGE_LAMBDA;
        atb[a] += RIDGE_LAMBDA;
    }
    let s = solve_dense(ata, atb);
    cols.into_iter()
        .zip(s)
        .map(|(k, v)| (k, v.clamp(SCALE_MIN, SCALE_MAX)))
        .collect()
}

/// Gaussian elimination with partial pivoting (n ≤ ~20). A singular system
/// (impossible with the ridge term) falls back to all-ones.
fn solve_dense(mut a: Vec<Vec<f64>>, mut b: Vec<f64>) -> Vec<f64> {
    let n = b.len();
    for col in 0..n {
        let pivot = (col..n)
            .max_by(|&x, &y| a[x][col].abs().total_cmp(&a[y][col].abs()))
            .unwrap_or(col);
        if a[pivot][col].abs() < 1e-12 {
            return vec![1.0; n];
        }
        a.swap(col, pivot);
        b.swap(col, pivot);
        for r in col + 1..n {
            let f = a[r][col] / a[col][col];
            let pivot_row = a[col].clone();
            for (x, p) in a[r].iter_mut().zip(&pivot_row).skip(col) {
                *x -= f * p;
            }
            b[r] -= f * b[col];
        }
    }
    let mut x = vec![0.0; n];
    for r in (0..n).rev() {
        let mut acc = b[r];
        for c in r + 1..n {
            acc -= a[r][c] * x[c];
        }
        x[r] = acc / a[r][r];
    }
    x
}

/// Apply the learned scales to a fresh discovery.
pub fn apply_scales(controls: &mut [Control], scales: &[(String, f64)]) {
    for c in controls.iter_mut() {
        let k = key_of(&c.node_id, &c.param);
        if let Some((_, s)) = scales.iter().find(|(kk, _)| *kk == k) {
            for r in c.response.iter_mut() {
                *r *= s;
            }
        }
    }
}

/// Controls to leave alone on the next proposal after `rejected` rounds (the
/// ones since the last acceptance, oldest first), by how many times the player
/// has said "not better" in a row:
/// 1 → the single control the last rejected round leaned on most (its largest
///     move in cap units) — the most likely taste offender;
/// 2 → every control any rejected round moved — a different family of fix;
/// 3+ → nothing excluded (the caller halves the move cap instead — a gentler
///     version of the honest fix); past [`MAX_VARIANTS`] the caller gives up.
pub fn exclusions(rejected: &[&Trial], variant: u32) -> HashSet<String> {
    let mut out = HashSet::new();
    match variant {
        0 => {}
        1 => {
            if let Some(last) = rejected.last() {
                // First-listed wins a tie (the plan lists moves in chain order).
                let mut biggest: Option<&PlanMove> = None;
                for m in &last.moves {
                    if biggest.is_none_or(|b| cap_units(m).abs() > cap_units(b).abs()) {
                        biggest = Some(m);
                    }
                }
                if let Some(m) = biggest {
                    out.insert(key_of(&m.node_id, &m.param));
                }
            }
        }
        2 => {
            for t in rejected {
                for m in &t.moves {
                    out.insert(key_of(&m.node_id, &m.param));
                }
            }
        }
        _ => {}
    }
    out
}

/// A move in units of its control's full cap (knob 0.35 / EQ 6 dB).
fn cap_units(m: &PlanMove) -> f64 {
    let cap = match m.unit {
        doctor_plan::ControlUnit::Knob => 0.35,
        doctor_plan::ControlUnit::Db => 6.0,
    };
    (m.to - m.from) / cap
}

/// Move-cap multiplier per rejection streak: full for the first two variants
/// (they change WHICH controls move), half from the third (same controls,
/// smaller step).
pub fn cap_for_variant(variant: u32) -> f64 {
    if variant >= 3 {
        0.5
    } else {
        1.0
    }
}

/// Rejection streaks beyond this get no new proposal ("this is as far as the
/// knobs go") — four distinct attempts from one baseline is the honest limit.
pub const MAX_VARIANTS: u32 = 4;

/// Cumulative ops = the baseline's ops with `moves` laid over (a later value
/// for the same `(group, node, param)` replaces the earlier one). Every op is
/// an absolute `Param` write relative to the SAVED preset, so replaying the
/// list from a restored preset reproduces the candidate exactly.
pub fn merge_ops(baseline: &[DoctorOp], moves: &[PlanMove]) -> Vec<DoctorOp> {
    let mut out: Vec<DoctorOp> = baseline.to_vec();
    for m in moves {
        let same = |op: &DoctorOp| {
            matches!(op, DoctorOp::Param { group_id, node_id, param, .. }
                if *group_id == m.group_id && *node_id == m.node_id && *param == m.param)
        };
        out.retain(|op| !same(op));
        out.push(DoctorOp::Param {
            group_id: m.group_id.clone(),
            node_id: m.node_id.clone(),
            param: m.param.clone(),
            value: m.to,
        });
    }
    out
}

/// The chain as the candidate's ops leave it: `Param` ops update the node's
/// current value (so the next discovery starts from the baseline, not the
/// saved preset); `InsertNode` ops append a minimal node.
pub fn nodes_with_ops(nodes: &[DoctorNode], ops: &[DoctorOp]) -> Vec<DoctorNode> {
    let mut out = nodes.to_vec();
    for op in ops {
        match op {
            DoctorOp::Param {
                group_id,
                node_id,
                param,
                value,
            } => {
                for n in out
                    .iter_mut()
                    .filter(|n| n.group_id == *group_id && n.node_id == *node_id)
                {
                    n.params.insert(param.clone(), *value);
                }
            }
            DoctorOp::InsertNode {
                group_id,
                fender_id,
                params,
                ..
            } => out.push(DoctorNode {
                group_id: group_id.clone(),
                node_id: fender_id.clone(),
                model: fender_id.clone(),
                bypassed: false,
                cab_sim_id: None,
                cab_sim2_enabled: None,
                params: params.iter().cloned().collect(),
            }),
        }
    }
    out
}

/// What one round's proposal rests on.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProposalNote {
    /// Controls whose response the loop has re-scaled from measurements:
    /// `(label "Block · Knob", scale)`.
    pub learned: Vec<(String, f64)>,
    /// Controls excluded for this variant (player rejections).
    pub excluded: Vec<String>,
    /// Move-cap multiplier in force.
    pub cap: f64,
}

/// Propose the next round from `baseline_profile` (the accepted state's
/// capture) on `nodes` (the chain with the baseline's ops applied):
/// calibrated responses from every trial so far, the rejection streak's
/// exclusions/cap, the plan solver. `None` = nothing left to propose.
pub struct Baseline<'a> {
    /// The accepted state's capture.
    pub profile: &'a SoundProfile,
    /// The chain with the baseline's ops applied.
    pub nodes: &'a [DoctorNode],
    pub family: Family,
    pub kind: StimulusKind,
    pub coverage: Option<&'a [bool]>,
    pub diags: &'a [LeveledDiag],
}

/// The search memory the proposal draws on.
pub struct History<'a> {
    /// Every measured round so far (accepted or not) — calibrates the model.
    pub trials: &'a [Trial],
    /// The rejected rounds since the last acceptance, oldest first.
    pub rejected_since_accept: &'a [&'a Trial],
    /// The rejection streak = which variant to propose.
    pub variant: u32,
}

/// Propose the next round from the baseline: calibrated responses from every
/// trial so far, the rejection streak's exclusions/cap, the plan solver.
/// `None` = nothing left to propose.
pub fn propose(base: &Baseline<'_>, hist: &History<'_>) -> Option<(TonePlan, ProposalNote)> {
    let (nodes, family, kind, coverage, diags) = (
        base.nodes,
        base.family,
        base.kind,
        base.coverage,
        base.diags,
    );
    let (trials, rejected_since_accept, variant) =
        (hist.trials, hist.rejected_since_accept, hist.variant);
    if variant >= MAX_VARIANTS {
        return None;
    }
    let mut controls = doctor_plan::discover_controls(nodes, family);
    if controls.is_empty() {
        return None;
    }
    let scales = calibrate(&controls, trials);
    apply_scales(&mut controls, &scales);
    let excluded = exclusions(rejected_since_accept, variant);
    let cap = cap_for_variant(variant);
    let learned: Vec<(String, f64)> = scales
        .iter()
        .filter(|(_, s)| (s - 1.0).abs() > 0.05)
        .filter_map(|(k, s)| {
            controls
                .iter()
                .find(|c| key_of(&c.node_id, &c.param) == *k)
                .map(|c| (format!("{} · {}", c.block_name, c.label), *s))
        })
        .collect();
    let excluded_labels: Vec<String> = controls
        .iter()
        .filter(|c| excluded.contains(&key_of(&c.node_id, &c.param)))
        .map(|c| format!("{} · {}", c.block_name, c.label))
        .collect();
    let mut controls: Vec<Control> = controls
        .into_iter()
        .filter(|c| !excluded.contains(&key_of(&c.node_id, &c.param)))
        .collect();
    for c in controls.iter_mut() {
        c.cap = cap;
    }
    let plan = doctor_plan::generate_plan_with(
        base.profile,
        nodes,
        family,
        kind,
        coverage,
        diags,
        controls,
    )?;
    Some((
        plan,
        ProposalNote {
            learned,
            excluded: excluded_labels,
            cap,
        },
    ))
}

/// Tonal finding keys of a diagnosis, for the round-over-round comparison.
pub fn tonal_keys(diags: &[LeveledDiag]) -> Vec<String> {
    diags
        .iter()
        .map(|d| d.diag.key)
        .filter(|k| doctor_plan::is_tonal(k))
        .map(str::to_string)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::doctor_plan::ControlUnit;

    fn ctl(node: &str, param: &str, response: Vec<f64>) -> Control {
        Control {
            group_id: "G1".into(),
            node_id: node.into(),
            model: "ACD_TwinReverb65NoFx".into(),
            block_name: "'65 Twin Reverb".into(),
            param: param.into(),
            label: param.to_uppercase(),
            unit: ControlUnit::Knob,
            current: 0.5,
            lo: 0.0,
            hi: 1.0,
            response,
            cap: 1.0,
        }
    }

    fn mv(node: &str, param: &str, from: f64, to: f64) -> PlanMove {
        PlanMove {
            group_id: "G1".into(),
            node_id: node.into(),
            model: "ACD_TwinReverb65NoFx".into(),
            block_name: "'65 Twin Reverb".into(),
            param: param.into(),
            control_label: param.to_uppercase(),
            unit: ControlUnit::Knob,
            from,
            to,
            from_label: format!("{:.1}", from * 10.0),
            to_label: format!("{:.1}", to * 10.0),
        }
    }

    fn trial(moves: Vec<PlanMove>, delta: Vec<f64>, accepted: bool) -> Trial {
        Trial {
            ops: Vec::new(),
            moves,
            balance_db: vec![0.0; delta.len()],
            delta_db: delta,
            accepted,
        }
    }

    #[test]
    fn calibrate_scales_a_control_by_what_the_device_did() {
        // Nominal bass: 16 dB at lows per unit travel; the device showed half of it.
        let bass = ctl("amp", "bass", vec![16.0, 10.0, 3.0, 1.0, 0.0, 0.0]);
        let m = bass.response.iter().sum::<f64>() / 6.0;
        let centered: Vec<f64> = bass.response.iter().map(|r| r - m).collect();
        let dx = -0.3;
        let observed: Vec<f64> = centered.iter().map(|r| 0.5 * r * dx).collect();
        // One round, full-cap move: pulls partway toward 0.5; two rounds most of the way.
        let one = calibrate(
            std::slice::from_ref(&bass),
            &[trial(
                vec![mv("amp", "bass", 0.5, 0.2)],
                observed.clone(),
                true,
            )],
        );
        let s1 = one[0].1;
        assert!(s1 < 0.9 && s1 > 0.5, "one round pulls toward 0.5: {s1}");
        let two = calibrate(
            std::slice::from_ref(&bass),
            &[
                trial(vec![mv("amp", "bass", 0.5, 0.2)], observed.clone(), false),
                trial(vec![mv("amp", "bass", 0.5, 0.2)], observed, true),
            ],
        );
        assert!(two[0].1 < s1, "more evidence, closer to 0.5: {}", two[0].1);
        // An unmoved control is not in the result (keeps 1.0).
        let treb = ctl("amp", "treb", vec![0.0, 0.0, 3.0, 9.0, 15.0, 16.0]);
        let r = calibrate(
            &[bass, treb],
            &[trial(vec![mv("amp", "bass", 0.5, 0.2)], vec![0.0; 6], true)],
        );
        assert_eq!(r.len(), 1);
        assert!(r[0].0.starts_with("amp"));
    }

    #[test]
    fn calibrate_clamps_and_survives_a_flat_measurement() {
        let bass = ctl("amp", "bass", vec![16.0, 10.0, 3.0, 1.0, 0.0, 0.0]);
        // The device did nothing → scale heads to 0, clamped at the floor.
        let r = calibrate(
            std::slice::from_ref(&bass),
            &(0..6)
                .map(|_| trial(vec![mv("amp", "bass", 0.5, 0.15)], vec![0.0; 6], false))
                .collect::<Vec<_>>(),
        );
        assert!((r[0].1 - SCALE_MIN).abs() < 0.2, "{r:?}");
        // No trials → nothing learned.
        assert!(calibrate(&[bass], &[]).is_empty());
    }

    #[test]
    fn apply_scales_multiplies_only_the_named_control() {
        let mut cs = vec![
            ctl("amp", "bass", vec![16.0, 10.0, 3.0, 1.0, 0.0, 0.0]),
            ctl("amp", "treb", vec![0.0, 0.0, 3.0, 9.0, 15.0, 16.0]),
        ];
        apply_scales(&mut cs, &[(key_of("amp", "bass"), 0.5)]);
        assert_eq!(cs[0].response[0], 8.0);
        assert_eq!(cs[1].response[5], 16.0);
    }

    #[test]
    fn exclusions_follow_the_rejection_streak() {
        let t1 = trial(
            vec![mv("amp", "bass", 0.5, 0.2), mv("amp", "treb", 0.5, 0.55)],
            vec![0.0; 6],
            false,
        );
        let t2 = trial(vec![mv("ts", "tone", 0.5, 0.7)], vec![0.0; 6], false);
        assert!(exclusions(&[&t1], 0).is_empty());
        let v1 = exclusions(&[&t1], 1);
        assert_eq!(v1.len(), 1);
        assert!(
            v1.contains(&key_of("amp", "bass")),
            "the biggest move: {v1:?}"
        );
        let v2 = exclusions(&[&t1, &t2], 2);
        assert_eq!(v2.len(), 3);
        assert!(exclusions(&[&t1, &t2], 3).is_empty());
        assert_eq!(cap_for_variant(0), 1.0);
        assert_eq!(cap_for_variant(3), 0.5);
    }

    #[test]
    fn merge_ops_replaces_same_target_and_keeps_the_rest() {
        let base = vec![
            DoctorOp::Param {
                group_id: "G1".into(),
                node_id: "amp".into(),
                param: "bass".into(),
                value: 0.4,
            },
            DoctorOp::Param {
                group_id: "G1".into(),
                node_id: "amp".into(),
                param: "treb".into(),
                value: 0.6,
            },
        ];
        let merged = merge_ops(
            &base,
            &[mv("amp", "bass", 0.4, 0.3), mv("ts", "tone", 0.5, 0.6)],
        );
        let vals: Vec<(String, f64)> = merged
            .iter()
            .map(|op| match op {
                DoctorOp::Param {
                    node_id,
                    param,
                    value,
                    ..
                } => (format!("{node_id}.{param}"), *value),
                _ => unreachable!(),
            })
            .collect();
        assert_eq!(
            vals,
            vec![
                ("amp.treb".to_string(), 0.6),
                ("amp.bass".to_string(), 0.3),
                ("ts.tone".to_string(), 0.6)
            ]
        );
    }

    #[test]
    fn nodes_with_ops_updates_values_and_appends_inserts() {
        let nodes = vec![DoctorNode {
            group_id: "G1".into(),
            node_id: "amp".into(),
            model: "ACD_TwinReverb65NoFx".into(),
            bypassed: false,
            cab_sim_id: None,
            cab_sim2_enabled: None,
            params: [("bass".to_string(), 0.6)].into_iter().collect(),
        }];
        let ops = vec![
            DoctorOp::Param {
                group_id: "G1".into(),
                node_id: "amp".into(),
                param: "bass".into(),
                value: 0.3,
            },
            DoctorOp::InsertNode {
                group_id: "G3".into(),
                before_fender_id: None,
                fender_id: "ACD_TenBandEQStereo".into(),
                params: vec![("gain250hz".into(), -3.0)],
            },
        ];
        let out = nodes_with_ops(&nodes, &ops);
        assert_eq!(out[0].params["bass"], 0.3);
        assert_eq!(out[1].node_id, "ACD_TenBandEQStereo");
        assert_eq!(out[1].params["gain250hz"], -3.0);
    }

    #[test]
    fn propose_excludes_rejected_controls_and_gives_up_past_the_variant_limit() {
        let target = crate::doctor::target_curve(Family::Guitar);
        let delta = [0.0, 7.0, 0.0, 0.0, 0.0, 0.0];
        let profile = SoundProfile {
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
        };
        let nodes = vec![DoctorNode {
            group_id: "G1".into(),
            node_id: "amp".into(),
            model: "ACD_TwinReverb65NoFx".into(),
            bypassed: false,
            cab_sim_id: None,
            cab_sim2_enabled: None,
            params: [("bass", 0.6), ("mid", 0.5), ("treb", 0.5)]
                .into_iter()
                .map(|(k, v)| (k.to_string(), v))
                .collect(),
        }];
        let diags = crate::doctor::diagnose_levels(
            &profile,
            Some(&nodes),
            Family::Guitar,
            StimulusKind::Synthetic,
            None,
        );
        let base = Baseline {
            profile: &profile,
            nodes: &nodes,
            family: Family::Guitar,
            kind: StimulusKind::Synthetic,
            coverage: None,
            diags: &diags,
        };
        let (p0, note0) = propose(
            &base,
            &History {
                trials: &[],
                rejected_since_accept: &[],
                variant: 0,
            },
        )
        .expect("round 0 proposes");
        assert!(p0.moves.iter().any(|m| m.param == "bass"));
        assert!(note0.excluded.is_empty() && note0.learned.is_empty());
        // The player rejects it → variant 1 excludes its biggest control.
        let rejected = Trial {
            moves: p0.moves.clone(),
            ops: p0.rx.ops.clone(),
            balance_db: vec![0.0; 6],
            delta_db: vec![0.0; 6],
            accepted: false,
        };
        let trials = [rejected.clone()];
        let r1 = propose(
            &base,
            &History {
                trials: &trials,
                rejected_since_accept: &[&rejected],
                variant: 1,
            },
        );
        let excluded = exclusions(&[&rejected], 1);
        assert_eq!(excluded.len(), 1);
        let (p1, note1) = r1.expect("a variant without the excluded control still exists");
        assert!(
            p1.moves
                .iter()
                .all(|m| !excluded.contains(&key_of(&m.node_id, &m.param))),
            "{p1:?}"
        );
        assert_eq!(note1.excluded.len(), 1);
        assert!(note1.excluded[0].starts_with("'65 Twin Reverb · "));
        assert!(propose(
            &base,
            &History {
                trials: &[],
                rejected_since_accept: &[],
                variant: MAX_VARIANTS,
            },
        )
        .is_none());
    }
}
