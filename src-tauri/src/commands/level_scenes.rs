//! Per-scene leveling commands + setlist common-target leveling.
#![allow(clippy::too_many_arguments)]
use crate::*;

/// One resolved amp knob: `(group_id, node_id, current_outputLevel)`.
pub(crate) type AmpKnobSpec = (String, String, f32);
/// A candidate leveling knob for `level_scenes_apply` — the frontend passes EVERY
/// amp-level candidate (it owns amp-ness via the models catalog); the backend picks
/// PER SCENE the one whose block is actually ON in that scene.
#[derive(serde::Deserialize, serde::Serialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
pub struct LevelBlockArg {
    pub(crate) group_id: String,
    pub(crate) node_id: String,
    pub(crate) parameter_id: String,
    pub(crate) value: f32,
}

#[derive(Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SceneLevelProgressItem {
    scene_slot: u32,
    status: String,
    result: Option<leveller::LevelResult>,
    message: Option<String>,
}

/// One scene-leveling request from the wizard: a wire scene slot + its OWN loudness
/// target. Per-job targets (mirroring `FootswitchLevelJob`) let a preset with a mix of
/// targets level in ONE batch — one prepass, one runner, one deferred save.
#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SceneLevelJobArg {
    scene_slot: u32,
    target_lufs: f64,
    /// How to read `target_lufs` — see [`leveller::SceneTargetMode`]. Defaulted to `match`
    /// (today's behavior) so an existing payload with no `targetMode` key is unchanged.
    #[serde(default)]
    target_mode: leveller::SceneTargetMode,
    /// The user's OWN control for this scene. Absent = the amp-`outputLevel` path (joint-k,
    /// rebalance, every existing caller).
    #[serde(default)]
    handle: Option<SceneHandleArg>,
}

/// A user-chosen scene leveling control: the block param the solve should sweep INSTEAD of
/// the active amp's `outputLevel`.
#[derive(serde::Deserialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SceneHandleArg {
    group_id: String,
    node_id: String,
    parameter_id: String,
}

/// Wire payload for `tmp://leveling-lufs` — the advisory live measured loudness streamed
/// while a leveling capture runs, so the UI can show a "measuring…" readout. ADVISORY: this
/// is the loudness at the reference level, NOT the final preset level (the result row is the
/// confirm). `momentary` is the current hop's plain RMS in dB (decorative fuel for the live
/// VU bars, not the solve). Mirrored in `src/lib/types.ts`.
#[derive(Clone, serde::Serialize)]
pub(crate) struct LiveLufsEvent {
    lufs: f64,
    momentary: f64,
}

/// RAII guard: installs an advisory live-LUFS sink that emits `tmp://leveling-lufs` for the
/// lifetime of a leveling run, clearing it on drop (incl. unwind). Every leveling command
/// runs serialized under the device-op lock, so only one guard is ever live at a time.
pub(crate) struct LiveLufsGuard;

impl LiveLufsGuard {
    pub(crate) fn install<R: tauri::Runtime>(app: tauri::AppHandle<R>) -> Self {
        use tauri::Emitter;
        audio::set_live_lufs_sink(Box::new(move |lufs, momentary| {
            let _ = app.emit("tmp://leveling-lufs", LiveLufsEvent { lufs, momentary });
        }));
        LiveLufsGuard
    }
}

impl Drop for LiveLufsGuard {
    fn drop(&mut self) {
        audio::clear_live_lufs_sink();
    }
}

pub(crate) static SCENE_LEVEL_CANCEL: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

#[tauri::command]
pub(crate) fn cancel_scene_leveling() {
    SCENE_LEVEL_CANCEL.store(true, SeqCst);
    // Also wake the in-flight capture/settle waits (see `device_gate::OP_ABORT`).
    crate::request_op_abort();
}

fn pick_scene_level_knob(
    slot: u32,
    scene: u32,
    candidates: &[LevelBlockArg],
) -> Result<(leveller::LevelKnob, f32, f32, f32), String> {
    let scene_slot = if scene >= session::BASE_SCENE_SLOT {
        None
    } else {
        Some(scene)
    };
    // ONE rich session (HW-rearchitected): heartbeat warmup → loads
    // via send_and_collect → live doc from the accumulated field-3 pushes. The
    // old connect → load → drop → connect_for_discovery chain is broken on fw
    // 1.8.45 twice over: a close chased by a re-open wedges the device's next
    // exclusive open (0xe00002c5 lockout), and field-78 kills field-3 delivery
    // for its whole session anyway. After each load the raw accumulator is
    // cleared so the doc reflects the POST-scene live state (the pick must read
    // the sounding graph, never stale pre-scene pushes).
    let live_doc = {
        let mut s = Session::connect()?;
        for _ in 0..16 {
            s.heartbeat()?;
            s.pump_collect(120)?;
        }
        s.raw.clear();
        s.send_and_collect(&proto::load_preset((slot + 1) as u64, 1), 300)?;
        for _ in 0..8 {
            s.heartbeat()?;
            s.pump_collect(200)?;
        }
        if let Some(sl) = scene_slot {
            s.raw.clear();
            s.send_and_collect(&proto::load_scene(sl as u64), 300)?;
            for _ in 0..8 {
                s.heartbeat()?;
                s.pump_collect(200)?;
            }
        }
        s.current_preset_value()?
    };
    for c in candidates {
        log::info!(
            "pick_scene_level_knob scene={scene} candidate {}/{}/{} live_bypass={:?}",
            c.group_id,
            c.node_id,
            c.parameter_id,
            scenes::block_bypass_in_live_graph(&live_doc, &c.group_id, &c.node_id),
        );
    }
    let picked = candidates
        .iter()
        .filter(|c| is_amp_output_level_param(&c.parameter_id))
        .find(|c| {
            scenes::block_bypass_in_live_graph(&live_doc, &c.group_id, &c.node_id) == Some(false)
        })
        .ok_or_else(|| format!("no active amp outputLevel control found for scene slot {scene}"))?;
    let (lo, hi) = knob_bounds(picked.value);
    Ok((
        leveller::LevelKnob::Block {
            group_id: picked.group_id.clone(),
            node_id: picked.node_id.clone(),
            parameter_id: picked.parameter_id.clone(),
            scene_slot,
        },
        lo,
        hi,
        picked.value,
    ))
}

