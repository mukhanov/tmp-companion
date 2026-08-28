//! Doctor tune loop commands — the device side of `doctor_tune` (the pure
//! search): one sound's closed-loop balance session, driven round by round
//! from the Results page.
//!
//! `doctor_tune_step` takes the player's decision and runs ONE round:
//! `start` captures the SAVED sound as the baseline (after the lazy-commit
//! barrier), then proposes → applies live (cumulative ops, never saved) →
//! captures → diagnoses the candidate; `better` promotes the last candidate to
//! the baseline and runs the next round from it; `worse` rejects it and runs a
//! different variant (`doctor_tune::exclusions` / cap). The session (baseline +
//! candidate profiles, clips, the trial history that calibrates the model) is
//! kept in ONE process-wide slot keyed on the sound + stimulus — a step for a
//! different sound starts over, `doctor_tune_end` clears it (restoring the
//! stored preset when `discard`). Persisting is `doctor_save` with the step's
//! cumulative `ops` (the frontend hands them back), which also registers the
//! commit witness like every other Doctor save.
//!
//! Every round leaves the device with the candidate in its UNSAVED edit buffer
//! and re-amp OFF (`reamp_off_guaranteed`); a round's capture runs on the same
//! session that wrote the ops (`doctor_capture_on_session`), exactly the
//! `doctor_apply` shape.
use crate::*;

use super::doctor::{
    analyze_doctor_capture, doctor_fresh_load, family_of_topology, ops_session,
    resolve_sound_isolation, ApplyMeasure, DoctorApplyJob, SoundIsolation,
};
use crate::doctor_tune::{self, ProposalNote, Trial};

/// The player's decision that starts the next round.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TuneDecision {
    /// Capture the saved sound as the baseline and run round 1.
    Start,
    /// The last candidate sounds better: it becomes the baseline; next round.
    Better,
    /// The last candidate does not: reject it; propose a different variant.
    Worse,
}

#[derive(Debug, Clone, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DoctorTuneJob {
    /// The diagnosed sound's context — the same fields `doctor_apply` takes
    /// (its `ops` are ignored here; the loop owns the ops).
    pub ctx: DoctorApplyJob,
    pub decision: TuneDecision,
}

/// One measured state of the sound inside a session.
#[derive(Debug, Clone)]
struct Round {
    ops: Vec<doctor::DoctorOp>,
    moves: Vec<doctor_plan::PlanMove>,
    profile: doctor::SoundProfile,
    coverage: Vec<bool>,
    balance: Vec<f64>,
    lufs: f64,
    clip: String,
    diags: Vec<doctor::LeveledDiag>,
}

struct TuneSession {
    key: (u32, String, Option<u32>, Option<u32>, String, Option<u32>),
    family: doctor::Family,
    kind: doctor::StimulusKind,
    stim: Vec<f32>,
    iso: SoundIsolation,
    tail_ms: u64,
    nodes: Vec<doctor::DoctorNode>,
    baseline: Round,
    last: Option<Round>,
    trials: Vec<Trial>,
    rejected_streak: u32,
    round: u32,
}

static SESSION: std::sync::Mutex<Option<TuneSession>> = std::sync::Mutex::new(None);

