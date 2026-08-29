//! `probe --doctor-knob-sweep` — HARDWARE calibration arm for the balance
//! plan's NOMINAL tone-control response model (`doctor_plan::NOMINAL`).
//!
//! For every drivable amp/pedal tone control the probed preset carries
//! (`doctor_plan::discover_controls` — the same discovery the plan uses),
//! captures the sound with that ONE knob nudged `±delta` from its saved value
//! (one side only when the knob sits at an edge), everything else untouched,
//! and reports the MEASURED per-band dB per full knob travel next to the
//! nominal table's prediction. Graphic/parametric bands are skipped (their
//! response is the filter's own dB, not a nominal shape) unless `--eq` asks
//! for them too. Never saves — every step writes the live edit buffer and the
//! stored preset is reloaded between knobs and at the end (and on any error).
//!
//! Usage:
//! ```text
//! probe --doctor-knob-sweep <slot> [--delta 0.25] [--eq] [--out <report.json>]
//! ```
//! Attended; loads the probed slot; ends re-amp OFF. ~3 connections per
//! capture step (restore → write → capture), so a 4-knob amp takes ~2 min.
//! The JSON report carries, per control, `nominal` and `measured` (dB per
//! unit, family band layout) plus the per-band ratio — the evidence a
//! per-amp-family table would be re-derived from (`notes/doctor-calibration.md`).

use crate::doctor;
use crate::doctor_plan::{self, Control, ControlUnit};
use crate::leveller;

use super::doctor_inject::measure;
use super::stimulus::{probe_stimulus_path, read_stimulus_48k};

/// One control's sweep rows — see the module doc.
#[derive(Debug, serde::Serialize)]
struct KnobRow {
    block: String,
    model: String,
    node_id: String,
    param: String,
    label: String,
    unit: ControlUnit,
    saved: f64,
    /// The two (or one) values actually captured at.
    points: Vec<f64>,
    /// Raw per-band `band_db` at each point, family layout.
    band_db: Vec<Vec<f64>>,
    /// Measured dB per unit of the control (finite difference across `points`).
    measured: Vec<f64>,
    nominal: Vec<f64>,
    /// `measured / nominal` per band (`null` where nominal ≈ 0).
    ratio: Vec<Option<f64>>,
}

#[derive(Debug, serde::Serialize)]
struct Report {
    slot: u32,
    preset: String,
    model: &'static str,
    delta: f64,
    band_labels: Vec<String>,
    baseline_band_db: Vec<f64>,
    controls: Vec<KnobRow>,
}

/// The two capture points for a control: `saved ± delta` clamped into range,
/// collapsing to one side at an edge (a knob at 0 can only go up).
fn sweep_points(c: &Control, delta: f64) -> Vec<f64> {
    let (lo, hi) = match c.unit {
        ControlUnit::Knob => ((c.current - delta).max(c.lo), (c.current + delta).min(c.hi)),
        // EQ bands: the same fraction of their range (±3 dB for 0.25 of ±12).
        ControlUnit::Db => {
            let d = delta * (c.hi - c.lo) / 2.0;
            ((c.current - d).max(c.lo), (c.current + d).min(c.hi))
        }
        // Cut corners move in octaves: ±delta octaves around the current corner.
        ControlUnit::Hz => (
            (c.current / 2f64.powf(delta)).max(c.lo),
            (c.current * 2f64.powf(delta)).min(c.hi),
        ),
    };
    let mut pts = Vec::new();
    if lo < c.current - 1e-9 {
        pts.push(lo);
    }
    if hi > c.current + 1e-9 {
        pts.push(hi);
    }
    pts
}