/// Level ONE scene the capture-per-connection way (`level_preset_block`): pick
/// the scene's knob from its live graph, then closed-loop with fresh re-amp
/// captures. The legacy `level_scenes_apply` path; the shipped batched flow is
/// `level_scenes_apply_batched` → `leveller::level_scenes_oneshot` (or
/// `level_scenes_rebalance` for the parallel-amp option) — NOT the retired
/// bench-only `level_scenes_live_batched` (see notes/leveling.md).
fn level_one_scene_legacy(
    slot: u32,
    scene: u32,
    candidates: &[LevelBlockArg],
    stimulus: &[f32],
    target_lufs: f64,
    save: bool,
) -> Result<leveller::LevelResult, String> {
    let (knob, lo, hi, _current) = pick_scene_level_knob(slot, scene, candidates)?;
    // 800 ms before the leveller's first fresh connect — the empirical safe gap
    // after a rich-session close (shorter chases trip the device's open lockout).
    crate::settle(std::time::Duration::from_millis(800));
    let opts = leveller::LevelOptions {
        save,
        verify: true,
        ..Default::default()
    };
    leveller::level_preset_block(slot, stimulus, &knob, lo, hi, target_lufs, opts, || false)
}

/// Per-scene leveling APPLY (chosen mechanism: enable scene mode on the amp
/// block, level only the amp `outputLevel` control). For each selected scene, drive
/// the scene's ACTIVE amp's `outputLevel` knob closed-loop to `target_lufs` with
/// per-block Scene Edit enabled —
/// so the level lands on that scene's overlay, not the base. The knob is resolved
/// PER SCENE from `candidates` by the scene overlay's `bypass` (HW-found:
/// a preset can carry several amps with scenes swapping which is live — leveling a
/// bypassed amp's knob measures flat and clamps).
/// `scene_slots` are the WIRE slots: 0-based `scenes[]` indices for FS scenes;
/// `session::BASE_SCENE_SLOT` (8) = the base/preset value (levelled WITHOUT scene-edit
/// — a preset load activates base, so no scene recall is needed).
/// DEVICE WRITE when `save` — opt-in, gated by the read-only HW policy + the leveling
/// overlay confirm. Reuses `level_preset_block` (the scene context rides the knob and
/// is re-asserted on every connection). Each scene is a self-contained leveling pass.
#[tauri::command]
#[allow(clippy::too_many_arguments)]
pub(crate) async fn level_scenes_apply(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    slot: u32,
    scene_slots: Vec<u32>,
    candidates: Vec<LevelBlockArg>,
    target_lufs: f64,
    save: bool,
    topology_id: Option<String>,
    calibration_lufs: Option<f32>,
) -> Result<Vec<leveller::LevelResult>, String> {
    if !candidates
        .iter()
        .any(|c| is_amp_output_level_param(&c.parameter_id))
    {
        return Err("per-scene leveling needs at least one amp outputLevel candidate".to_string());
    }
    if scene_slots.is_empty() {
        return Err("no scenes selected".to_string());
    }
    let target_lufs = target_lufs + playback_offset_for(&app, topology_id.as_deref());
    let stim_path = resolve_stimulus(&app, None, topology_id)?;
    with_released_seize(state.session.clone(), move || {
        let stim = read_stimulus_calibrated(&stim_path, calibration_lufs)?;
        let run = || -> Result<Vec<leveller::LevelResult>, String> {
            let mut results = Vec::with_capacity(scene_slots.len());
            for scene in &scene_slots {
                let r = level_one_scene_legacy(
                    slot,
                    *scene,
                    &candidates,
                    &stim,
                    target_lufs,
                    save,
                )?;
                log::info!(
                    "level_scenes_apply slot={slot} scene={scene} save={save} final_level={:.4} measured={:.2} clamped={}",
                    r.final_level, r.measured_lufs, r.clamped,
                );
                results.push(r);
            }
            Ok(results)
        };
        let result = run();
        // Run-end backstop, success or failure (see `reamp_off_guaranteed`: the
        // device drops an in-session OFF sent after ~1 s of idle — every capture).
        leveller::reamp_off_guaranteed("level_scenes_apply");
        result
    })
    .await
}