/// Where the loop stands after a step.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum TuneStatus {
    /// A candidate is applied (unsaved) and measured — the player decides.
    Candidate,
    /// The baseline has no tonal finding left — nothing to improve.
    Converged,
    /// No further variant from this baseline (rejections exhausted, or no
    /// drivable control) — keep the baseline, or stop.
    Exhausted,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DoctorTuneStep {
    pub round: u32,
    pub status: TuneStatus,
    /// The round's proposal (moves from the baseline), when `status == Candidate`.
    pub candidate: Option<doctor_plan::TonePlan>,
    pub note: Option<ProposalNote>,
    /// Cumulative ops of the CANDIDATE (what `doctor_save` must persist to keep
    /// it) — the baseline's when there is no candidate.
    pub ops: Vec<doctor::DoctorOp>,
    /// Cumulative ops of the baseline (what the device holds after `worse` /
    /// what `doctor_save` persists to keep the last accepted round).
    pub baseline_ops: Vec<doctor::DoctorOp>,
    pub baseline_clip: String,
    pub candidate_clip: Option<String>,
    /// Baseline vs candidate, measured.
    pub measured: Option<ApplyMeasure>,
    pub baseline_diags: Vec<doctor::LeveledDiag>,
    pub candidate_diags: Vec<doctor::LeveledDiag>,
    pub cleared: Vec<String>,
    pub remained: Vec<String>,
    pub introduced: Vec<String>,
    pub band_labels: Vec<String>,
    pub baseline_balance_db: Vec<f64>,
    pub candidate_balance_db: Option<Vec<f64>>,
    /// One honest sentence for the card.
    pub message: String,
}

fn session_key(
    job: &DoctorApplyJob,
    stim_path: &str,
    cal: Option<f32>,
) -> (u32, String, Option<u32>, Option<u32>, String, Option<u32>) {
    (
        job.list_index,
        job.name.clone(),
        job.scene,
        job.footswitch,
        stim_path.to_string(),
        cal.map(f32::to_bits),
    )
}

/// The per-session capture context every round shares.
struct CaptureCtx<'a> {
    job: &'a DoctorApplyJob,
    stim: &'a [f32],
    iso: &'a SoundIsolation,
    tail_ms: u64,
    family: doctor::Family,
    kind: doctor::StimulusKind,
}

/// Capture + analyze one state of the sound. `s = None`: capture the STORED
/// preset (loads the slot — the baseline); `Some`: capture the live edit buffer
/// on that session (the candidate, on the session that just wrote it). `nodes`
/// is the chain as the state's `ops` leave it (the diagnosis's rx target).
fn measure_round(
    cx: &CaptureCtx<'_>,
    nodes: &[doctor::DoctorNode],
    ops: Vec<doctor::DoctorOp>,
    moves: Vec<doctor_plan::PlanMove>,
    s: Option<&mut Session>,
) -> Result<Round, String> {
    let (samples, rate, loudness) = match s {
        None => leveller::doctor_capture_with_loudness(
            cx.job.list_index,
            cx.job.scene,
            &cx.iso.bypass,
            &cx.iso.params,
            cx.stim,
            None,
            cx.tail_ms,
            false,
            None,
        )?,
        Some(s) => leveller::doctor_capture_on_session_with_loudness(
            s,
            &cx.iso.bypass,
            &cx.iso.params,
            cx.stim,
            None,
            cx.tail_ms,
        )?,
    };
    let (profile, coverage, balance) = analyze_doctor_capture(
        &samples,
        rate,
        loudness,
        cx.stim,
        u32::try_from(cx.tail_ms).unwrap_or(u32::MAX),
        cx.family,
        &cx.job.name,
    )?;
    let clip = format!(
        "data:audio/wav;base64,{}",
        base64_encode(&wav_bytes(&samples, rate)?)
    );
    let diags = doctor::diagnose_levels(&profile, Some(nodes), cx.family, cx.kind, Some(&coverage));
    Ok(Round {
        ops,
        moves,
        lufs: profile.integrated_lufs,
        profile,
        coverage,
        balance,
        clip,
        diags,
    })
}