/// See the module doc.
pub fn probe_doctor_knob_sweep(
    slot: u32,
    delta: f64,
    include_eq: bool,
    out: Option<&str>,
) -> Result<String, String> {
    if !(delta > 0.0 && delta <= 0.5) {
        return Err(format!("--delta must be in (0, 0.5], got {delta}"));
    }
    let stim = leveller::doctor_stim_slice(read_stimulus_48k(&probe_stimulus_path(
        "guitar-humbucker",
    )?)?);
    let (preset, _, _) = crate::read_slot_preset_parsed(slot)?;
    let name = preset
        .pointer("/info/displayName")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default()
        .to_string();
    let nodes: Vec<doctor::DoctorNode> = crate::session::extract_active_graph(&preset, None)
        .nodes
        .iter()
        .map(doctor::DoctorNode::from_graph_node)
        .collect();
    let family = doctor::Family::Guitar;
    let tail_ms = u64::from(doctor::doctor_tail_ms(&nodes));
    let controls: Vec<Control> = doctor_plan::discover_controls(&nodes, family)
        .into_iter()
        .filter(|c| include_eq || c.unit == ControlUnit::Knob)
        .collect();
    if controls.is_empty() {
        return Err(format!(
            "slot {slot} ({name}) has no drivable tone control (amp Bass/Mid/Treble/Presence/Tone, drive-pedal Tone…){}",
            if include_eq { "" } else { " — pass --eq to include EQ bands" }
        ));
    }
    // Fresh-connection re-amp OFF on EVERY exit path from here down.
    let _reamp_off = super::ReampOffGuard;
    let mut out_text = format!(
        "doctor-knob-sweep slot {slot} \"{name}\" delta {delta} — {} control(s), model {}\n",
        controls.len(),
        doctor_plan::NOMINAL
    );

    // Baseline: the stored preset as saved (also loads it).
    let (base, line) = measure(
        &stim,
        "saved",
        leveller::doctor_capture(slot, None, &[], &[], &stim, Some(0.5), tail_ms, false),
        u32::try_from(tail_ms).unwrap_or(u32::MAX),
    )?;
    out_text += &line;
    let baseline = base.band_db.clone();

    let mut rows: Vec<KnobRow> = Vec::new();
    let run = |rows: &mut Vec<KnobRow>, out_text: &mut String| -> Result<(), String> {
        for c in &controls {
            let points = sweep_points(c, delta);
            let mut band_db: Vec<Vec<f64>> = Vec::new();
            for &v in &points {
                std::thread::sleep(std::time::Duration::from_millis(leveller::RECONNECT_GAP_MS));
                let ops = vec![doctor::DoctorOp::Param {
                    group_id: c.group_id.clone(),
                    node_id: c.node_id.clone(),
                    param: c.param.clone(),
                    value: v,
                }];
                let s =
                    crate::commands::doctor::ops_session(slot, &name, None, &ops, "sweep", &[])?;
                drop(s);
                std::thread::sleep(std::time::Duration::from_millis(leveller::RECONNECT_GAP_MS));
                let label = format!("{}={:.2}", c.param, v);
                let (read, line) = measure(
                    &stim,
                    &label,
                    leveller::doctor_capture_current(&stim, None, &[], Some(0.5), tail_ms),
                    u32::try_from(tail_ms).unwrap_or(u32::MAX),
                )?;
                *out_text += &format!("  [{}] {}", c.block_name, line.trim_start());
                band_db.push(read.band_db);
                // Back to the stored preset before the next point/knob so
                // each capture isolates exactly ONE knob move.
                leveller::restore_saved_preset(slot)?;
            }
            // Finite difference across the captured points (two-sided when
            // both exist, else against the saved baseline).
            let (lo_v, lo_db, hi_v, hi_db): (f64, &[f64], f64, &[f64]) = match points.len() {
                2 => (points[0], &band_db[0], points[1], &band_db[1]),
                1 if points[0] < c.current => (points[0], &band_db[0], c.current, &baseline),
                1 => (c.current, &baseline, points[0], &band_db[0]),
                _ => continue,
            };
            let span = hi_v - lo_v;
            let measured: Vec<f64> = hi_db
                .iter()
                .zip(lo_db)
                .map(|(h, l)| (h - l) / span)
                .collect();
            let ratio = measured
                .iter()
                .zip(&c.response)
                .map(|(m, n)| (n.abs() > 0.5).then(|| m / n))
                .collect();
            *out_text += &format!(
                "  {} {} ({}): measured/unit {} | nominal {}\n",
                c.block_name,
                c.label,
                c.param,
                measured
                    .iter()
                    .map(|v| format!("{v:+.1}"))
                    .collect::<Vec<_>>()
                    .join(","),
                c.response
                    .iter()
                    .map(|v| format!("{v:+.1}"))
                    .collect::<Vec<_>>()
                    .join(","),
            );
            rows.push(KnobRow {
                block: c.block_name.clone(),
                model: c.model.clone(),
                node_id: c.node_id.clone(),
                param: c.param.clone(),
                label: c.label.clone(),
                unit: c.unit,
                saved: c.current,
                points,
                band_db,
                measured,
                nominal: c.response.clone(),
                ratio,
            });
        }
        Ok(())
    };
    let result = run(&mut rows, &mut out_text);
    // The edit buffer must never outlive the command — also on a mid-sweep
    // failure; a restore failure is reported, never dropped.
    let restore = leveller::restore_saved_preset(slot);
    match (result, restore) {
        (Ok(()), Ok(())) => {}
        (Ok(()), Err(r)) => return Err(format!("edit-buffer restore failed: {r}")),
        (Err(e), restore) => return Err(leveller::append_restore_err(e, restore)),
    }
    let report = Report {
        slot,
        preset: name,
        model: doctor_plan::NOMINAL,
        delta,
        band_labels: family.labels_owned(),
        baseline_band_db: baseline,
        controls: rows,
    };
    if let Some(path) = out {
        let json = serde_json::to_string_pretty(&report).map_err(|e| e.to_string())?;
        std::fs::write(path, json).map_err(|e| format!("write {path}: {e}"))?;
        out_text += &format!("  report → {path}\n");
    }
    out_text += "  (edit buffer discarded — stored preset reloaded)\n";
    Ok(out_text)
}