#[tauri::command]
#[allow(clippy::too_many_arguments)]
pub(crate) async fn level_scenes_apply_batched<R: tauri::Runtime>(
    app: tauri::AppHandle<R>,
    state: State<'_, AppState>,
    slot: u32,
    jobs: Vec<SceneLevelJobArg>,
    candidates: Vec<LevelBlockArg>,
    save: bool,
    rebalance: bool,
    topology_id: Option<String>,
    calibration_lufs: Option<f32>,
    profile_id: Option<String>,
    on_result: tauri::ipc::Channel<SceneLevelProgressItem>,
) -> Result<Vec<leveller::LevelResult>, String> {
    if jobs.is_empty() {
        return Err("no scenes selected".to_string());
    }
    // A row that names its own control needs no amp candidate and no routing classification —
    // the user picked the knob. So this pre-device guard fires only for a batch where NOBODY
    // named one (every row is an amp-`outputLevel` joint-k row and the whole run is doomed);
    // a MIXED batch proceeds and `build_scene_jobs_with_handles` skips just the amp rows.
    if jobs.iter().all(|j| j.handle.is_none())
        && !candidates
            .iter()
            .any(|c| is_amp_output_level_param(&c.parameter_id))
    {
        return Err("per-scene leveling needs at least one amp outputLevel candidate".to_string());
    }
    SCENE_LEVEL_CANCEL.store(false, SeqCst);
    // Playback compensation is one offset for the whole batch; each job's own target
    // gets it added below (the per-scene targets differ, the offset does not).
    let offset = playback_offset_for(&app, topology_id.as_deref());
    let (stim_path, calibration_lufs) = resolve_stimulus_for_leveling(
        &app,
        None,
        topology_id,
        profile_id.as_deref(),
        calibration_lufs,
    )?;
    let app_evt = app.clone();
    with_released_seize(state.session.clone(), move || {
        // Stream advisory live LUFS while each capture runs (dropped at closure end).
        let _lufs = LiveLufsGuard::install(app_evt);
        let stim = read_stimulus_calibrated(&stim_path, calibration_lufs)?;
        let mut scene_slots: Vec<u32> = jobs.iter().map(|j| j.scene_slot).collect();
        // Force-append base so the prepass always harvests a base doc — one more chance at a
        // complete `audioGraph.template` for `build_scene_jobs`' routing classification (the
        // scene-vs-base repair diff it was originally added for is gone: `set_knobs` now
        // enables Scene Edit only where the node has no overlay, so nothing gets reseeded
        // away). Stripped back out below (before the wire-job match) when the user never
        // asked to level base itself.
        let base_requested = scene_slots.contains(&session::BASE_SCENE_SLOT);
        if !base_requested {
            scene_slots.push(session::BASE_SCENE_SLOT);
        }
        // THE field-8 read for this preset (one per run, before any other session — nothing
        // has just closed one here, and it leaves the validated prepass→runner boundary
        // below untouched). Feeds the raw per-node scene overlays (`scene_jobs::
        // scene_overlay`, the Scene Edit enable + bake gates) AND `build_scene_jobs`'
        // routing-structure fallback — which still only fills in for a live doc set that
        // lacks `audioGraph.template`, so an unconditional `Some` changes no classification.
        let saved = crate::read_saved_preset(slot);
        let run_batched = |save_run: bool| -> Result<Vec<leveller::BatchedSceneOutcome>, String> {
            // Un-engaged pre-pass (scene docs → jobs), then the ONE-SHOT runner:
            // amp `outputLevel` is linear in dB, so each scene is measured once at a
            // reference level (ISOLATED fresh re-amp capture) and solved exactly — the
            // BatchedLive shared-stream loop mis-measured scenes (HW).
            // `restore_scene` = the preset's original active scene: the batch-end
            // single save recalls it first so the preset persists in the same
            // base/scene/footswitch state it was loaded in.
            // DARK: overlay path validated by `probe --overlay-ab` (76/76 scene-amp pairs,
            // 0 bypass mismatches) but adoption is a gated follow-up — flip to `true` then
            // (see prepass_scene_docs_via's adoption-time TODO). `false` = live prepass today.
            let (docs, restore_scene) = prepass_scene_docs_via(slot, &scene_slots, false)?;
            // Inter-session HID gap: the prepass session has just closed; the one-shot
            // runner opens a fresh one. Reuse the leveller's HW-proven open-after-close
            // gap (was a hard-coded 800, copied from the bench). build_scene_jobs below
            // is pure CPU, so this is the only wait here.
            crate::settle(std::time::Duration::from_millis(leveller::RECONNECT_GAP_MS));
            // `build_scene_jobs` stamps a base target on every job; override each with its
            // OWN wire job's offset-adjusted target (match by scene slot) so a mixed-target
            // preset levels in this ONE batch. `jobs` is non-empty (guarded above).
            let base_target = jobs[0].target_lufs + offset;
            // Each row's own control, threaded INTO the builder (sparse, keyed by wire scene
            // slot): a handle row is built from that param and never consults the amp
            // classifier, so an unreadable routing template can only skip the rows that
            // actually need the amp.
            let handles: Vec<(u32, SceneHandleSpec)> = jobs
                .iter()
                .filter_map(|j| {
                    j.handle.as_ref().map(|h| {
                        (
                            j.scene_slot,
                            SceneHandleSpec {
                                group_id: &h.group_id,
                                node_id: &h.node_id,
                                parameter_id: &h.parameter_id,
                            },
                        )
                    })
                })
                .collect();
            let mut scene_jobs = build_scene_jobs_with_handles(
                &scene_slots,
                &candidates,
                &docs,
                base_target,
                saved.as_ref(),
                &handles,
            )?;
            if !base_requested {
                scene_jobs.retain(|sj| sj.scene_slot != session::BASE_SCENE_SLOT);
            }
            // Error on ANY slot mismatch between the built jobs and the wire jobs — a silent
            // default (especially NaN, which `.min(k_cap)` would collapse to the cap and slam
            // the amp) must never reach a solve. This is also where each row's target MODE is
            // stamped: one reconciliation pass over the wire jobs, not two.
            for sj in scene_jobs.iter_mut() {
                let arg = jobs
                    .iter()
                    .find(|j| j.scene_slot == sj.scene_slot)
                    .ok_or_else(|| {
                        format!("built scene job slot {} has no wire target", sj.scene_slot)
                    })?;
                if !arg.target_lufs.is_finite() {
                    return Err(format!(
                        "scene slot {} has a non-finite target ({})",
                        arg.scene_slot, arg.target_lufs
                    ));
                }
                sj.target_lufs = arg.target_lufs + offset;
                sj.target_mode = arg.target_mode;
            }
            if let Some(j) = jobs
                .iter()
                .find(|j| !scene_jobs.iter().any(|sj| sj.scene_slot == j.scene_slot))
            {
                return Err(format!(
                    "requested scene slot {} produced no scene job",
                    j.scene_slot
                ));
            }
            let on_scene = |scene, done: Option<&leveller::BatchedSceneOutcome>| {
                let _ = on_result.send(scene_progress_item(slot, save_run, scene, done));
            };
            let cancelled = || SCENE_LEVEL_CANCEL.load(SeqCst);
            // `rebalance` (opt-in) equalizes a path-MERGE scene's two lanes before joint-k;
            // non-mergeable scenes fall through to the same joint-k either way.
            if rebalance {
                leveller::level_scenes_rebalance(
                    slot,
                    &scene_jobs,
                    &stim,
                    save_run,
                    restore_scene,
                    saved.as_ref(),
                    on_scene,
                    cancelled,
                )
            } else {
                leveller::level_scenes_oneshot(
                    slot,
                    &scene_jobs,
                    &stim,
                    save_run,
                    restore_scene,
                    saved.as_ref(),
                    on_scene,
                    cancelled,
                )
            }
        };
        // Per-scene leveling drives ONLY the active amp's `outputLevel`. When a scene
        // can't reach target even at the knob's limit it CLAMPS and reports the achieved
        // loudness — we do NOT raise the global `presetLevel` to compensate. Raising it
        // lifts EVERY other scene off-target (presetLevel is the Base's job, settled once
        // before the scene pass), and HW the old boost-and-rerun drove
        // presetLevel to 1.0 and blew preset 001's loud scenes 5–7 LU over target.
        let outcome = run_batched(save);
        let result = match outcome {
            Ok(outcomes) => Ok(outcomes
                .iter()
                .filter(|o| o.failure.is_none())
                .map(|o| outcome_to_level_result(slot, save, o))
                .collect()),
            Err(e) if e == leveller::CANCELLED => {
                let _ = on_result.send(SceneLevelProgressItem {
                    scene_slot: session::BASE_SCENE_SLOT,
                    status: "cancelled".to_string(),
                    result: None,
                    message: Some(e),
                });
                Ok(Vec::new())
            }
            Err(e) => Err(e),
        };
        leveller::reamp_off_guaranteed("level_scenes_apply_batched");
        result
    })
    .await
}

