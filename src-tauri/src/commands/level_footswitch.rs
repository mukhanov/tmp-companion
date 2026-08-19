//! Footswitch (engaged-state) leveling command + job resolution.
#![allow(clippy::too_many_arguments)]
use crate::*;

// ───────────────────────── Footswitch (engaged-state) leveling ─────────────────────────

/// One footswitch-leveling request: level switch `switch`'s engaged state by solving the
/// `(lev_group_id, lev_node_id, lev_parameter_id)` param to hit `target_lufs`.
#[derive(serde::Deserialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
pub(crate) struct FootswitchLevelJob {
    pub(crate) switch: u32,
    /// The leveling handle. Defaulted (empty) so a malformed payload is a clean per-row
    /// error rather than a solve against node "".
    #[serde(default)]
    pub(crate) lev_group_id: String,
    #[serde(default)]
    pub(crate) lev_node_id: String,
    #[serde(default)]
    pub(crate) lev_parameter_id: String,
    /// This row's OWN loudness target. Per-row (not one batch target) so a preset with a
    /// mix of targets levels in ONE batch — one prepass, one runner, one deferred save.
    pub(crate) target_lufs: f64,
    /// The switch's CURRENT display label as the UI shows it (the Level list's footswitch row
    /// name: the player's `customLabel`, else the toggled block's friendly name). Used ONLY
    /// when the assign path appends a second function to a switch whose `customLabel` is empty
    /// — the unit displays "MULTI" for a multi-function switch with no label, so writing the
    /// prior display name keeps the pedalboard reading the same. Absent → today's behavior.
    #[serde(default)]
    pub(crate) display_label: Option<String>,
    /// THE SCENE CONTEXT this switch's sound is measured and solved in (D3): a 0-based
    /// `scenes[]` wire slot, or `None` = the preset's BASE sound (the historical behaviour and
    /// the serde default, so an existing payload with no `sceneContext` key is unchanged).
    ///
    /// A footswitch does not sound the same in every scene — the scene's overlay decides which
    /// blocks the switch is layered on top of, and (for the headroom trade) whether the sound
    /// is pinned by its own `outputLevel` overlay or inherits base's. The UI picks this with
    /// [`footswitch::scene_contexts_for_switches`]: exactly ONE scene enabling the switch
    /// preselects that scene,
    /// anything else falls back to base. A user override to a NON-enabling scene is allowed —
    /// it is a real sound, just not one the pedalboard reaches by tapping that switch there —
    /// and the picker flags it.
    #[serde(default)]
    pub(crate) scene_context: Option<u32>,
}

#[derive(Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct FootswitchLevelProgressItem {
    switch: u32,
    status: String, // active | done | error | cancelled
    result: Option<leveller::FootswitchLevelResult>,
    message: Option<String>,
}

static FOOTSWITCH_LEVEL_CANCEL: AtomicBool = AtomicBool::new(false);

#[tauri::command]
pub(crate) fn cancel_footswitch_leveling() {
    FOOTSWITCH_LEVEL_CANCEL.store(true, SeqCst);
    // Also wake the in-flight capture/settle waits (see `device_gate::OP_ABORT`).
    crate::request_op_abort();
}

/// A numeric `dspUnitParameter` of `node_id` (e.g. the lev param's current value = `valueB`).
pub(crate) fn node_param_f64(
    preset: &serde_json::Value,
    node_id: &str,
    param: &str,
) -> Option<f64> {
    let mut found = None;
    audiograph::for_each_node(preset, |obj| {
        if obj.get("nodeId").and_then(|v| v.as_str()) == Some(node_id) {
            found = obj
                .get("dspUnitParameters")
                .and_then(|p| p.get(param))
                .and_then(|v| v.as_f64());
        }
    });
    found
}

/// Resolved inputs to `leveller::level_footswitch`: the switch-OFF value (`valueB` = the
/// param's current value) and the write spec.
type FootswitchJobResolution = (f32, leveller::FootswitchWriteSpec);