fn step_result(
    sess: &TuneSession,
    status: TuneStatus,
    note: Option<ProposalNote>,
    message: String,
) -> DoctorTuneStep {
    let base = &sess.baseline;
    let base_keys = doctor_tune::tonal_keys(&base.diags);
    let (
        candidate,
        candidate_clip,
        measured,
        candidate_diags,
        cleared,
        remained,
        introduced,
        ops,
        cand_balance,
    ) = match (&sess.last, status) {
        (Some(c), TuneStatus::Candidate) => {
            let cand_keys = doctor_tune::tonal_keys(&c.diags);
            let plan = doctor_plan::TonePlan {
                moves: c.moves.clone(),
                before_db: doctor::centered_deviations(
                    &doctor::deviations(&doctor::band_db(&base.profile.bands), sess.family),
                    sess.family,
                ),
                predicted_db: doctor::centered_deviations(
                    &doctor::deviations(&doctor::band_db(&c.profile.bands), sess.family),
                    sess.family,
                ),
                band_labels: sess.family.labels_owned(),
                clears: base_keys
                    .iter()
                    .filter(|k| !cand_keys.contains(k))
                    .cloned()
                    .collect(),
                remains: c
                    .diags
                    .iter()
                    .filter(|d| base_keys.iter().any(|k| k == d.diag.key))
                    .map(|d| doctor::LeveledKey {
                        key: d.diag.key.to_string(),
                        from_level: d.from_level,
                    })
                    .collect(),
                loudness_delta_db: c.lufs - base.lufs,
                balance_error_before_db: balance_error(&base.profile, &base.coverage, sess.family),
                balance_error_after_db: balance_error(&c.profile, &c.coverage, sess.family),
                rx: doctor::Rx {
                    kind: doctor::RxKind::OneClick,
                    title: format!("Round {}", sess.round),
                    detail: moves_line(&c.moves),
                    cpu_note: "no CPU change".to_string(),
                    ops: c.ops.clone(),
                    chain: None,
                },
                model: doctor_plan::NOMINAL,
            };
            let measured = ApplyMeasure {
                band_labels: sess.family.labels_owned(),
                delta_db: c
                    .balance
                    .iter()
                    .zip(&base.balance)
                    .map(|(a, b)| a - b)
                    .collect(),
                before_balance_db: base.balance.clone(),
                after_balance_db: c.balance.clone(),
                loudness_delta_db: c.lufs - base.lufs,
            };
            (
                Some(plan),
                Some(c.clip.clone()),
                Some(measured),
                c.diags.clone(),
                base_keys
                    .iter()
                    .filter(|k| !cand_keys.contains(k))
                    .cloned()
                    .collect::<Vec<_>>(),
                base_keys
                    .iter()
                    .filter(|k| cand_keys.contains(k))
                    .cloned()
                    .collect::<Vec<_>>(),
                cand_keys
                    .iter()
                    .filter(|k| !base_keys.contains(k))
                    .cloned()
                    .collect::<Vec<_>>(),
                c.ops.clone(),
                Some(c.balance.clone()),
            )
        }
        _ => (
            None,
            None,
            None,
            Vec::new(),
            Vec::new(),
            base_keys.clone(),
            Vec::new(),
            base.ops.clone(),
            None,
        ),
    };
    DoctorTuneStep {
        round: sess.round,
        status,
        candidate,
        note,
        ops,
        baseline_ops: base.ops.clone(),
        baseline_clip: base.clip.clone(),
        candidate_clip,
        measured,
        baseline_diags: base.diags.clone(),
        candidate_diags,
        cleared,
        remained,
        introduced,
        band_labels: sess.family.labels_owned(),
        baseline_balance_db: base.balance.clone(),
        candidate_balance_db: cand_balance,
        message,
    }
}

/// A round's anchored per-band deviation from the authored target.
fn round_dev(profile: &doctor::SoundProfile, family: doctor::Family) -> Vec<f64> {
    let bdb = doctor::band_db(&profile.bands);
    doctor::anchor_deviations(
        doctor::deviations(&bdb, family),
        profile.stim_bands.as_deref(),
        family,
    )
}

/// A round's MAX distance to the authored balance (`doctor_plan::balance_error_db`)
/// — reported on the wire for the plan card, but NOT the loop's decision metric
/// (an undrivable dominant band keeps it high on an intentionally-voiced preset).
fn balance_error(profile: &doctor::SoundProfile, coverage: &[bool], family: doctor::Family) -> f64 {
    doctor_plan::balance_error_db(
        &round_dev(profile, family),
        family,
        doctor_plan::POLISH_TOL_DB,
        Some(coverage),
    )
}