/// Build the streamed progress row for one scene step — `None` = the step just STARTED
/// (spinner), `Some(outcome)` = it finished (a `done` result or an `error` message). Shared
/// by `level_scenes_apply_batched` + `redistribute_headroom` so their per-row wire shape can't
/// drift.
fn scene_progress_item(
    slot: u32,
    save: bool,
    scene: u32,
    done: Option<&leveller::BatchedSceneOutcome>,
) -> SceneLevelProgressItem {
    match done {
        None => SceneLevelProgressItem {
            scene_slot: scene,
            status: "active".to_string(),
            result: None,
            message: None,
        },
        Some(o) => match &o.failure {
            None => SceneLevelProgressItem {
                scene_slot: scene,
                status: "done".to_string(),
                result: Some(outcome_to_level_result(slot, save, o)),
                message: None,
            },
            Some(e) => SceneLevelProgressItem {
                scene_slot: scene,
                status: "error".to_string(),
                result: None,
                message: Some(e.clone()),
            },
        },
    }
}

/// Map a [`leveller::BatchedSceneOutcome`] onto the frontend's `LevelResult`
/// contract (the batched runner's outcome is per-scene; `verify_lufs` carries
/// the final measured window).
fn outcome_to_level_result(
    slot: u32,
    save: bool,
    o: &leveller::BatchedSceneOutcome,
) -> leveller::LevelResult {
    let lufs = o.final_lufs.unwrap_or(f64::NAN);
    leveller::LevelResult {
        slot,
        // IDENTITY, straight off the outcome — never the row's position. The caller
        // FILTERS failed outcomes out of the vec it returns, so a positional read
        // mislabels every row after a mid-batch failure (see `LevelResult::scene_slot`).
        scene_slot: Some(o.scene_slot),
        ref_level: o.final_level.unwrap_or(0.0),
        measured_lufs: lufs,
        constant_c: f64::NAN,
        final_level: o.final_level.unwrap_or(0.0),
        // Per-scene target lives on the outcome (a batch can mix targets).
        target_lufs: o.target_lufs,
        predicted_lufs: lufs,
        clamped: o.clamped,
        saved: save,
        verify_lufs: o.final_lufs,
        iterations: o.windows.max(o.writes),
        dynamic_spread_lu: o.dynamic_spread_lu,
        clamp_reason: o.clamp_reason.clone(),
        verify_by_ear: o.verify_by_ear,
        // Scene rows write amp outputLevel, not presetLevel — nothing to revert here.
        previous_level: None,
        // Scene path: no predicted true peak this cycle (only the one-shot presetLevel
        // path in `level_preset` estimates it).
        true_peak_dbtp: None,
        persist_mismatch: o.persist_mismatch,
        // `target_lufs` above is already the EFFECTIVE (offset-shifted) target; this is the
        // shift itself, which the frontend can't derive from what it sent.
        target_offset_lu: o.target_offset_lu,
    }
}

// ───────────────────── Scene handle picker (enumeration) ─────────────────────

/// One control a scene row could be leveled on, with the two annotations the picker cannot
/// derive on its own. `class`/`range` come from [`crate::param_class`]; `current` is the
/// value AUTHORED IN THAT SCENE (overlay if present, else base).
///
/// Block DISPLAY info is `groupId`/`nodeId`/`fenderId` only — the frontend owns the
/// friendly name (it has the models catalog; the backend deliberately does not, the same
/// split as the amp candidates the Level view already sends down).
#[derive(serde::Serialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SceneHandleCandidate {
    group_id: String,
    node_id: String,
    fender_id: String,
    parameter_id: String,
    /// `"level_linear" | "level_db" | "wet_mix"` — the classifier's verdict, serialized
    /// straight off [`param_class::ParamClass`] (no hand-rolled mapping to drift from the
    /// table). `"other"` never appears: `footswitch::level_candidates_for_node` admits only
    /// params the classifier recognises, so an unrecognised one is not a candidate at all.
    class: param_class::ParamClass,
    /// The param's usable `[lo, hi]` — NOT `[0,1]` for every control (`ACD_Boost.gain` is
    /// raw dB over `[0, 12]`). For a `wet_mix` the LOW bound is already raised to the wet
    /// floor, so the picker shows the range the solve may actually write.
    range: [f32; 2],
    current: f32,
    /// Does writing this control in THIS scene affect ONLY this scene?
    /// * `"isolated"` — the scene carries a knob overlay for the node (or none at all, in
    ///   which case the Scene Edit enable materialises one). The write stays here.
    /// * `"shared_with_base"` — the node's Scene Edit flag is OFF (a bypass-only overlay),
    ///   so this scene reads the BASE knob and a write would change every sharing scene.
    ///   `set_knobs` REFUSES such a write, so the picker must warn rather than offer it
    ///   silently.
    /// * `"unknown"` — the saved read could not answer (a truncated `scenes` tail). Both
    ///   write shapes corrupt from there, so the write is refused too.
    scope: String,
    /// `"full"` = the control has room in both directions. `"lowers_only"` = its authored
    /// value already sits at (or within a whisker of) the top of its range, so this handle
    /// can only make the scene QUIETER — the picker should say so before the user finds out
    /// from a clamped row.
    headroom: String,
}

/// The handle candidates for ONE scene.
#[derive(serde::Serialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SceneHandleRow {
    /// 0-based `scenes[]` wire index. FS scenes only — base handles are the preset lane's
    /// own picker (`list_level_blocks`), and the overlay annotations below are meaningless
    /// for base (it has no overlay concept).
    scene_slot: u32,
    candidates: Vec<SceneHandleCandidate>,
}

/// Within this fraction of the range's top, a control counts as having no room to go up.
/// A knob authored at 0.995 of `[0,1]` has ~0.04 dB of boost left — reporting that as "full"
/// headroom would be a lie the user only discovers from a clamped row.
const HANDLE_TOP_EPS_FRACTION: f32 = 0.01;

/// Per-scene handle candidates for the picker. PURE apart from ONE field-8 read: every
/// annotation (class, range, per-scene current value, overlay scope) comes out of the saved
/// document, so no scene is recalled on the unit and nothing is measured.
#[tauri::command]
pub(crate) async fn list_scene_level_handles(
    state: State<'_, AppState>,
    slot: u32,
) -> Result<Vec<SceneHandleRow>, String> {
    with_released_seize(state.session.clone(), move || {
        let (preset, _, _) = read_slot_preset_parsed(slot)?;
        Ok(scene_handle_rows(&preset))
    })
    .await
}