/// Resolve a footswitch-leveling job against the preset: the lev param's current value
/// (`valueB`) and the write spec (edit an existing matching `param` function, else add at
/// the next free index; enforce the firmware's 5-function cap). The leveler only ever
/// creates/edits a parameter-change assignment — it does not touch on/off.
pub(crate) fn resolve_footswitch_job(
    ftsw: &serde_json::Value,
    preset: &serde_json::Value,
    job: &FootswitchLevelJob,
) -> Result<FootswitchJobResolution, String> {
    let switches = ftsw.as_array().ok_or("preset has no ftsw")?;
    let sw = switches
        .get(job.switch as usize)
        .and_then(|s| s.as_array())
        .ok_or_else(|| format!("footswitch {} not found", job.switch))?;

    let value_b =
        node_param_f64(preset, &job.lev_node_id, &job.lev_parameter_id).ok_or_else(|| {
            format!(
                "parameter {} not found on {}",
                job.lev_parameter_id, job.lev_node_id
            )
        })? as f32;

    // Edit an existing param fn on (lev_node, lev_param), else add (≤5 cap).
    let existing = footswitch::existing_param_fn_index(
        ftsw,
        job.switch,
        &job.lev_node_id,
        &job.lev_parameter_id,
    )
    .and_then(|i| sw.get(i as usize).map(|a| (i, a)));
    // colorA/colorB/customLabel/linkGroup/switchType are SWITCH-level — the manual:
    // "common to all five footswitch assignments" — so both an edited existing
    // function AND a brand-new one read them the same way (an absent field on an
    // existing function falls back to the historical constants, same as a switch
    // with no sibling to inherit from). isActive is NOT switch-level (it's the
    // per-function ENGAGED state — HW round-trip: engaging an unlinked switch and
    // re-saving flipped it false→true on that one function only) — a firmware-
    // authored disengaged assignment reads false, so an absent field means
    // disengaged, never inherited/defaulted to engaged.
    let field_u64 = |v: Option<&serde_json::Value>, field: &str, default: u64| -> u32 {
        v.and_then(|a| a.get(field))
            .and_then(|v| v.as_u64())
            .unwrap_or(default) as u32
    };
    let field_str = |v: Option<&serde_json::Value>, field: &str| -> String {
        v.and_then(|a| a.get(field))
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string()
    };
    let spec = match existing {
        Some((i, a)) => leveller::FootswitchWriteSpec {
            function_index: i,
            color_a: field_u64(Some(a), "colorA", 3),
            color_b: field_u64(Some(a), "colorB", 0),
            custom_label: field_str(Some(a), "customLabel"),
            link_group: field_u64(Some(a), "linkGroup", 0),
            is_active: a.get("isActive").and_then(|v| v.as_bool()).unwrap_or(false),
            switch_type: field_u64(Some(a), "switchType", 0),
        },
        None => {
            if sw.len() >= 5 {
                return Err(format!(
                    "footswitch {} is full (5 functions) — no room to add a leveling param",
                    job.switch
                ));
            }
            // A new assignment must INHERIT the switch-level fields from an existing
            // sibling function (`sw.first()`), not hardcode defaults: a linked
            // on-off's linkGroup would otherwise drop the switch out of its Switch
            // Link, and colour/label would silently diverge from the switch's other
            // assignments (Fender's own CloudPresets multi-function switches keep
            // all five fields identical across every assignment). Falls back to the
            // historical constants only when the switch has no existing function.
            let sibling = sw.first();
            // A second function on an UNLABELLED switch makes the unit display "MULTI" instead
            // of the single function's implied name. Nothing else changes the label: an already
            // labelled switch keeps the player's own text (inherited), and the first function on
            // an empty switch shows its own name, so there is nothing to preserve there.
            let inherited = field_str(sibling, "customLabel");
            let custom_label = match (inherited.is_empty(), sibling, &job.display_label) {
                (true, Some(_), Some(shown)) => shown.clone(),
                _ => inherited,
            };
            leveller::FootswitchWriteSpec {
                function_index: sw.len() as u32,
                color_a: field_u64(sibling, "colorA", 3),
                color_b: field_u64(sibling, "colorB", 0),
                custom_label,
                link_group: field_u64(sibling, "linkGroup", 0),
                is_active: false,
                switch_type: field_u64(sibling, "switchType", 0),
            }
        }
    };
    Ok((value_b, spec))
}

/// Which scenes enable each footswitch of `slot`, for the leveling wizard's SCENE-CONTEXT
/// picker (D3). PURE apart from ONE field-8 read: every answer comes out of the saved document,
/// so no scene is recalled on the unit and nothing is measured.
///
/// The frontend preselects [`footswitch::FsSceneContext::suggested`] — the single enabling
/// scene when there is exactly one, else base — and sends the user's final choice back as
/// [`FootswitchLevelJob::scene_context`].
#[tauri::command]
pub(crate) async fn list_footswitch_scene_contexts(
    state: State<'_, AppState>,
    slot: u32,
) -> Result<Vec<footswitch::FsSceneContext>, String> {
    with_released_seize(state.session.clone(), move || {
        // Every row of the picker comes out of `ftsw`, so a body that lost it would show
        // the player a switch-less preset rather than an error.
        let (preset, _, _) = read_slot_preset_complete(slot, &["ftsw"])?;
        Ok(footswitch::scene_contexts_for_switches(&preset))
    })
    .await
}