/// A round's polish OBJECTIVE energy (`doctor_plan::polish_energy`) — the value
/// the loop's "keep going / done" decision reads (it falls whenever any band
/// moves toward target, unlike the MAX balance error an undrivable band pins).
fn balance_energy(
    profile: &doctor::SoundProfile,
    coverage: &[bool],
    family: doctor::Family,
) -> f64 {
    doctor_plan::polish_energy(
        &round_dev(profile, family),
        family,
        doctor_plan::POLISH_TOL_DB,
        Some(coverage),
    )
}

/// "'65 Twin Reverb: Bass 6.0 → 4.0, Treble 5.0 → 5.5 · Greenbox 8: Tone 5.0 → 6.0".
fn moves_line(moves: &[doctor_plan::PlanMove]) -> String {
    let mut by_block: Vec<(String, Vec<String>)> = Vec::new();
    for m in moves {
        let line = format!("{} {} → {}", m.control_label, m.from_label, m.to_label);
        match by_block.iter_mut().find(|(name, _)| *name == m.block_name) {
            Some((_, lines)) => lines.push(line),
            None => by_block.push((m.block_name.clone(), vec![line])),
        }
    }
    by_block
        .iter()
        .map(|(name, lines)| format!("{name}: {}", lines.join(", ")))
        .collect::<Vec<_>>()
        .join(" · ")
}

/// Propose from the session's baseline, apply, capture, diagnose → the new
/// `last`. Returns the step (or an early converged/exhausted step).
fn propose_and_measure(
    sess: &mut TuneSession,
    job: &DoctorApplyJob,
) -> Result<DoctorTuneStep, String> {
    let base_keys = doctor_tune::tonal_keys(&sess.baseline.diags);
    let base_energy = balance_energy(&sess.baseline.profile, &sess.baseline.coverage, sess.family);
    // Converged = the coarse rules are quiet AND the spectrum sits inside the
    // polish tolerance (near-zero objective energy). Quiet rules alone are not
    // the end: the gates are ±3–4 dB wide.
    if base_keys.is_empty() && base_energy <= doctor_plan::POLISH_MIN_ENERGY {
        sess.last = None;
        return Ok(step_result(
            sess,
            TuneStatus::Converged,
            None,
            format!(
                "No finding is left and every band sits within ±{:.0} dB of the reference \
                 balance — nothing more to even out.",
                doctor_plan::POLISH_TOL_DB
            ),
        ));
    }
    let nodes_now = doctor_tune::nodes_with_ops(&sess.nodes, &sess.baseline.ops);
    let rejected: Vec<&Trial> = sess
        .trials
        .iter()
        .rev()
        .take_while(|t| !t.accepted)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    let proposal = doctor_tune::propose(
        &doctor_tune::Baseline {
            profile: &sess.baseline.profile,
            nodes: &nodes_now,
            family: sess.family,
            kind: sess.kind,
            coverage: Some(&sess.baseline.coverage),
            diags: &sess.baseline.diags,
        },
        &doctor_tune::History {
            trials: &sess.trials,
            rejected_since_accept: &rejected,
            variant: sess.rejected_streak,
        },
    );
    let Some((plan, note)) = proposal else {
        sess.last = None;
        let why = if sess.rejected_streak >= doctor_tune::MAX_VARIANTS {
            "Every variant from here was turned down — this is as far as these knobs go. Keep what sounds best, or stop.".to_string()
        } else if base_keys.is_empty() {
            // No finding, and no knob move evens the spectrum further — what's
            // left is this preset's own voicing, a matter of taste, not a
            // defect the chain's tone controls can address.
            "No finding is left, and no move on this chain's own knobs evens the spectrum out further — what's left is this preset's voicing (a different cab or amp would be the bigger change). Keep what sounds best, or stop.".to_string()
        } else {
            "The model has nothing left to move on this chain (no drivable control, or no move that helps).".to_string()
        };
        return Ok(step_result(sess, TuneStatus::Exhausted, None, why));
    };
    let ops = doctor_tune::merge_ops(&sess.baseline.ops, &plan.moves);
    sess.round += 1;
    // Apply the CUMULATIVE ops from the stored preset (a restore discards the
    // previous candidate's unsaved edit buffer), then capture on the same session.
    leveller::restore_saved_preset(job.list_index)?;
    crate::settle(std::time::Duration::from_millis(leveller::RECONNECT_GAP_MS));
    let round = {
        let mut s = ops_session(
            job.list_index,
            &job.name,
            job.scene,
            &ops,
            "tune",
            &job.scene_overlay,
        )?;
        let cx = CaptureCtx {
            job,
            stim: &sess.stim,
            iso: &sess.iso,
            tail_ms: sess.tail_ms,
            family: sess.family,
            kind: sess.kind,
        };
        let r = measure_round(
            &cx,
            &doctor_tune::nodes_with_ops(&sess.nodes, &ops),
            ops.clone(),
            plan.moves.clone(),
            Some(&mut s),
        );
        match r {
            Ok(r) => r,
            Err(e) => {
                drop(s);
                return Err(match leveller::restore_saved_preset(job.list_index) {
                    Ok(()) => e,
                    Err(re) => format!(
                        "{e} AND the restore also failed ({re}) — verify the preset on the unit"
                    ),
                });
            }
        }
    };
    sess.trials.push(Trial {
        moves: round.moves.clone(),
        ops: round.ops.clone(),
        balance_db: round.balance.clone(),
        delta_db: round
            .balance
            .iter()
            .zip(&sess.baseline.balance)
            .map(|(a, b)| a - b)
            .collect(),
        accepted: false,
    });
    sess.last = Some(round);
    let learned = if note.learned.is_empty() {
        String::new()
    } else {
        format!(
            " Learned from earlier rounds: {}.",
            note.learned
                .iter()
                .map(|(l, s)| format!("{l} ×{s:.2}"))
                .collect::<Vec<_>>()
                .join(", ")
        )
    };
    let msg = format!(
        "Round {}: applied {} — listen, then say whether it's better.{}",
        sess.round,
        moves_line(
            &sess
                .last
                .as_ref()
                .map(|r| r.moves.clone())
                .unwrap_or_default()
        ),
        learned
    );
    Ok(step_result(sess, TuneStatus::Candidate, Some(note), msg))
}