/// [`list_scene_level_handles`]'s pure core — the whole annotation rule, unit-testable
/// against a preset document with no device in the loop.
fn scene_handle_rows(preset: &serde_json::Value) -> Vec<SceneHandleRow> {
    let scene_count = preset
        .get("scenes")
        .and_then(|s| s.as_array())
        .map_or(0, |a| a.len()) as u32;
    // nodeId → its base `dspUnitParameters` (the candidate source; a scene overlay only
    // ever restates params the base node already carries).
    let mut params: std::collections::HashMap<String, serde_json::Map<String, serde_json::Value>> =
        std::collections::HashMap::new();
    audiograph::for_each_node(preset, |obj| {
        if let Some(nid) = obj.get("nodeId").and_then(|v| v.as_str()) {
            params.insert(
                nid.to_string(),
                obj.get("dspUnitParameters")
                    .and_then(|p| p.as_object())
                    .cloned()
                    .unwrap_or_default(),
            );
        }
    });
    let roster = audiograph::roster(preset);
    (0..scene_count)
        .map(|scene| {
            let mut candidates = Vec::new();
            for (group_id, node_id, fender_id) in &roster {
                let Some(base_params) = params.get(node_id) else {
                    continue;
                };
                // The overlay decides BOTH the scope annotation and which values are the
                // scene's own — one lookup per node, reused for every candidate on it.
                // `scene_overlay_for`, not `scene_overlay`: the roster triple is already in
                // hand, so the string-keyed wrapper's whole-graph `roster_entry` walk would
                // be re-paid once per (scene, node) pair for an answer we resolved once.
                let overlay = scene_overlay_for(preset, scene, (group_id, node_id, fender_id));
                let (scope, overlay_params) = match &overlay {
                    SceneOverlay::Full(p) => ("isolated", Some(*p)),
                    // No overlay yet: the Scene Edit enable materialises one, so a write
                    // here does land on this scene alone.
                    SceneOverlay::Absent => ("isolated", None),
                    SceneOverlay::BypassOnly(_) => ("shared_with_base", None),
                    SceneOverlay::Unknown => ("unknown", None),
                };
                for mut c in
                    footswitch::level_candidates_for_node(group_id, node_id, fender_id, base_params)
                {
                    if let Some(v) = overlay_params
                        .and_then(|p| p.get(&c.parameter_id))
                        .and_then(|v| v.as_f64())
                    {
                        c.current = v;
                    }
                    let current = c.current as f32;
                    let target = leveller::FsParamTarget::new(fender_id, &c.parameter_id, current);
                    let (lo, hi) = target.bounds();
                    let headroom = if current >= hi - (hi - lo).abs() * HANDLE_TOP_EPS_FRACTION {
                        "lowers_only"
                    } else {
                        "full"
                    };
                    candidates.push(SceneHandleCandidate {
                        group_id: c.group_id,
                        node_id: c.node_id,
                        fender_id: c.fender_id,
                        parameter_id: c.parameter_id,
                        class: target.info.class,
                        range: [lo, hi],
                        current,
                        scope: scope.to_string(),
                        headroom: headroom.to_string(),
                    });
                }
            }
            SceneHandleRow {
                scene_slot: scene,
                candidates,
            }
        })
        .collect()
}

// ───────────────────────── Gain-budget redistribution (PR5) ─────────────────────────

/// One touched knob's PRE-redistribution value — the Restore anchor. `scene_slot` `None` =
/// the base amp (plain write); `Some(i)` = the i-th FS scene overlay (scene-edit write).
#[derive(Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct PreviousKnob {
    group_id: String,
    node_id: String,
    scene_slot: Option<u32>,
    value: f32,
}

/// Result of a redistribution: the per-sound outcomes + the values it rewrote, recorded for
/// the Summary's one-click Restore (presetLevel + every touched amp `outputLevel`).
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct RedistributeResult {
    results: Vec<leveller::LevelResult>,
    previous_preset_level: f32,
    previous_knobs: Vec<PreviousKnob>,
    delta_db: f64,
    new_preset_level: f32,
}