/// Level one or more block-acting footswitches of preset `slot`, streaming a progress item
/// per switch. Each switch's engaged state is measured/solved independently against the
/// base preset; jobs run sequentially. Mirrors `level_scenes_apply_batched`.
#[tauri::command]
#[allow(clippy::too_many_arguments)]
pub(crate) async fn level_footswitches_apply<R: tauri::Runtime>(
    app: tauri::AppHandle<R>,
    state: State<'_, AppState>,
    slot: u32,
    jobs: Vec<FootswitchLevelJob>,
    save: bool,
    topology_id: Option<String>,
    calibration_lufs: Option<f32>,
    profile_id: Option<String>,
    on_result: tauri::ipc::Channel<FootswitchLevelProgressItem>,
) -> Result<Vec<leveller::FootswitchLevelResult>, String> {
    let (stim_path, calibration_lufs) = resolve_stimulus_for_leveling(
        &app,
        None,
        topology_id.clone(),
        profile_id.as_deref(),
        calibration_lufs,
    )?;
    let stim = read_stimulus_calibrated(&stim_path, calibration_lufs)?;
    let offset = playback_offset_for(&app, topology_id.as_deref());
    FOOTSWITCH_LEVEL_CANCEL.store(false, SeqCst);
    let app_evt = app.clone();

    with_released_seize(state.session.clone(), move || {
        // Stream advisory live LUFS while each capture runs (dropped at closure end).
        let _lufs = LiveLufsGuard::install(app_evt);
        // THE single field-8 read: it resolves every job AND is the planner's scene-overlay
        // source (per-node bake gate) — never add a second read here. `ftsw` is REQUIRED:
        // a body cut before it plans zero jobs and reports a clean run on a preset whose
        // switches were never touched, so the read either delivers it or the run refuses
        // (before any device state moves).
        let (preset, _, _) = read_slot_preset_complete(slot, &["ftsw"])?;
        crate::settle(std::time::Duration::from_millis(leveller::RECONNECT_GAP_MS));
        let ftsw = preset
            .get("ftsw")
            .cloned()
            .unwrap_or(serde_json::Value::Null);

        // Plan bake-vs-assign for the whole batch (pure) — block-off-in-base + sole-owner + no
        // scene overlay on THAT node's bypass ⇒ bake straight onto the block (no `ftsw` write, so
        // the switch keeps its single function and its label); otherwise the param assignment.
        // PLAN SPACE IS JOB SPACE: every row is a level row (the verify-mode filter that used
        // to thin this list is gone), so `plan_footswitch_jobs` answers one plan per job in
        // job order and `BakeShared.rep` is already a `results`/`plans` index — nothing to
        // realign.
        let keys: Vec<footswitch::FsJobKey> = jobs
            .iter()
            .map(|j| footswitch::FsJobKey {
                switch: j.switch,
                lev_node: &j.lev_node_id,
                lev_param: &j.lev_parameter_id,
                target_bits: j.target_lufs.to_bits(),
            })
            .collect();
        let plans: Vec<footswitch::FsLevelPlan> =
            footswitch::plan_footswitch_jobs(&ftsw, &preset, &keys);

        // Freshness barrier: a same-slot batch load starting shortly after this preset's own
        // earlier save (base level, a prior FS batch, …) could otherwise materialize the
        // PRE-save preset — the incident this whole fix exists for. Tell the wizard WHY
        // nothing moves for up to ~2 min before paying the wait, since `ensure_fresh_load`
        // gates silently otherwise.
        if leveller::slot_save_pending_commit(slot) {
            if let Some(first) = jobs.first() {
                let _ = on_result.send(FootswitchLevelProgressItem {
                    switch: first.switch,
                    status: "active".into(),
                    result: None,
                    message: Some("waiting for the device to commit the previous save…".into()),
                });
            }
        }
        leveller::ensure_fresh_load(slot, &mut || crate::op_aborted())?;
        // Load the preset ONCE for the whole batch — `measure_footswitch`'s caller
        // contract. Every job's sweep runs against this load (its pollution is
        // self-correcting: each job's force list explicitly sets every sibling
        // block's bypass, and swept params live on blocks the next job forces off);
        // the ONE write session's reload discards it all at the end.
        {
            let mut s = Session::connect_lean()?;
            s.load_preset(slot)?;
            crate::settle(std::time::Duration::from_millis(
                leveller::settle_after_load_ms(),
            ));
        }
        crate::settle(std::time::Duration::from_millis(leveller::RECONNECT_GAP_MS));

        let mut results: Vec<Option<leveller::FootswitchLevelResult>> = vec![None; jobs.len()];
        // The solved writes pending the batch's single write+save session, each
        // carrying its job index (one vec — no hand-aligned parallel arrays).
        let mut pending: Vec<(usize, leveller::FsPendingWrite)> = Vec::new();
        // ── THE REORDERED RUN, FS HALF ────────────────────────────────────────────────
        // PHASE 1: read every row's CEILING (its engaged sound with the leveling handle
        // pinned at the top of its range) BEFORE any solve. One engage per row — a
        // MEASUREMENT, never an extrapolation, because an arbitrary block param has no
        // algebraically predictable response. A row whose target sits clearly above its own
        // ceiling is settled here: it clamps honestly and its (up to `FS_CORRECT_MAX`)
        // secant captures are never paid.
        //
        // A `BakeShared` sibling pays NOTHING here. Its plan says its sound is the SAME sound
        // as `rep`'s — same node, same param, same target (`FsJobKey`), which is the very
        // premise phase 2 already relies on when it copies `results[rep]` verbatim — so its
        // ceiling is `rep`'s ceiling, and `rep` is always a LOWER index (the planner picks the
        // first row of the group as the representative), so the entry is already in hand.
        // Saves one engage+capture per sibling, on a device where that is ~10 s.
        let mut ceilings: Vec<Option<leveller::FsCeiling>> = Vec::with_capacity(jobs.len());
        for (idx, job) in jobs.iter().enumerate() {
            let ceiling = (|| {
                if let Some(footswitch::FsLevelPlan::BakeShared { rep }) = plans.get(idx) {
                    return ceilings.get(*rep).copied().flatten();
                }
                if FOOTSWITCH_LEVEL_CANCEL.load(SeqCst)
                    || job.lev_group_id.is_empty()
                    || job.lev_node_id.is_empty()
                    || job.lev_parameter_id.is_empty()
                {
                    return None;
                }
                let handle = leveller::FsParamTarget::from_preset(
                    &preset,
                    &job.lev_node_id,
                    &job.lev_parameter_id,
                );
                // An unrecognised control refuses at the solve; measuring its ceiling first
                // would burn a capture to reach the same refusal.
                if handle.refuse_if_not_a_level_control().is_some() {
                    return None;
                }
                let states = footswitch::switch_states(&ftsw, &preset, job.switch);
                let probe = leveller::FsCeilingProbe {
                    // THE ROW'S SCENE CONTEXT (D3). `None` = base — the historical default,
                    // the sound the isolation list describes on its own. A `Some(i)` row
                    // recalls scene `i` before engaging, so the ceiling read describes the
                    // switch AS IT SOUNDS IN THAT SCENE — the same context its solve below
                    // measures in, or the two would describe different sounds.
                    scene: job.scene_context,
                    states: &states,
                    handle: (
                        job.lev_group_id.clone(),
                        job.lev_node_id.clone(),
                        handle.clone(),
                    ),
                };
                // No intended `presetLevel`: this command owns no solved or unsaved level of
                // its own (it never writes `presetLevel`), so every capture renders at the
                // level the preset holds — which the `ensure_fresh_load` barrier above has
                // already waited for a pending same-slot save to commit.
                match leveller::measure_fs_ceiling(&probe, &stim, None) {
                    Ok(l) => {
                        let ceiling_lufs = l.integrated_lufs;
                        let target = job.target_lufs + offset;
                        let unreachable =
                            leveller::fs_target_beyond_ceiling(ceiling_lufs, target);
                        log::info!(
                            "fs prepass switch={} ceiling={ceiling_lufs:.2} LUFS target={target:.2} \
                             unreachable={unreachable}",
                            job.switch
                        );
                        Some(leveller::FsCeiling {
                            ceiling_lufs,
                            spread_lu: l.spread_lu(),
                            unreachable,
                        })
                    }
                    // A failed ceiling read is NOT a failed row: the solve takes over and
                    // reports whatever it finds, exactly as it did before this phase existed.
                    Err(e) => {
                        log::warn!(
                            "fs prepass ceiling for switch {} failed ({e}); its solve will decide",
                            job.switch
                        );
                        None
                    }
                }
            })();
            ceilings.push(ceiling);
        }
        // Guaranteed fresh re-amp OFF after the measurement phase — each capture disengages
        // itself, but an interrupted one can strand the unit input-muted (`danger.md`).
        leveller::reamp_off_guaranteed("fs_prepass_ceilings");

        // PHASE 2: solve + collect the writes (the writes themselves still ride the single
        // live-edit session at the end, unchanged).
        for (idx, job) in jobs.iter().enumerate() {
            if FOOTSWITCH_LEVEL_CANCEL.load(SeqCst) {
                let _ = on_result.send(FootswitchLevelProgressItem {
                    switch: job.switch,
                    status: "cancelled".into(),
                    result: None,
                    message: None,
                });
                break;
            }
            let _ = on_result.send(FootswitchLevelProgressItem {
                switch: job.switch,
                status: "active".into(),
                result: None,
                message: None,
            });
            let lev = (
                job.lev_group_id.as_str(),
                job.lev_node_id.as_str(),
                job.lev_parameter_id.as_str(),
            );
            // The CLASSIFIED solve target, off the batch's single field-8 read: it carries
            // the param's class (an `Other` refuses before any device work), its real range
            // (no `[0,1]` assumption) and its authored value (the wet-mix floor anchor).
            let lev_param = leveller::FsParamTarget::from_preset(
                &preset,
                &job.lev_node_id,
                &job.lev_parameter_id,
            );
            let lev_owned = || {
                (
                    job.lev_group_id.clone(),
                    job.lev_node_id.clone(),
                    job.lev_parameter_id.clone(),
                )
            };
            let plan = plans.get(idx);
            // PREPASS VERDICT FIRST: the ceiling read already proved this row cannot reach
            // its target with the handle at the top, so the solve has nothing to find. Report
            // the honest clamp at the loudest the sound can actually be, write nothing, and
            // keep the row's authored value exactly as the player wrote it.
            let unreachable = ceilings[idx].filter(|c| c.unreachable);
            let outcome: Result<leveller::FootswitchLevelResult, String> = match plan {
                _ if unreachable.is_some() => {
                    let c = unreachable.expect("checked by the guard");
                    Ok(leveller::fs_result_from_ceiling(
                        job.switch,
                        job.target_lufs + offset,
                        &lev_param,
                        &c,
                        // The method is what the row WOULD have used; nothing was written.
                        match plan {
                            Some(footswitch::FsLevelPlan::Bake { .. })
                            | Some(footswitch::FsLevelPlan::BakeShared { .. }) => "baked",
                            _ => "assigned",
                        },
                    ))
                }
                // A row's handle is what the whole solve addresses; an empty coordinate
                // would sweep node "" and report a number for nothing. (The verify-only row
                // that used to legitimately carry no handle is gone — every row levels.)
                _ if job.lev_group_id.is_empty()
                    || job.lev_node_id.is_empty()
                    || job.lev_parameter_id.is_empty() =>
                {
                    Err("no leveling parameter chosen for this footswitch".to_string())
                }
                // Unreachable by construction (`plans[idx]` is `Some` for every row); an
                // honest per-row error beats a panic inside a device run.
                None => Err("internal: no plan built for this footswitch row".to_string()),
                Some(footswitch::FsLevelPlan::Clamp(msg)) => Err(msg.clone()),
                // A sibling switch already baked this (node, param, target) — reuse its
                // result. `rep` is a JOB index (translated at alignment), so it indexes
                // `results` directly.
                Some(footswitch::FsLevelPlan::BakeShared { rep }) => results[*rep]
                    .clone()
                    .map(|mut r| {
                        r.switch = job.switch;
                        r
                    })
                    .ok_or_else(|| "shared bake produced no result".to_string()),
                // Bake has no cheap re-run marker (the block's param value is `Some` from the
                // factory too), so it always solves — no idempotency probe (`current` = None).
                Some(footswitch::FsLevelPlan::Bake {
                    engaged,
                    clear_stale,
                    mirror_scenes,
                }) => leveller::measure_footswitch(
                    job.switch,
                    job.scene_context,
                    lev,
                    engaged,
                    &stim,
                    job.target_lufs + offset,
                    "baked",
                    None,
                    &lev_param,
                    // See the prepass above: this lane holds no `presetLevel` of its own.
                    None,
                )
                .inspect(|r| {
                    if save && r.clamp_reason.is_none() {
                        pending.push((
                            idx,
                            leveller::FsPendingWrite {
                                switch: job.switch,
                                lev: lev_owned(),
                                write: leveller::FsWrite::Bake {
                                    clear_stale: *clear_stale,
                                    mirror_scenes: mirror_scenes.clone(),
                                },
                                value: r.final_value,
                            },
                        ));
                    }
                }),
                Some(footswitch::FsLevelPlan::Assign { engaged }) => {
                    match resolve_footswitch_job(&ftsw, &preset, job) {
                        Err(e) => Err(e),
                        Ok((value_b, spec)) => {
                            // Re-run anchor: a prior assign's stored valueA (None = fresh).
                            // Also the wet-floor anchor — the solve raises `lev_param`'s
                            // anchor to this ENGAGED value itself (`FsParamTarget::anchored`).
                            let current = footswitch::existing_param_fn_value_a(
                                &ftsw,
                                job.switch,
                                &job.lev_node_id,
                                &job.lev_parameter_id,
                            )
                            .map(|v| v as f32);
                            leveller::measure_footswitch(
                                job.switch,
                                job.scene_context,
                                lev,
                                engaged,
                                &stim,
                                job.target_lufs + offset,
                                "assigned",
                                current,
                                &lev_param,
                                // See the prepass above: this lane holds no `presetLevel`
                                // of its own.
                                None,
                            )
                            // Skip the write when the leveler left the value unchanged — its
                            // `final_value == current` is the idempotency signal (no wire field).
                            .inspect(|r| {
                                if save
                                    && r.clamp_reason.is_none()
                                    && Some(r.final_value) != current
                                {
                                    pending.push((
                                        idx,
                                        leveller::FsPendingWrite {
                                            switch: job.switch,
                                            lev: lev_owned(),
                                            write: leveller::FsWrite::Assign { value_b, spec },
                                            value: r.final_value,
                                        },
                                    ));
                                }
                            })
                        }
                    }
                }
            };
            // A Stop mid-sweep surfaces as the CANCELLED sentinel, not a failure — report
            // it like the top-of-loop check would rather than as an errored switch.
            if let Err(e) = &outcome {
                if *e == leveller::CANCELLED {
                    let _ = on_result.send(FootswitchLevelProgressItem {
                        switch: job.switch,
                        status: "cancelled".into(),
                        result: None,
                        message: None,
                    });
                    break;
                }
            }
            let item = match outcome {
                Ok(r) => {
                    results[idx] = Some(r.clone());
                    FootswitchLevelProgressItem {
                        switch: job.switch,
                        status: "done".into(),
                        result: Some(r),
                        message: None,
                    }
                }
                Err(e) => FootswitchLevelProgressItem {
                    switch: job.switch,
                    status: "error".into(),
                    result: None,
                    message: Some(e),
                },
            };
            let _ = on_result.send(item);
        }
        // ── ONE write session + ONE save for every solved switch (also fired after a
        // cancel, so already-reported switches persist), then a reload to leave the
        // working copy clean. No re-measure verify capture: `predicted_lufs` is already a
        // REAL measurement at `final_value` (the sweep's best point), not a model
        // prediction — re-measuring it bought nothing but ~10 s per switch. A CHEAP
        // param-level persist verify (one field-8 read, §A4) still runs below.
        let write_result = if save && !pending.is_empty() {
            // Snapshot for the post-save persist-verify BEFORE the unzip below consumes
            // `pending` — (result idx, node, param, solved value, is-an-Assign-write).
            let verify_specs: Vec<(usize, String, String, f32, bool)> = pending
                .iter()
                .map(|(idx, w)| {
                    let is_assign = matches!(w.write, leveller::FsWrite::Assign { .. });
                    (*idx, w.lev.1.clone(), w.lev.2.clone(), w.value, is_assign)
                })
                .collect();
            // Snapshot the run's own earlier base-save expectation NOW — the write below
            // registers the batch's `Param` witness over the same slot key, after which the
            // registry can no longer answer for the base save (`registered_preset_level`).
            let base_expect = leveller::registered_preset_level(slot);
            let (idxs, writes): (Vec<usize>, Vec<leveller::FsPendingWrite>) =
                pending.into_iter().unzip();
            // Re-stamp the preset's original `lastLoadedScene`: the write session's base
            // recall (and any bake-mirror scene recalls) leave the wrong scene active, and
            // the save records the active one (HW: the FS save stamped 8 over scene 3).
            let restore = crate::last_loaded_scene(&preset);
            crate::warn_missing_restore_scene("level_footswitches", slot, &preset, restore);
            leveller::write_footswitch_values(slot, &writes, restore).map(|()| {
                let written: std::collections::HashSet<usize> = idxs.iter().copied().collect();
                for &idx in &idxs {
                    if let Some(r) = &mut results[idx] {
                        r.saved = true;
                    }
                }
                // §A4: one field-8 read, stamping `persist_mismatch` per switch — the FS-lane
                // mirror of the scene lane's `verify_persisted_writes`.
                leveller::verify_fs_persisted_writes(
                    slot,
                    &verify_specs,
                    base_expect,
                    &mut results,
                );
                // Propagate the persisted state (saved + persist_mismatch) to BakeShared
                // siblings that reused a now-saved representative's result (they share the
                // same written write). `plans`, `written` and `results` are all JOB-indexed
                // and `rep` was translated at alignment, so no remap happens here.
                for (idx, plan) in plans.iter().enumerate() {
                    if let footswitch::FsLevelPlan::BakeShared { rep } = plan {
                        if written.contains(rep) {
                            let rep_mismatch =
                                results[*rep].as_ref().and_then(|r| r.persist_mismatch);
                            if let Some(r) = &mut results[idx] {
                                r.saved = true;
                                r.persist_mismatch = rep_mismatch;
                            }
                        }
                    }
                }
            })
        } else {
            // Dry run / nothing solved: discard the sweep pollution.
            let _ = Session::connect_lean().map(|mut s| s.load_preset(slot));
            Ok(())
        };
        // Guarantee re-amp OFF on a fresh connection.
        if let Ok(mut s) = Session::connect_lean() {
            let _ = s.set_reamp_mode(false);
        }
        write_result?;
        Ok(results.into_iter().flatten().collect())
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    fn preset_with_lev_param(value: f64) -> serde_json::Value {
        serde_json::json!({
            "audioGraph": {
                "template": "gtrSeries",
                "guitarNodes": {
                    "G1": [
                        { "FenderId": "amp", "nodeId": "amp",
                          "dspUnitParameters": { "drive": value } }
                    ]
                }
            }
        })
    }

    fn job() -> FootswitchLevelJob {
        FootswitchLevelJob {
            switch: 0,
            lev_group_id: "G1".into(),
            lev_node_id: "amp".into(),
            lev_parameter_id: "drive".into(),
            target_lufs: -20.0,
            display_label: None,
            scene_context: None,
        }
    }

    // PER-ROW TARGET is the wire contract: each row carries its OWN `targetLufs`, so one
    // batch can level a preset whose rows ask for different loudnesses (one prepass, one
    // runner, one deferred save). The verify-only `mode` key is GONE — every row levels.
    #[test]
    fn a_job_carries_its_own_target_and_handle() {
        let job: FootswitchLevelJob = serde_json::from_value(serde_json::json!({
            "switch": 0,
            "levGroupId": "G1",
            "levNodeId": "amp",
            "levParameterId": "drive",
            "targetLufs": -20.0
        }))
        .expect("deserialize");
        assert_eq!(job.target_lufs, -20.0);
        assert_eq!(job.lev_parameter_id, "drive");
    }

    // A payload from an older frontend still carrying `mode` must not fail to deserialize —
    // the key is simply ignored and the row levels like any other.
    #[test]
    fn a_legacy_mode_key_is_ignored_rather_than_rejected() {
        let job: FootswitchLevelJob = serde_json::from_value(serde_json::json!({
            "switch": 3,
            "targetLufs": -23.0,
            "mode": "verify"
        }))
        .expect("deserialize");
        assert!(job.lev_group_id.is_empty());
        assert!(job.lev_node_id.is_empty());
        assert!(job.lev_parameter_id.is_empty());
    }

    // (A4) A NEW assignment on a switch that already has a function must inherit
    // that sibling's switch-level fields (colour/label/link/switchType) rather
    // than hardcode defaults — a linked on-off's linkGroup would otherwise drop
    // the switch out of its Switch Link, and colour/label would silently diverge.
    #[test]
    fn new_assignment_inherits_sibling_switch_level_fields() {
        let ftsw = serde_json::json!([
            [
                {
                    "func": "on-off",
                    "colorA": 1, "colorB": 2,
                    "customLabel": "BOOST",
                    "linkGroup": 1,
                    "isActive": true,
                    "switchType": 1
                }
            ]
        ]);
        let preset = preset_with_lev_param(0.5);
        let (_, spec) = resolve_footswitch_job(&ftsw, &preset, &job()).expect("resolve");
        assert_eq!(
            spec.function_index, 1,
            "appended after the existing on-off fn"
        );
        assert_eq!(spec.color_a, 1, "colorA inherited from the sibling");
        assert_eq!(spec.color_b, 2, "colorB inherited from the sibling");
        assert_eq!(
            spec.custom_label, "BOOST",
            "customLabel inherited from the sibling"
        );
        assert_eq!(
            spec.link_group, 1,
            "linkGroup inherited — else the switch drops out of its Switch Link"
        );
        assert_eq!(spec.switch_type, 1, "switchType inherited from the sibling");
        assert!(
            !spec.is_active,
            "isActive is per-function engaged state, NOT inherited — a fresh assignment starts disengaged"
        );
    }

    // An empty switch (no existing function to inherit from) falls back to the
    // historical constants — there is nothing to inherit.
    #[test]
    fn new_assignment_on_empty_switch_falls_back_to_defaults() {
        let ftsw = serde_json::json!([[]]);
        let preset = preset_with_lev_param(0.5);
        let (_, spec) = resolve_footswitch_job(&ftsw, &preset, &job()).expect("resolve");
        assert_eq!(spec.function_index, 0);
        assert_eq!(spec.color_a, 3);
        assert_eq!(spec.color_b, 0);
        assert_eq!(spec.custom_label, "");
        assert_eq!(spec.link_group, 0);
        assert_eq!(spec.switch_type, 0);
        assert!(!spec.is_active);
    }

    // BUG→GATE (the "MULTI" class): adding a SECOND function to a switch whose `customLabel`
    // is empty makes the unit stop showing the single function's implied name and display
    // "MULTI" instead. The UI's own row label rides along on the job, so write it as the
    // switch's `customLabel` and the pedalboard display doesn't change.
    #[test]
    fn new_assignment_labels_an_unlabelled_switch_with_the_ui_display_label() {
        let ftsw = serde_json::json!([[{ "func": "on-off", "customLabel": "" }]]);
        let preset = preset_with_lev_param(0.5);
        let job = FootswitchLevelJob {
            display_label: Some("Mythic Drive".into()),
            ..job()
        };
        let (_, spec) = resolve_footswitch_job(&ftsw, &preset, &job).expect("resolve");
        assert_eq!(spec.function_index, 1, "a SECOND function is being added");
        assert_eq!(
            spec.custom_label, "Mythic Drive",
            "the switch's prior display label is preserved as its customLabel"
        );
    }

    // The player's own label is never touched — inheritance still wins.
    #[test]
    fn new_assignment_keeps_a_non_empty_sibling_label() {
        let ftsw = serde_json::json!([[{ "func": "on-off", "customLabel": "BOOST" }]]);
        let preset = preset_with_lev_param(0.5);
        let job = FootswitchLevelJob {
            display_label: Some("Mythic Drive".into()),
            ..job()
        };
        let (_, spec) = resolve_footswitch_job(&ftsw, &preset, &job).expect("resolve");
        assert_eq!(spec.custom_label, "BOOST");
    }

    // A switch with NO existing function gets a single function — the unit shows that
    // function's own name, never "MULTI", so there is nothing to preserve.
    #[test]
    fn first_assignment_on_an_empty_switch_is_left_unlabelled() {
        let ftsw = serde_json::json!([[]]);
        let preset = preset_with_lev_param(0.5);
        let job = FootswitchLevelJob {
            display_label: Some("Mythic Drive".into()),
            ..job()
        };
        let (_, spec) = resolve_footswitch_job(&ftsw, &preset, &job).expect("resolve");
        assert_eq!(spec.function_index, 0);
        assert_eq!(spec.custom_label, "");
    }

    // An EXISTING param assignment's isActive reads its own field verbatim — an
    // absent field means disengaged (false), not the old unwrap_or(true) default.
    #[test]
    fn existing_assignment_missing_is_active_defaults_to_disengaged() {
        let ftsw = serde_json::json!([
            [
                {
                    "func": "param", "groupId": "G1", "nodeId": "amp", "parameterId": "drive",
                    "colorA": 5, "colorB": 9, "customLabel": "X", "linkGroup": 2
                }
            ]
        ]);
        let preset = preset_with_lev_param(0.5);
        let (_, spec) = resolve_footswitch_job(&ftsw, &preset, &job()).expect("resolve");
        assert_eq!(
            spec.function_index, 0,
            "edits the existing param fn, not a new one"
        );
        assert!(
            !spec.is_active,
            "an absent isActive must default to disengaged, not engaged"
        );
    }
}