/// One round of the tune loop — see the module doc.
#[tauri::command]
pub(crate) async fn doctor_tune_step<R: tauri::Runtime>(
    app: tauri::AppHandle<R>,
    state: State<'_, AppState>,
    job: DoctorTuneJob,
) -> Result<DoctorTuneStep, String> {
    let (stim_path, from_capture) = resolve_stimulus_with_capture(
        &app,
        None,
        job.ctx.topology_id.clone(),
        job.ctx.profile_id.as_deref(),
    )?;
    with_released_seize(state.session.clone(), move || {
        let ctx = &job.ctx;
        let cal = if from_capture {
            None
        } else {
            ctx.calibration_lufs
        };
        let key = session_key(ctx, &stim_path, cal);
        let run = || -> Result<DoctorTuneStep, String> {
            let mut guard = crate::lock_ok(&SESSION);
            let same = guard.as_ref().is_some_and(|s| s.key == key);
            match job.decision {
                TuneDecision::Start => {
                    *guard = None;
                    let stim = leveller::doctor_stim_slice(read_stimulus_calibrated(&stim_path, cal)?);
                    let kind = if from_capture {
                        doctor::StimulusKind::Capture
                    } else {
                        doctor::StimulusKind::Synthetic
                    };
                    let family = family_of_topology(ctx.topology_id.as_deref());
                    let iso = resolve_sound_isolation(
                        &ctx.nodes,
                        &ctx.footswitches,
                        ctx.scene,
                        ctx.footswitch,
                        ctx.list_index,
                        &mut std::collections::HashMap::new(),
                    );
                    let nodes = doctor::effective_nodes(&ctx.nodes, &ctx.scene_overlay, &iso.bypass);
                    let tail_ms = u64::from(doctor::doctor_tail_ms(&nodes));
                    doctor_fresh_load(ctx.list_index, || {
                        log::info!(
                            "doctor_tune: slot {} — waiting for the device to commit the previous save",
                            ctx.list_index
                        );
                    })?;
                    let baseline = measure_round(
                        &CaptureCtx {
                            job: ctx,
                            stim: &stim,
                            iso: &iso,
                            tail_ms,
                            family,
                            kind,
                        },
                        &nodes,
                        Vec::new(),
                        Vec::new(),
                        None,
                    )?;
                    crate::settle(std::time::Duration::from_millis(leveller::RECONNECT_GAP_MS));
                    let mut sess = TuneSession {
                        key,
                        family,
                        kind,
                        stim,
                        iso,
                        tail_ms,
                        nodes,
                        baseline,
                        last: None,
                        trials: Vec::new(),
                        rejected_streak: 0,
                        round: 0,
                    };
                    let step = propose_and_measure(&mut sess, ctx)?;
                    *guard = Some(sess);
                    Ok(step)
                }
                TuneDecision::Better | TuneDecision::Worse => {
                    if !same {
                        return Err("the tune loop isn't running on this sound — start it again".to_string());
                    }
                    let sess = guard.as_mut().ok_or("no tune session")?;
                    let Some(last) = sess.last.take() else {
                        return Err("there is no candidate to judge — start a new round".to_string());
                    };
                    let accepted = job.decision == TuneDecision::Better;
                    if let Some(t) = sess.trials.last_mut() {
                        t.accepted = accepted;
                    }
                    if accepted {
                        sess.baseline = last;
                        sess.rejected_streak = 0;
                    } else {
                        sess.rejected_streak += 1;
                    }
                    let step = propose_and_measure(sess, ctx)?;
                    Ok(step)
                }
            }
        };
        let result = run();
        leveller::reamp_off_guaranteed("doctor_tune_step");
        result
    })
    .await
}