/// Give clamped scenes headroom by redistributing the gain budget (loud-preset class,
/// single-amp v1): raise `presetLevel` by `delta` and re-level the base amp + every scene
/// back to target, so clamped scenes gain headroom while non-clamped sounds stay on target.
/// `jobs` are the WHOLE preset's sounds — base (`session::BASE_SCENE_SLOT`) + every FS scene —
/// each with its OWN target. `worst_clamped_deficit_db` (from the run: max `target − achieved`
/// over the clamped scenes) drives `delta` together with the preset's read-back presetLevel
/// headroom and the down-room before the lowest compensated knob hits the silence floor.
/// Opt-in (the Summary action) + reversible (returns the recorded previous values).
#[tauri::command]
#[allow(clippy::too_many_arguments)]
pub(crate) async fn redistribute_headroom<R: tauri::Runtime>(
    app: tauri::AppHandle<R>,
    state: State<'_, AppState>,
    slot: u32,
    jobs: Vec<SceneLevelJobArg>,
    candidates: Vec<LevelBlockArg>,
    worst_clamped_deficit_db: f64,
    topology_id: Option<String>,
    calibration_lufs: Option<f32>,
    profile_id: Option<String>,
    on_result: tauri::ipc::Channel<SceneLevelProgressItem>,
) -> Result<RedistributeResult, String> {
    if !candidates
        .iter()
        .any(|c| is_amp_output_level_param(&c.parameter_id))
    {
        return Err("redistribution needs at least one amp outputLevel candidate".to_string());
    }
    if jobs.is_empty() {
        return Err("no sounds to redistribute".to_string());
    }
    if !worst_clamped_deficit_db.is_finite() || worst_clamped_deficit_db <= 0.0 {
        return Err("redistribution needs a positive clamped-scene deficit".to_string());
    }
    SCENE_LEVEL_CANCEL.store(false, SeqCst);
    let offset = playback_offset_for(&app, topology_id.as_deref());
    let (stim_path, calibration_lufs) = resolve_stimulus_for_leveling(
        &app,
        None,
        topology_id,
        profile_id.as_deref(),
        calibration_lufs,
    )?;
    let app_evt = app.clone();
    with_released_seize(state.session.clone(), move || {
        let _lufs = LiveLufsGuard::install(app_evt);
        let stim = read_stimulus_calibrated(&stim_path, calibration_lufs)?;
        let scene_slots: Vec<u32> = jobs.iter().map(|j| j.scene_slot).collect();

        // THE field-8 read for this preset (see `level_scenes_apply_batched`): raw scene
        // overlays + the routing-structure fallback, once, ahead of the prepass session.
        let saved = crate::read_saved_preset(slot);
        // Prepass: ONE rich session loads the preset + harvests each sound's live doc (the
        // pre-raise presetLevel + per-sound current outputLevel). No re-amp yet.
        let (docs, restore_scene) = prepass_scene_docs_via(slot, &scene_slots, false)?;
        crate::settle(std::time::Duration::from_millis(leveller::RECONNECT_GAP_MS));
        let base_target = jobs[0].target_lufs + offset;
        let mut scene_jobs =
            build_scene_jobs(&scene_slots, &candidates, &docs, base_target, saved.as_ref())?;
        // Stamp each job with its OWN (offset-adjusted) target.
        for sj in scene_jobs.iter_mut() {
            let arg = jobs
                .iter()
                .find(|j| j.scene_slot == sj.scene_slot)
                .ok_or_else(|| format!("built job slot {} has no wire target", sj.scene_slot))?;
            if !arg.target_lufs.is_finite() {
                return Err(format!("scene slot {} has a non-finite target", arg.scene_slot));
            }
            sj.target_lufs = arg.target_lufs + offset;
        }
        // Reverse-check (mirrors `level_scenes_apply_batched`): a requested sound that produced
        // NO job is a silent drop — fail loudly rather than redistribute a partial sound set.
        if let Some(j) = jobs
            .iter()
            .find(|j| !scene_jobs.iter().any(|sj| sj.scene_slot == j.scene_slot))
        {
            return Err(format!(
                "requested sound slot {} produced no redistribution job",
                j.scene_slot
            ));
        }

        // Read the pre-raise presetLevel from the prepass docs (any sound's audioGraph).
        let preset_level = docs
            .iter()
            .find_map(|(_, d)| d.as_ref().and_then(audiograph::preset_level))
            .ok_or_else(|| "could not read the preset's current presetLevel".to_string())?
            as f32;
        // Record the previous values (pl + every touched knob) BEFORE any write — the Restore
        // anchor. `current` on each job knob is the sound's pre-raise outputLevel.
        let previous_knobs: Vec<PreviousKnob> = scene_jobs
            .iter()
            .flat_map(|sj| {
                sj.knobs.iter().filter_map(|kt| match &kt.knob {
                    leveller::LevelKnob::Block {
                        group_id,
                        node_id,
                        scene_slot,
                        ..
                    } => Some(PreviousKnob {
                        group_id: group_id.clone(),
                        node_id: node_id.clone(),
                        scene_slot: *scene_slot,
                        value: kt.current,
                    }),
                    leveller::LevelKnob::PresetLevel => None,
                })
            })
            .collect();
        // delta = min(worst clamped deficit, presetLevel headroom, down-room before the
        // lowest compensated knob hits the floor).
        let min_knob = scene_jobs
            .iter()
            .flat_map(|sj| sj.knobs.iter().map(|kt| kt.current))
            .fold(f32::INFINITY, f32::min);
        let delta_db = leveller::redistribute_delta_db(preset_level, worst_clamped_deficit_db, min_knob);
        if delta_db <= 1e-3 {
            return Err(
                "no headroom to redistribute (presetLevel already near max, or a knob at the floor) \
                 — try re-leveling to a lower common target instead"
                    .to_string(),
            );
        }
        let new_preset_level = (f64::from(preset_level) * 10f64.powf(delta_db / 20.0)).min(1.0) as f32;

        let on_scene = |scene, done: Option<&leveller::BatchedSceneOutcome>| {
            let _ = on_result.send(scene_progress_item(slot, true, scene, done));
        };
        let cancelled = || SCENE_LEVEL_CANCEL.load(SeqCst);
        let outcome = leveller::redistribute_clamped_headroom(
            slot,
            new_preset_level,
            &scene_jobs,
            &stim,
            restore_scene,
            saved.as_ref(),
            on_scene,
            cancelled,
        );
        let result = match outcome {
            Ok(outcomes) => Ok(RedistributeResult {
                results: outcomes
                    .iter()
                    .filter(|o| o.failure.is_none())
                    .map(|o| outcome_to_level_result(slot, true, o))
                    .collect(),
                previous_preset_level: preset_level,
                previous_knobs,
                delta_db,
                new_preset_level,
            }),
            Err(e) if e == leveller::CANCELLED => {
                let _ = on_result.send(SceneLevelProgressItem {
                    scene_slot: session::BASE_SCENE_SLOT,
                    status: "cancelled".to_string(),
                    result: None,
                    message: Some(e.clone()),
                });
                Err(e)
            }
            Err(e) => Err(e),
        };
        leveller::reamp_off_guaranteed("redistribute_headroom");
        result
    })
    .await
}

/// One-click Restore for a redistribution: write the recorded pre-redistribution values
/// (presetLevel + every touched amp `outputLevel`) back and save — the reverse of the atomic
/// write, on ONE session (base recall before save). Name-guarded (the run recorded the slot's
/// display name); a drifted list fails loudly rather than restoring onto a different preset.
#[tauri::command]
pub(crate) async fn restore_redistribution(
    state: State<'_, AppState>,
    slot: u32,
    preset_level: f32,
    knobs: Vec<PreviousKnob>,
    expected_name: String,
) -> Result<(), String> {
    with_released_seize(state.session.clone(), move || {
        let writes: Vec<leveller::PrevKnobWrite> = knobs
            .iter()
            .map(|k| leveller::PrevKnobWrite {
                group_id: k.group_id.clone(),
                node_id: k.node_id.clone(),
                scene_slot: k.scene_slot,
                value: k.value,
            })
            .collect();
        let r = leveller::restore_redistribution(slot, preset_level, &writes, &expected_name);
        leveller::reamp_off_guaranteed("restore_redistribution");
        r
    })
    .await
}

/// Headroom (LU) below the quietest-capable preset's ceiling when auto-picking
/// the setlist common target. Small margin so the floor preset isn't clamped.
const SETLIST_HEADROOM_LU: f64 = 1.0;

/// One preset in a setlist leveling job: its slot + the instrument profile's
/// topology (resolved to that instrument's stimulus).
#[derive(serde::Deserialize)]
pub(crate) struct SetlistJobEntry {
    slot: u32,
    topology_id: Option<String>,
    calibration_lufs: Option<f32>,
}