/// `probe --doctor-plan-dry <slot> [<scene>]` — the balance plan, read-only
/// and NEVER writing/saving: capture the sound (base, or a 0-based wire scene)
/// exactly as the app does (effective scene chain, synthetic guitar-humbucker
/// stimulus), diagnose, discover every drivable tone control with its remedy
/// verdict, and print the proposal — the moves and, when nothing is proposed,
/// WHY (no finding / no lever / the gate refused it). Ends re-amp OFF.
pub fn probe_doctor_plan_dry(
    slot: u32,
    scene: Option<u32>,
    stim_wav: Option<&str>,
) -> Result<String, String> {
    use crate::doctor_plan;
    // A captured DI (a profile's Tier-2 wav) is injected VERBATIM and diagnosed
    // in CAPTURE space — the same seam the app uses when a profile is picked;
    // else the synthetic humbucker sample. This is exactly what changes a
    // scene's balance/diagnoses between "dry synthetic" and "what the player
    // saw", so the arm must be able to reproduce both.
    let kind = if stim_wav.is_some() {
        doctor::StimulusKind::Capture
    } else {
        doctor::StimulusKind::Synthetic
    };
    let stim = leveller::doctor_stim_slice(read_stimulus_48k(&stim_wav.map_or_else(
        || probe_stimulus_path("guitar-humbucker"),
        |w| Ok(w.to_string()),
    )?)?);
    let (preset, _, _) = crate::read_slot_preset_parsed(slot)?;
    let name = preset
        .pointer("/info/displayName")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default()
        .to_string();
    let base_nodes: Vec<doctor::DoctorNode> = crate::session::extract_active_graph(&preset, None)
        .nodes
        .iter()
        .map(doctor::DoctorNode::from_graph_node)
        .collect();
    let overlays = crate::session::extract_scene_overlays(&preset);
    let overlay = scene
        .and_then(|s| overlays.get(s as usize).cloned())
        .unwrap_or_default();
    let nodes = doctor::effective_nodes(&base_nodes, &overlay, &[]);
    let family = doctor::Family::Guitar;
    let tail_ms = u64::from(doctor::doctor_tail_ms(&nodes));
    let _reamp_off = super::ReampOffGuard;

    let mut out = format!("doctor-plan-dry slot {slot} \"{name}\" scene {scene:?}\n");
    out += &format!(
        "  effective chain ({} active node(s)):\n",
        nodes.iter().filter(|n| !n.bypassed).count()
    );
    for n in &nodes {
        out += &format!(
            "    {} {} {}{}\n",
            n.group_id,
            n.node_id,
            if n.bypassed { "[bypassed] " } else { "" },
            if n.params.is_empty() {
                String::new()
            } else {
                format!("params={:?}", n.params)
            }
        );
    }

    let (samples, rate, loudness) = leveller::doctor_capture_with_loudness(
        slot,
        scene,
        &[],
        &[],
        &stim,
        Some(0.5),
        tail_ms,
        false,
        None,
    )?;
    let (profile, coverage, balance) = crate::commands::doctor::analyze_doctor_capture(
        &samples,
        rate,
        loudness,
        &stim,
        u32::try_from(tail_ms).unwrap_or(u32::MAX),
        family,
        &name,
    )?;
    out += &format!(
        "  balance dB {}\n  coverage {:?}\n",
        balance
            .iter()
            .map(|v| format!("{v:+.1}"))
            .collect::<Vec<_>>()
            .join(","),
        coverage
    );
    let diags = doctor::diagnose_levels(
        &profile,
        Some(&nodes),
        family,
        doctor::StimulusKind::Synthetic,
        Some(&coverage),
    );
    out += &format!(
        "  diagnoses: {}\n",
        if diags.is_empty() {
            "(none)".into()
        } else {
            diags
                .iter()
                .map(|d| {
                    format!(
                        "{}({:?},sev {:.2})",
                        d.diag.key, d.from_level, d.diag.severity
                    )
                })
                .collect::<Vec<_>>()
                .join(" ")
        }
    );

    let controls = doctor_plan::discover_controls(&nodes, family);
    out += &format!("  drivable controls ({}):\n", controls.len());
    for c in &controls {
        out += &format!(
            "    {} · {} ({}) cur {:.3} range [{:.2},{:.2}] resp {}\n",
            c.block_name,
            c.label,
            c.param,
            c.current,
            c.lo,
            c.hi,
            c.response
                .iter()
                .map(|v| format!("{v:+.1}"))
                .collect::<Vec<_>>()
                .join(",")
        );
    }

    let bdb = doctor::band_db(&profile.bands);
    let dev = doctor::anchor_deviations(
        doctor::deviations(&bdb, family),
        profile.stim_bands.as_deref(),
        family,
    );
    out += &format!(
        "  anchored deviation {}\n  balance-error MAX {:.1} dB (tol ±{:.0})\n",
        dev.iter()
            .map(|v| format!("{v:+.1}"))
            .collect::<Vec<_>>()
            .join(","),
        doctor_plan::balance_error_db(&dev, family, doctor_plan::BALANCE_TOL_DB, Some(&coverage)),
        doctor_plan::BALANCE_TOL_DB,
    );

    out += &doctor_plan::dry_solve_report(&profile, &nodes, family, Some(&coverage), &diags);

    match doctor_plan::generate_plan_with(
        &profile,
        &nodes,
        family,
        kind,
        Some(&coverage),
        &diags,
        controls,
    ) {
        Some(plan) => {
            out += &format!(
                "  PROPOSAL: {}\n    balance error {:.1} → {:.1} dB  clears {:?}  remains {}\n",
                plan.rx.detail,
                plan.balance_error_before_db,
                plan.balance_error_after_db,
                plan.clears,
                plan.remains.len(),
            );
        }
        None => out += "  PROPOSAL: none (no tonal finding, no drivable lever, or no move clears/eases a finding without introducing one)\n",
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctl(current: f64, unit: ControlUnit, lo: f64, hi: f64) -> Control {
        Control {
            group_id: "G1".into(),
            node_id: "n".into(),
            model: "m".into(),
            block_name: "b".into(),
            param: "p".into(),
            label: "P".into(),
            unit,
            current,
            lo,
            hi,
            response: vec![0.0; 6],
            cap: 1.0,
            remedy: crate::doctor_plan::RemedyState::Free,
        }
    }

    #[test]
    fn sweep_points_are_two_sided_inside_and_one_sided_at_an_edge() {
        let mid = ctl(0.5, ControlUnit::Knob, 0.0, 1.0);
        assert_eq!(sweep_points(&mid, 0.25), vec![0.25, 0.75]);
        let floor = ctl(0.0, ControlUnit::Knob, 0.0, 1.0);
        assert_eq!(sweep_points(&floor, 0.25), vec![0.25]);
        let near_top = ctl(0.9, ControlUnit::Knob, 0.0, 1.0);
        assert_eq!(sweep_points(&near_top, 0.25), vec![0.65, 1.0]);
        // EQ: the same fraction of the ±12 dB range → ±3 dB.
        let eq = ctl(0.0, ControlUnit::Db, -12.0, 12.0);
        assert_eq!(sweep_points(&eq, 0.25), vec![-3.0, 3.0]);
    }
}