/// End the loop: clear the session; `discard` reloads the stored preset (the
/// candidate's unsaved edit buffer is dropped). After a `doctor_save` of the
/// step's ops the frontend ends WITHOUT discard (the saved state is what the
/// device holds).
#[tauri::command]
pub(crate) async fn doctor_tune_end(
    state: State<'_, AppState>,
    list_index: u32,
    discard: bool,
) -> Result<(), String> {
    with_released_seize(state.session.clone(), move || {
        *crate::lock_ok(&SESSION) = None;
        if discard {
            leveller::restore_saved_preset(list_index)?;
        }
        Ok(())
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn moves_line_groups_per_block_in_order() {
        let m =
            |node: &str, block: &str, label: &str, from: &str, to: &str| doctor_plan::PlanMove {
                group_id: "G1".into(),
                node_id: node.into(),
                model: node.into(),
                block_name: block.into(),
                param: label.to_lowercase(),
                control_label: label.into(),
                unit: doctor_plan::ControlUnit::Knob,
                from: 0.5,
                to: 0.4,
                from_label: from.into(),
                to_label: to.into(),
            };
        let line = moves_line(&[
            m("amp", "'65 Twin Reverb", "Bass", "6.0", "4.0"),
            m("ts", "Greenbox 8", "Tone", "5.0", "6.0"),
            m("amp", "'65 Twin Reverb", "Treble", "5.0", "5.5"),
        ]);
        assert_eq!(
            line,
            "'65 Twin Reverb: Bass 6.0 → 4.0, Treble 5.0 → 5.5 · Greenbox 8: Tone 5.0 → 6.0"
        );
    }

    #[test]
    fn tune_decision_deserializes_lowercase() {
        let j: DoctorTuneJob = serde_json::from_str(
            r#"{ "ctx": { "listIndex": 4, "name": "Lead", "ops": [] }, "decision": "better" }"#,
        )
        .expect("deserializes");
        assert_eq!(j.decision, TuneDecision::Better);
        assert_eq!(j.ctx.list_index, 4);
    }
}