/// Level a whole setlist to one common loudness target so switching presets (and
/// instruments) on stage causes no jump. Measures every preset's ceiling, picks a
/// target just below the quietest, and applies it to all. Like `level_preset`, it
/// releases the app's seize, runs, then re-establishes the UI session.
#[tauri::command]
pub(crate) async fn level_setlist(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    entries: Vec<SetlistJobEntry>,
    save: bool,
) -> Result<leveller::SetlistResult, String> {
    if entries.is_empty() {
        return Err("no presets selected to level".to_string());
    }
    // Resolve each entry's stimulus path + playback compensation on the UI
    // thread (needs AppHandle; the store is read ONCE for the whole setlist).
    // The common target stays one loudness; a bass entry's offset rides its own
    // effective target inside the leveller.
    let playback = profiles::load(&app)
        .map(|s| s.playback_level)
        .unwrap_or_default();
    let resolved: Vec<(u32, String, Option<f32>, f64)> = entries
        .into_iter()
        .map(|e| {
            let offset_lu = profiles::playback_offset_lu(
                playback,
                stimulus_instrument(e.topology_id.as_deref()),
            );
            resolve_stimulus(&app, None, e.topology_id)
                .map(|p| (e.slot, p, e.calibration_lufs, offset_lu))
        })
        .collect::<Result<_, _>>()?;
    with_released_seize(state.session.clone(), move || {
        // Own each stimulus (calibrated if the profile has a real-output level),
        // then borrow into entries for the leveller.
        let stims: Vec<(u32, Vec<f32>, f64)> = resolved
            .into_iter()
            .map(|(slot, path, cal, off)| {
                read_stimulus_calibrated(&path, cal).map(|s| (slot, s, off))
            })
            .collect::<Result<_, _>>()?;
        let lvl_entries: Vec<leveller::SetlistEntry> = stims
            .iter()
            .map(|(slot, s, off)| leveller::SetlistEntry {
                slot: *slot,
                stimulus: s,
                offset_lu: *off,
            })
            .collect();
        let result = leveller::level_setlist(&lvl_entries, SETLIST_HEADROOM_LU, 0.5, save);
        leveller::reamp_off_guaranteed("level_setlist");
        result
    })
    .await
}
/// One already-measured ceiling from a finished run, for [`common_reachable_target`]: the
/// sound's raw ceiling `c_lufs` + the topology that decides its playback offset.
#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct CeilingArg {
    c_lufs: f64,
    topology_id: Option<String>,
}

/// Derive the reachable common target for a finished run's already-measured ceilings —
/// `min(C − offset) − headroom`, the quiet-preset clamp fallback (PR6). PURE (no device I/O,
/// no seize): reuses the run's measured `C` values, so nothing is re-captured. Each ceiling's
/// per-instrument playback offset is resolved via [`playback_offset_for`] (single source of
/// truth for the Fletcher–Munson offset), then the target is solved in offset-adjusted space
/// (same as `level_setlist`). The frontend re-levels every sound to this target; the runners
/// add each offset back. Errors when no ceiling is finite (an all-silent run has none).
#[tauri::command]
pub(crate) async fn common_reachable_target<R: tauri::Runtime>(
    app: tauri::AppHandle<R>,
    ceilings: Vec<CeilingArg>,
) -> Result<f64, String> {
    let pairs: Vec<(f64, f64)> = ceilings
        .iter()
        .map(|c| {
            (
                c.c_lufs,
                playback_offset_for(&app, c.topology_id.as_deref()),
            )
        })
        .collect();
    leveller::common_reachable_target(&pairs, SETLIST_HEADROOM_LU)
        .ok_or_else(|| "no reachable ceilings to derive a common target from".to_string())
}

/// Measure each scene's ceiling loudness (re-amp + `loadScene` per scene)
/// and return the per-scene gain offsets to a common target (MEASURE — drives the
/// device; HW-pending). Supersedes hand-entered C values when hardware is present.
#[tauri::command]
pub(crate) async fn level_scenes(
    app: tauri::AppHandle,
    slot: u32,
    scene_count: u32,
    topology_id: Option<String>,
    headroom_lu: f64,
    state: State<'_, AppState>,
) -> Result<Vec<f64>, String> {
    let stim_path = resolve_stimulus(&app, None, topology_id)?;
    with_released_seize(state.session.clone(), move || {
        let stim = read_stimulus_calibrated(&stim_path, None)?;
        let cs = leveller::capture_scene_ceilings(slot, scene_count, &stim)?;
        scenes::normalize_scene_targets(&cs, headroom_lu)
            .ok_or_else(|| "no finite scene loudness measured".to_string())
    })
    .await
}

#[cfg(test)]
mod scene_handle_tests {
    use super::*;

    /// A 2-scene preset: an amp whose scene-0 overlay carries a KNOB (`Full`) and whose
    /// scene-1 overlay carries only `bypass` (`BypassOnly` — Scene Edit off, knobs shared
    /// with base), plus a pedal no scene mentions at all (`Absent`).
    fn preset() -> serde_json::Value {
        serde_json::json!({
            "audioGraph": { "guitarNodes": { "G1": [
                { "nodeId": "amp", "FenderId": "ACD_TwinReverb65NoFx",
                  "dspUnitParameters": { "outputLevel": 0.5, "volume": 0.7, "bypass": false } },
                { "nodeId": "ped", "FenderId": "ACD_KingOfTone",
                  "dspUnitParameters": { "volume": 1.0, "overdrive": 0.4, "bypass": false } }
            ] } },
            "scenes": [
                { "guitarNodes": { "G1": {
                    "ACD_TwinReverb65NoFx": { "dspUnitParameters": { "outputLevel": 0.3 } } } } },
                { "guitarNodes": { "G1": {
                    "ACD_TwinReverb65NoFx": { "dspUnitParameters": { "bypass": true } } } } }
            ]
        })
    }

    /// The batch's amp candidate — an `outputLevel` on the fixture's amp, so a HANDLE-less
    /// row has something to classify (the fixture carries no `audioGraph.template`, so the
    /// amp path still fails its routing prerequisite; that is the mixed-batch gate below).
    fn amp_candidates() -> Vec<LevelBlockArg> {
        vec![LevelBlockArg {
            group_id: "G1".into(),
            node_id: "amp".into(),
            parameter_id: "outputLevel".into(),
            value: 0.5,
        }]
    }

    /// Build the batch's jobs the way `level_scenes_apply_batched` does: the handles threaded
    /// INTO the builder, sparse and keyed by wire scene slot.
    fn build(
        slots: &[u32],
        handles: &[(u32, (&str, &str, &str))],
        docs: &[(u32, Option<serde_json::Value>)],
        saved: Option<&serde_json::Value>,
    ) -> Result<Vec<leveller::SceneJob>, String> {
        let specs: Vec<(u32, SceneHandleSpec)> = handles
            .iter()
            .map(|(scene, (g, n, p))| {
                (
                    *scene,
                    SceneHandleSpec {
                        group_id: g,
                        node_id: n,
                        parameter_id: p,
                    },
                )
            })
            .collect();
        build_scene_jobs_with_handles(slots, &amp_candidates(), docs, -23.0, saved, &specs)
    }

    // A handle points the row at the user's control, and its starting value / wet-floor
    // anchor is the value authored IN THAT SCENE (the pedal has no scene-0 overlay, so it
    // inherits base — the amp would have taken its overlay's 0.3 instead).
    #[test]
    fn a_handle_repoints_the_row_and_takes_the_scenes_own_value() {
        let preset = preset();
        let docs = vec![(0u32, Some(preset.clone()))];
        let jobs = build(&[0], &[(0, ("G1", "ped", "volume"))], &docs, Some(&preset))
            .expect("an all-handles batch needs no amp prerequisite");
        let sj = &jobs[0];
        assert!(sj.skip.is_none());
        assert!(sj.handle.is_some(), "the row is handle-driven");
        assert!(
            !sj.rebalanceable,
            "one user-chosen control is not a rebalanceable lane pair"
        );
        match &sj.knobs[..] {
            [k] => {
                assert_eq!(k.current, 1.0, "the pedal's authored volume");
                assert_eq!((k.lo, k.hi), (0.0, 1.0));
                assert_eq!(
                    k.knob.label(),
                    "G1/ped/volume@scene0",
                    "the write is scene-scoped"
                );
            }
            n => panic!("expected exactly one handle knob, got {}", n.len()),
        }
    }

    // An unrecognised control is refused BEFORE any device work, as that ROW's skip — the
    // rest of the batch must still run (the lane's own per-scene-skip rule).
    #[test]
    fn an_unclassifiable_handle_skips_only_its_own_row() {
        let preset = preset();
        let docs = vec![(0u32, Some(preset.clone())), (1u32, Some(preset.clone()))];
        let jobs = build(
            &[0, 1],
            &[
                // `overdrive` is a drive control, not a level control.
                (0, ("G1", "ped", "overdrive")),
                (1, ("G1", "ped", "volume")),
            ],
            &docs,
            Some(&preset),
        )
        .expect("one bad handle is a row skip, never a batch abort");
        let reason = jobs[0].skip.as_deref().expect("row 0 refused");
        assert!(
            reason.contains("not a level control"),
            "the shared refusal wording: {reason}"
        );
        assert!(jobs[0].knobs.is_empty(), "a refused row drives nothing");
        assert!(jobs[1].skip.is_none(), "row 1 is unaffected");
    }

    // Without the saved document there is no FenderId to classify against — refuse the row
    // rather than sweep an unclassified control.
    #[test]
    fn a_handle_without_a_saved_read_is_refused() {
        let jobs = build(&[0], &[(0, ("G1", "ped", "volume"))], &[], None)
            .expect("still a row skip, not a batch abort");
        assert!(jobs[0].skip.is_some());
        assert!(jobs[0].knobs.is_empty());
    }

    // BUG→GATE (the mixed-batch class): the amp prerequisites — an `outputLevel` candidate
    // and a readable routing template — are inputs a HANDLE row does not need. The fixture
    // carries no `audioGraph.template`, so the amp classifier fails preset-wide; that must
    // skip only the row that needed the amp, never the row whose control the user named.
    #[test]
    fn an_amp_prerequisite_failure_skips_only_the_rows_that_need_the_amp() {
        let preset = preset();
        let docs = vec![(0u32, Some(preset.clone())), (1u32, Some(preset.clone()))];
        let jobs = build(
            &[0, 1],
            &[(1, ("G1", "ped", "volume"))],
            &docs,
            Some(&preset),
        )
        .expect("a mixed batch must not abort on the amp classifier");
        assert!(
            jobs[0].skip.as_deref().unwrap_or("").contains("routing"),
            "the amp row reports the routing read: {:?}",
            jobs[0].skip
        );
        assert!(jobs[1].skip.is_none(), "the handle row levels regardless");
        assert!(jobs[1].handle.is_some());
    }

    // The picker's two annotations, both read straight off the saved overlays.
    #[test]
    fn handle_rows_annotate_scope_and_headroom_per_scene() {
        let rows = scene_handle_rows(&preset());
        assert_eq!(
            rows.len(),
            2,
            "one row per FS scene (base is not enumerated)"
        );
        let find = |row: &SceneHandleRow, node: &str, param: &str| {
            row.candidates
                .iter()
                .find(|c| c.node_id == node && c.parameter_id == param)
                .cloned()
        };

        // Scene 0: the amp's overlay carries a knob → the write stays in this scene, and
        // the overlay's own 0.3 is the current value (not base's 0.5).
        let amp0 = find(&rows[0], "amp", "outputLevel").expect("amp outputLevel");
        assert_eq!(amp0.scope, "isolated");
        assert_eq!(amp0.current, 0.3);
        assert_eq!(amp0.class, param_class::ParamClass::LevelLinear);
        assert_eq!(
            serde_json::to_value(amp0.class).expect("class serializes"),
            "level_linear",
            "the wire spelling the frontend reads is the table's own"
        );
        assert_eq!(amp0.range, [0.0, 1.0]);
        assert_eq!(amp0.headroom, "full");

        // Scene 1: bypass-only overlay means Scene Edit is OFF, so the knobs are SHARED
        // with base (and `set_knobs` refuses the write) — the picker must say so.
        let amp1 = find(&rows[1], "amp", "outputLevel").expect("amp outputLevel");
        assert_eq!(amp1.scope, "shared_with_base");
        assert_eq!(amp1.current, 0.5, "shared, so it reads the BASE value");

        // No overlay at all: the Scene Edit enable materialises one, so still isolated; and
        // a control authored at the top of its range can only go DOWN.
        let ped = find(&rows[0], "ped", "volume").expect("pedal volume");
        assert_eq!(ped.scope, "isolated");
        assert_eq!(ped.headroom, "lowers_only");

        // The classifier's bars hold here too: an AMP's `volume` is the breakup knob, and a
        // pedal's `overdrive` is a drive control — neither is ever offered.
        assert!(find(&rows[0], "amp", "volume").is_none());
        assert!(find(&rows[0], "ped", "overdrive").is_none());
    }
}
