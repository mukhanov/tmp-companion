//! TMP Companion — Tauri backend crate root.
//!
//! The app drives a USB-connected Fender Tone Master Pro in re-amp mode to
//! auto-level presets to a LUFS target: play a sample through the preset's DSP,
//! capture the processed USB-Out, measure LUFS, and solve the `presetLevel`
//! (one-shot open-loop) that hits the target.
//!
//! This file is the slim crate hub: the `mod` tree, the re-export seams that
//! make command/probe fns nameable at the crate root (`probe_api`, `commands`,
//! `bootstrap::run`, `e2e_server`), and the shared process state — `AppState`,
//! the `MONITOR_*` coordination statics, and `lock_ok`.

use std::sync::{Arc, Mutex};

use serde::Serialize;
use tauri::State;

// Several builders/methods are exercised only from M2/M3 onward; silence
// dead-code noise until then without weakening warnings elsewhere.
#[allow(dead_code)]
mod audio;
mod audiograph;
mod audition;
mod backup;
mod backup_read;
mod blockcaps;
mod blocklib;
mod bulk_cmd;
mod bulkrun;
mod device_gate;
#[cfg(target_os = "macos")]
mod dock;
mod doctor;
#[cfg(feature = "e2e")]
mod e2e_server;
mod footswitch;
#[allow(dead_code)]
mod hid;
mod ir;
#[allow(dead_code)]
mod leveller;
mod library;
mod lint;
#[allow(dead_code)]
mod lufs;
mod migration;
mod monitor;
mod param_class;
mod paramedit;
mod preset_io;
mod presetmeta;
mod probe_api;
mod profiles;
#[allow(dead_code)]
mod proto;
mod psd;
mod rename;
mod replace_inplace;
mod saved_blocks;
mod scenes;
mod search;
#[allow(dead_code)]
mod session;
#[cfg(any(test, feature = "e2e"))]
mod sim_device;
mod spectrum;
// `pub` so the `gen_samples` bin (a separate crate) can reach the shared
// catalog as `tmp_companion_lib::topologies`.
pub mod topologies;
mod variants;
mod watcher;

pub use backup_read::*;
pub(crate) use device_gate::*;
// The `probe_*` entry points (reachable as `<libcrate>::probe_xxx` for `bin/probe.rs`).
pub use probe_api::*;
// Interim seam: helpers that stayed-in-lib commands still call after the probe_api
// extraction (Phase 2). Explicit list documents the boundary until a later phase.
pub(crate) use probe_api::level::filter_amp_candidates;
pub(crate) use probe_api::scene_bench::knob_bounds;
pub(crate) use probe_api::scene_jobs::{
    build_scene_jobs, build_scene_jobs_with_handles, is_amp_model_id, is_amp_output_level_param,
    last_loaded_scene, prepass_scene_docs_via, read_saved_preset, scene_overlay, scene_overlay_for,
    scene_overlays_change_param, scene_write_verdict, scenes_restating_base,
    warn_missing_restore_scene, SceneHandleSpec, SceneOverlay, SceneWriteVerdict,
};
pub(crate) use probe_api::setlists::{read_setlist_list, read_setlist_songs};
pub(crate) use probe_api::slot_write::{discover_active_graph, load_then_discover_blocks};
pub(crate) use probe_api::songs::{converge_song_bpm, read_song_list, read_song_presets};
pub(crate) use probe_api::stimulus::{
    read_stimulus_calibrated, read_stimulus_calibrated_with_shortfall,
};
pub use replace_inplace::*;
pub use saved_blocks::*;

pub use session::PresetEntry;
use session::Session;
pub use session::{ActiveGraph, GraphNode, Stage};

#[macro_use]
mod commands;
mod bootstrap;
pub use bootstrap::run;
// The command modules' fns/types are crate-internal; this seam makes them nameable at
// the crate root for `bootstrap::run`'s `generate_handler!` and the e2e handler list.
// `bulk_replace`/`copy_apply`/`level_scenes` carry the wire enums/structs that were
// crate-public before the split (`CopyRepl` et al.), so their re-export stays `pub`
// to preserve that reachability (a `pub(crate)` cap would make serde-only fields read
// as dead code); the remaining modules expose only `pub(crate)` items.
pub use commands::{bulk_replace::*, copy_apply::*, level_scenes::*};
pub(crate) use commands::{
    device::*, doctor::*, edit_tools::*, held_edit::*, level_footswitch::*, level_preset::*,
    library::*, media::*, migration::*, presets::*, setlists::*, settings::*, songs::*, support::*,
};

/// Lock a state mutex, recovering the guard if a previous holder panicked and poisoned it
/// (`into_inner`). These mutexes guard single-writer state (the session slot, the library,
/// the run registry, the monitor caches); recovery is always the right move — a poisoned
/// `unwrap()` would otherwise brick the always-running monitor or every future device op.
/// Used at every lock site across lib.rs / monitor.rs / watcher.rs.
pub(crate) fn lock_ok<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

#[cfg(test)]
mod lock_ok_tests {
    use super::lock_ok;
    use std::sync::{Arc, Mutex};

    #[test]
    fn recovers_a_poisoned_mutex_instead_of_panicking() {
        let m = Arc::new(Mutex::new(5));
        let m2 = Arc::clone(&m);
        // Poison the mutex: a thread panics while holding the lock.
        let _ = std::thread::spawn(move || {
            let _g = m2.lock().unwrap();
            panic!("poison the mutex");
        })
        .join();
        assert!(m.lock().is_err(), "the mutex must be poisoned");
        // A plain .lock().unwrap() would panic here; lock_ok recovers the guard.
        assert_eq!(*lock_ok(&m), 5);
        *lock_ok(&m) = 9;
        assert_eq!(*lock_ok(&m), 9);
    }
}

/// Shared device session. `None` until the user connects. Behind an `Arc<Mutex>`
/// so blocking HID work can run off the UI thread via `spawn_blocking`.
#[derive(Default)]
pub(crate) struct AppState {
    session: Arc<Mutex<Option<Session>>>,
    /// The imported OFFLINE `.preset` library (None until `import_library`). The
    /// canonical full-preset source every bulk feature edits.
    library: Arc<Mutex<Option<library::Library>>>,
    /// Completed bulk runs, keyed by run_id, so `bulk_revert` can restore one.
    runs: Arc<Mutex<bulk_cmd::RunRegistry>>,
    /// Rendered audition clips, keyed by slot+topology, so re-auditioning
    /// skips the re-amp pass. Session-scoped (see `audition` module caveat).
    clip_cache: Arc<Mutex<audition::ClipCache>>,
}

use std::sync::atomic::{AtomicBool, Ordering::SeqCst};

/// Monitor intent: when set, the persistent device monitor (`monitor.rs`) owns the
/// idle HID seize, streams unsolicited unit pushes, and publishes the startup
/// snapshot. `connect_device` sets this after releasing any old UI session; commands
/// borrow the device through `DEVICE_OP_LOCK` + pause/ack. `stop_live_sync` is kept
/// for diagnostics/settings paths that explicitly need to reclaim a UI session.
pub(crate) static MONITOR_ENABLED: AtomicBool = AtomicBool::new(false);

/// A command (holding [`DEVICE_OP_LOCK`]) asks the persistent device monitor to
/// yield its exclusive HID seize so the command can open its own connection
/// without a `0xe00002c5` collision. Set true while a command's [`MonitorPauseGuard`]
/// is alive; cleared on its Drop. The monitor polls this every pump iteration.
pub(crate) static MONITOR_PAUSE_REQ: AtomicBool = AtomicBool::new(false);
/// The monitor has dropped its `Session` (its seize is free) in response to a pause
/// request. The command waits (bounded) for this ack before proceeding. Cleared by
/// the monitor when it resumes after the request clears.
pub(crate) static MONITOR_PAUSED_ACK: AtomicBool = AtomicBool::new(false);

/// A monitor THREAD actually exists in this process — set by [`monitor::spawn`].
///
/// [`MONITOR_ENABLED`] means "the monitor owns the device", which is not the same thing:
/// `e2e_server` sets it in BOTH tiers to get the reconnect skip in
/// `with_released_seize_blocking`, but it never calls `monitor::spawn` (only `bootstrap`
/// does). Waiting for [`MONITOR_PAUSED_ACK`] there waits for a thread that cannot answer,
/// so every bridged command paid the full `PAUSE_WAIT_TRIES × PAUSE_WAIT_STEP_MS` budget —
/// measured at 1.14 s for a trivial command. Gate the wait on a thread EXISTING, which is
/// the precise condition, rather than on which e2e tier is running.
pub(crate) static MONITOR_SPAWNED: AtomicBool = AtomicBool::new(false);

#[cfg(feature = "e2e")]
pub(crate) use e2e_server::e2e_offline_fake;
#[cfg(feature = "e2e")]
pub(crate) use e2e_server::e2e_online;
#[cfg(feature = "e2e")]
pub(crate) use e2e_server::e2e_showcase;
#[cfg(feature = "e2e")]
pub use e2e_server::run_e2e_server;

/// Fixture invariants that must hold regardless of build features.
///
/// Deliberately NOT inside the `#[cfg(feature = "e2e")]` test module: that module
/// only compiles under `--features e2e`, and building with that feature is
/// forbidden here (it fabricates every LUFS and clobbers the production probe in
/// the shared target dir). A gate that only runs in a build nobody may make is
/// not a gate.
#[cfg(test)]
mod fixture_gates {
    /// Every committed scenario fixture, decoded: `(listIndex, name, presetJson
    /// string, parsed presetJson)`. One reader, so a schema rename fails loudly in
    /// one place instead of making each gate below pass vacuously.
    fn fixtures() -> Vec<(u32, String, String, serde_json::Value)> {
        let path = std::path::Path::new("../e2e/fixtures/scenario-presets.json");
        assert!(
            path.is_file(),
            "{} is missing — it is git-tracked, so absence means a moved/renamed \
             fixture or a wrong relative path",
            path.display()
        );
        let raw = std::fs::read_to_string(path).expect("read fixture");
        let entries: Vec<serde_json::Value> = serde_json::from_str(&raw).expect("fixture is JSON");
        let out: Vec<_> = entries
            .iter()
            .map(|e| {
                let js = e["presetJson"].as_str().expect("presetJson is text");
                (
                    e["listIndex"].as_u64().expect("listIndex") as u32,
                    e["name"].as_str().expect("name").to_string(),
                    js.to_string(),
                    serde_json::from_str(js).expect("presetJson parses"),
                )
            })
            .collect();
        assert_eq!(out.len(), 6, "the scenario set is six presets at 400-405");
        out
    }

    /// The fixture at `list_index`, by its 0-based list index.
    fn fixture(list_index: u32) -> (String, String, serde_json::Value) {
        fixtures()
            .into_iter()
            .find(|(i, ..)| *i == list_index)
            .map(|(_, n, js, v)| (n, js, v))
            .unwrap_or_else(|| panic!("no scenario fixture at list index {list_index}"))
    }

    /// Catalog block ids whose `category` makes them a SPEAKER CABINET (or a raw IR)
    /// — the downstream block a bare amp head needs to satisfy the cab rule. Read
    /// from the same `tmp-model-guide.json` `scene_jobs::amp_model_ids` reads, so
    /// amp-ness and cab-ness can never disagree about what the catalog says.
    /// Catalog block ids whose `category` makes them a SPEAKER CABINET (or a raw IR)
    /// — the downstream block a bare amp head needs to satisfy the cab rule. Routed
    /// through the same category collector `scene_jobs::amp_model_ids` uses, so
    /// amp-ness and cab-ness can never disagree about what the catalog says.
    fn cab_model_ids() -> std::collections::HashSet<String> {
        let ids = crate::probe_api::scene_jobs::model_ids_by_category(|cat| {
            matches!(cat, "Cabinets" | "IR")
        });
        assert!(
            ids.len() > 50,
            "expected the catalog's cabinet rows ({} found) — a category rename would \
             otherwise make the cab rule pass vacuously",
            ids.len()
        );
        ids
    }

    /// Does this device FenderId carry its cabinet BAKED IN? Delegates to
    /// `scene_jobs::bakes_in_a_cab` so the cab-merged suffix rule can't drift between
    /// the production classifier and this gate.
    fn is_cab_merged_amp(model_id: &str) -> bool {
        crate::probe_api::scene_jobs::bakes_in_a_cab(model_id)
    }

    /// Every complete signal path through a preset, as ordered block lists — built
    /// from the PRODUCTION routing decoder (`session::extract_active_graph`), so the
    /// cab rule below reasons about the same lanes the app draws. A `Split` stage
    /// forks the path set; split-OUTPUT lanes fork it once more at the tail; the
    /// independent-rail templates (`gtrMicParallel`) replace it outright.
    /// Fork every path in `paths` into two, extending one copy with `a_blocks` and the
    /// other with `b_blocks` — the shared reshape both the `Split`-stage fork and the
    /// `graph.outputs` fork in [`signal_paths`] perform.
    fn fork(
        paths: &[Vec<crate::GraphNode>],
        a_blocks: &[crate::GraphNode],
        b_blocks: &[crate::GraphNode],
    ) -> Vec<Vec<crate::GraphNode>> {
        paths
            .iter()
            .flat_map(|p| {
                [a_blocks, b_blocks].map(|lane| {
                    let mut q = p.clone();
                    q.extend(lane.iter().cloned());
                    q
                })
            })
            .collect()
    }

    fn signal_paths(graph: &crate::ActiveGraph) -> Vec<Vec<crate::GraphNode>> {
        let mut paths: Vec<Vec<crate::GraphNode>> = vec![Vec::new()];
        for stage in &graph.stages {
            match stage {
                crate::Stage::Series { blocks } => {
                    for p in &mut paths {
                        p.extend(blocks.iter().cloned());
                    }
                }
                crate::Stage::Split { a, b } => {
                    paths = fork(&paths, a, b);
                }
            }
        }
        if let Some(outs) = &graph.outputs {
            paths = fork(&paths, &outs.a.blocks, &outs.b.blocks);
        }
        if let Some(lanes) = &graph.lanes {
            paths = lanes.iter().map(|l| l.blocks.clone()).collect();
        }
        paths
    }

    /// **THE CAB RULE** (standing user directive, enforced structurally so it cannot
    /// silently regress): every guitar amp in every committed fixture is a combo, an
    /// amp+cab-merged model (a cab/IR-suffixed id), or a bare head with a cabinet
    /// block DOWNSTREAM IN ITS OWN LANE. No bare heads — including in the incident
    /// fixtures, which used to be exempt by accident (`E2E Preset24` shipped four
    /// drives into a naked `ACD_TwinReverb65NoFx`).
    ///
    /// "Its own lane" is what makes this non-trivial: `E2E Hiwatt 3S` puts its head in
    /// the pre-split `G1` and a cab in EACH of the two `gtrParallel1` lanes (`G2`,
    /// `G3`), so a naive "later in the same group" check would red-light a
    /// device-authored preset that is perfectly cabbed. Hence the path walk.
    #[test]
    fn every_guitar_amp_in_every_fixture_reaches_a_cab() {
        let cabs = cab_model_ids();
        let mut amps_checked = 0usize;
        for (idx, name, _, preset) in fixtures() {
            let graph = crate::session::extract_active_graph(&preset, None);
            let paths = signal_paths(&graph);
            assert!(
                !paths.is_empty(),
                "{name} ({idx}): the routing decoder produced no signal path — a \
                 template rename would otherwise make this gate pass vacuously"
            );
            for path in &paths {
                for (i, node) in path.iter().enumerate() {
                    if !crate::is_amp_model_id(&node.model) {
                        continue;
                    }
                    amps_checked += 1;
                    if is_cab_merged_amp(&node.model) {
                        continue;
                    }
                    assert!(
                        path[i + 1..].iter().any(|n| cabs.contains(&n.model)),
                        "{name} ({idx}): amp {} ({}) is a BARE HEAD with no cabinet \
                         downstream in its lane [{}] — every fixture amp must be a \
                         combo, a cab-merged model, or a head + cab block",
                        node.node_id,
                        node.model,
                        path.iter()
                            .map(|n| n.model.as_str())
                            .collect::<Vec<_>>()
                            .join(" → ")
                    );
                }
            }
        }
        assert!(
            amps_checked >= 6,
            "expected every fixture's amps to be walked ({amps_checked} seen) — a \
             graph-decode change that emptied the node lists would pass vacuously"
        );
    }

    /// SIZE BUDGET. A slot-addressed saved-preset (`presetDataRequest`, field 8) read
    /// starts returning TAIL-TRUNCATED bodies somewhere around 17-20 KB, and the seed's
    /// own pristine/ownership probes are all substring scans over that body. Every
    /// fixture therefore stays under 16 KiB — with ONE named exemption: `E2E Hiwatt 3S`
    /// is a real device export kept BYTE-VERBATIM as the scene-conformance oracle, and
    /// it already sits at the cliff. Its size is pinned exactly rather than bounded, so
    /// an accidental edit to the one fixture nobody may edit fails here.
    #[test]
    fn e2e_fixtures_stay_inside_the_field8_read_budget() {
        const BUDGET: usize = 16 * 1024;
        const HIWATT_BYTES: usize = 20_012;
        for (idx, name, js, _) in fixtures() {
            if name == "E2E Hiwatt 3S" {
                assert_eq!(
                    js.len(),
                    HIWATT_BYTES,
                    "{name} ({idx}) is the KEEP-VERBATIM device export — it must not be \
                     edited (it is the scene-conformance oracle and already sits at the \
                     field-8 truncation cliff)"
                );
                continue;
            }
            assert!(
                js.len() < BUDGET,
                "{name} ({idx}) serializes to {} bytes, over the {BUDGET}-byte field-8 \
                 budget — trim scene overlays (a per-scene splitMix alone costs ~730 B, \
                 paid once per scene)",
                js.len()
            );
        }
    }

    /// FX1 `E2E Rig` @ 400 — the scene/overlay + damage-signature fixture. Pins the
    /// structural facts `e2e/fixtures/COVERAGE.md` maps use-case rows onto, so a
    /// fixture edit that quietly drops one fails here rather than in a spec whose
    /// failure message says nothing about the cause.
    #[test]
    fn fx_rig_carries_the_scene_and_damage_cases() {
        let (name, _, p) = fixture(400);
        assert_eq!(name, "E2E Rig");
        assert_eq!(p["audioGraph"]["template"], "gtrSeries");
        assert_eq!(p["scenes"].as_array().expect("scenes").len(), 4);
        assert_eq!(p["lastLoadedScene"], 8, "base is the saved context");

        let g1 = p["audioGraph"]["guitarNodes"]["G1"]
            .as_array()
            .expect("G1 chain");
        let ids: Vec<&str> = g1
            .iter()
            .map(|n| n["FenderId"].as_str().unwrap_or_default())
            .collect();
        assert_eq!(
            ids,
            [
                "ACD_TubeScreamer",
                "ACD_JC120",
                "ACD_TwinReverb65NoFx",
                "ACD_CabSimTMS",
                "ACD_Boost",
                "ACD_TMSpring63",
                "ACD_CryBabyQ535",
            ],
            "TWO amps (the amp-flip pair) sharing one downstream cab, then the raw-dB \
             boost, the wet-mix block and the all-Other-class block"
        );
        // The on-off drive stomp is saved ENGAGED (isActive true ⇒ bypass false).
        assert_eq!(g1[0]["dspUnitParameters"]["bypass"], false);

        let overlay = |scene: usize, node: &str| -> Option<&serde_json::Value> {
            p["scenes"][scene]["guitarNodes"]["G1"]
                .get(node)
                .map(|e| &e["dspUnitParameters"])
        };
        let is_bypass_only = |v: &serde_json::Value| {
            v.as_object()
                .expect("overlay params")
                .keys()
                .all(|k| ["bypass", "bypassType"].contains(&k.as_str()))
        };
        for (scene, active, idle) in [
            (0usize, "ACD_JC120", "ACD_TwinReverb65NoFx"),
            (1, "ACD_TwinReverb65NoFx", "ACD_JC120"), // the AMP FLIP
            (2, "ACD_JC120", "ACD_TwinReverb65NoFx"),
            (3, "ACD_JC120", "ACD_TwinReverb65NoFx"),
        ] {
            let a = overlay(scene, active).expect("active amp overlay");
            let b = overlay(scene, idle).expect("idle amp overlay");
            assert_eq!(
                a["bypass"], false,
                "scene {scene}: {active} is the live amp"
            );
            assert_eq!(b["bypass"], true, "scene {scene}: {idle} is flipped out");
            assert_eq!(
                a["outputLevel"], b["outputLevel"],
                "scene {scene}: BOTH amps must carry the SAME outputLevel — the offline \
                 capture model's stored-level probe takes the first G1 node carrying one, \
                 so unequal values would make the modelled ol_term depend on map order"
            );
            assert!(!is_bypass_only(a), "scene {scene}: amp overlays are FULL");
        }
        assert_eq!(
            overlay(2, "ACD_JC120").expect("ceiling amp")["outputLevel"],
            1.0,
            "scene 2 'Ceiling' is the headroom lowers_only / scene-clamp row"
        );
        // The Boost: a picker-visible isolated handle in scenes 0-2, Scene-Edit
        // DISABLED (bypass-only ⇒ shared_with_base refusal) in scene 3.
        for s in 0..3 {
            assert!(
                !is_bypass_only(overlay(s, "ACD_Boost").expect("boost overlay")),
                "scene {s}: the Boost handle is isolated (Scene Edit on)"
            );
        }
        assert!(
            is_bypass_only(overlay(3, "ACD_Boost").expect("boost overlay")),
            "scene 3 'Shared': the Boost overlay is bypass-only → shared_with_base"
        );
        // The all-Other-class block never gets an overlay: the SceneOverlay::Absent /
        // NeedsEnable case.
        for s in 0..4 {
            assert!(overlay(s, "ACD_CryBabyQ535").is_none());
        }

        // The two DOCTOR leveling-damage signatures, in the wire `ftsw` shape the
        // backup scan feeds `doctor::leveling_damage_hints`.
        let fs = crate::footswitch::enumerate_block_footswitches(&p["ftsw"], &p);
        let hints = crate::doctor::leveling_damage_hints(&fs);
        let kinds: Vec<_> = hints.iter().map(|h| h.kind).collect();
        assert!(
            kinds.contains(&crate::doctor::LevelingDamageKind::DeletedEffect)
                && kinds.contains(&crate::doctor::LevelingDamageKind::SweptOther),
            "E2E Rig must carry BOTH damage signatures (a zeroed wet mix and a swept \
             Other-class param); got {hints:?}"
        );
        // One UNLABELED block-acting switch (the empty-customLabel rendering case).
        assert!(
            fs.iter().any(|f| f.label.is_empty()),
            "E2E Rig must keep one unlabeled block-acting switch"
        );
    }

    /// FX2 `E2E Parallel` @ 403 — the joint-k / rebalance fixture.
    #[test]
    fn fx_parallel_runs_both_lane_amps() {
        let (name, _, p) = fixture(403);
        assert_eq!(name, "E2E Parallel");
        assert_eq!(p["audioGraph"]["template"], "gtrParallel1");
        assert_eq!(p["scenes"].as_array().expect("scenes").len(), 4);

        let lane = |g: &str| -> Vec<String> {
            p["audioGraph"]["guitarNodes"][g]
                .as_array()
                .expect("lane")
                .iter()
                .map(|n| n["FenderId"].as_str().unwrap_or_default().to_string())
                .collect()
        };
        assert_eq!(lane("G2"), ["ACD_JC120", "ACD_CabSimTMS"]);
        assert_eq!(lane("G3"), ["ACD_MarshallPlexi", "ACD_Mar412Cent100"]);
        for (g, node) in [("G2", "ampA"), ("G3", "ampB")] {
            let amp = p["audioGraph"]["guitarNodes"][g][0].clone();
            assert_eq!(amp["nodeId"], node);
            assert_eq!(amp["dspUnitParameters"]["bypass"], false, "both lanes live");
            assert_eq!(
                amp["dspUnitParameters"]["outputLevel"], 1.0,
                "both lane amps sit at outputLevel 1.0 in base and in every scene but the \
                 deliberate zero-authority one — they must MATCH each other, or the \
                 offline model's stored-level probe (first node carrying an outputLevel, \
                 G1..G7) would desync written/stored and the closed-form joint-k solve \
                 would stop converging in one step"
            );
        }
        for s in 0..4 {
            // Scene 2 "Clean" is saved with BOTH amps' output at ZERO — no authority over
            // the USB capture, so its job returns the ROUTING clamp, not a headroom one.
            let want = if s == 2 { 0.0 } else { 1.0 };
            for (g, node) in [("G2", "ampA"), ("G3", "ampB")] {
                assert_eq!(
                    p["scenes"][s]["guitarNodes"][g][node]["dspUnitParameters"]["outputLevel"],
                    want,
                    "scene {s} {node}"
                );
            }
            // The Bass-VI shared-knob shape: bypass-only in EVERY scene ⇒ the scene
            // handle picker must report scope "shared_with_base" everywhere.
            let kot = p["scenes"][s]["guitarNodes"]["G4"]["ACD_KingOfTone"]["dspUnitParameters"]
                .as_object()
                .expect("KingOfTone overlay");
            assert!(
                kot.keys()
                    .all(|k| ["bypass", "bypassType"].contains(&k.as_str())),
                "scene {s}: the post-merge KingOfTone overlay stays bypass-only"
            );
        }
        // The in-path mixer the rebalance lane drives.
        let mix1 = &p["audioGraph"]["splitMix"]["mixPoints"][0]["parameters"];
        assert!(mix1["levelA"].is_f64() && mix1["levelB"].is_f64());
        assert_ne!(
            mix1["levelA"], mix1["levelB"],
            "an authored, non-neutral mix"
        );
        // A switch-LINK radio group selecting the lane amps.
        let fs = crate::footswitch::enumerate_block_footswitches(&p["ftsw"], &p);
        let linked: Vec<_> = fs.iter().filter(|f| f.link_group.is_some()).collect();
        assert_eq!(linked.len(), 2, "a two-member switch-link radio group");
        assert_eq!(linked[0].link_group, linked[1].link_group);
    }

    /// FX3 `E2E Pedalboard` @ 401 — the SCENE-FREE fixture (copy/import + the Doctor's
    /// simple-chain apply), carrying the EXP, link-group and second-bank cases.
    #[test]
    fn fx_pedalboard_is_scene_free_with_exp_and_a_second_bank_switch() {
        let (name, _, p) = fixture(401);
        assert_eq!(name, "E2E Pedalboard");
        assert_eq!(p["audioGraph"]["template"], "gtrSeries");
        assert!(
            p["scenes"].as_array().expect("scenes").is_empty(),
            "FX3 is the ZERO-scene fixture"
        );
        // EXP: a volume pedal on exp1, a wah on exp2, and a TOE assign.
        assert_eq!(p["exp"]["exp1"][0]["nodeId"], "ACD_VolumePedal");
        assert_eq!(p["exp"]["exp2"][0]["nodeId"], "ACD_CryBabyQ535");
        assert_eq!(p["exp"]["toe"][0]["nodeId"], "ACD_CryBabyQ535");
        // A tempo-synced time-based block (noteDivision off "off" + a preset bpm).
        let trem = p["audioGraph"]["guitarNodes"]["G1"]
            .as_array()
            .expect("G1")
            .iter()
            .find(|n| n["FenderId"] == "ACD_TremoloBias")
            .expect("the tempo-synced block");
        assert_ne!(trem["dspUnitParameters"]["noteDivision"], "off");
        assert!(p["bpm"].as_f64().is_some_and(|b| b > 0.0));
        // A PARAM radio link group, and a switch on the SECOND bank (index >= 11).
        let fs = crate::footswitch::enumerate_block_footswitches(&p["ftsw"], &p);
        let radio: Vec<_> = fs
            .iter()
            .filter(|f| f.link_group == Some(3) && f.functions.iter().all(|x| x.func == "param"))
            .collect();
        assert_eq!(radio.len(), 2, "a two-member PARAM radio link group");
        assert!(
            fs.iter().any(|f| f.switch >= 11),
            "one block-acting switch must live on the second bank"
        );
    }

    /// FX4 `E2E Edge` @ 402 — the split-output / 8-scene fixture that also carries the
    /// Doctor's ONLINE oracle: Target 2's baked 2.6 kHz EQ ring, byte-verbatim.
    #[test]
    fn fx_edge_keeps_the_eq_ring_and_eight_scenes() {
        let (name, _, p) = fixture(402);
        assert_eq!(name, "E2E Edge");
        assert_eq!(p["audioGraph"]["template"], "gtrSplit");
        assert_eq!(p["scenes"].as_array().expect("scenes").len(), 8);
        assert_eq!(
            p["lastLoadedScene"], 3,
            "the saved context is a NON-base scene (the measurement-context case)"
        );
        assert!(
            p["outputMixerSettings"].is_object(),
            "a split-output preset keeps its outputMixerSettings"
        );
        // THE ORACLE: filters 3 and 4 both ring at 2.6 kHz, +12 dB, Q 14. Doctor's
        // online `harsh`/`fizzy` diagnosis is measured against exactly these values.
        let eq = p["audioGraph"]["guitarNodes"]["G2"]
            .as_array()
            .expect("out1 lane")
            .iter()
            .find(|n| n["FenderId"] == "ACD_FiveBandParamEQ")
            .expect("the EQ-ring block")["dspUnitParameters"]
            .clone();
        for band in [3, 4] {
            assert_eq!(eq[format!("filter{band}frequency")], 2600.0);
            assert_eq!(eq[format!("filter{band}gaindb")], 12.0);
            assert_eq!(eq[format!("filter{band}q")], 14.0);
            assert_eq!(eq[format!("filter{band}bypass")], false);
        }
        // The three overlay states across the 8 scenes: FULL (isolated handle),
        // BYPASS-ONLY (shared_with_base) and ABSENT (NeedsEnable) — see COVERAGE.md
        // for why FX4 cannot afford a full overlay per node per scene.
        let ol = |s: usize, node: &str| -> Option<Vec<String>> {
            p["scenes"][s]["guitarNodes"]["G1"]
                .get(node)?
                .get("dspUnitParameters")?
                .as_object()
                .map(|m| m.keys().cloned().collect())
        };
        assert!(
            ol(0, "ACD_JC120").is_some_and(|k| k.len() > 2),
            "scene 0 FULL"
        );
        assert!(
            ol(4, "ACD_JC120").is_some_and(|k| k.len() > 2),
            "scene 4 FULL"
        );
        assert_eq!(
            ol(2, "ACD_JC120"),
            Some(vec!["bypass".into(), "bypassType".into()]),
            "scene 2's amp overlay is bypass-only"
        );
        for s in [1usize, 3, 5, 6, 7] {
            assert!(
                ol(s, "ACD_JC120").is_none(),
                "scene {s}: amp overlay ABSENT"
            );
        }
    }

    /// 404/405 — the two INCIDENT fixtures. 404 is kept verbatim (its exact bytes are
    /// pinned by the size gate above); 405's amendment must not have disturbed the
    /// lazy-save incident's own shape: the four drive pedals, their block-acting
    /// switches and the amp node are all untouched, and only a cab was appended.
    #[test]
    fn incident_fixtures_keep_their_shapes() {
        let (name, _, hiwatt) = fixture(404);
        assert_eq!(name, "E2E Hiwatt 3S");
        assert_eq!(hiwatt["lastLoadedScene"], 3, "the saved non-base context");
        assert_eq!(hiwatt["scenes"].as_array().expect("scenes").len(), 4);

        let (name, _, p24) = fixture(405);
        assert_eq!(name, "E2E Preset24");
        let ids: Vec<&str> = p24["audioGraph"]["guitarNodes"]["G1"]
            .as_array()
            .expect("G1")
            .iter()
            .map(|n| n["FenderId"].as_str().unwrap_or_default())
            .collect();
        assert_eq!(
            ids,
            [
                "ACD_Plumes",
                "ACD_BluesDriver",
                "ACD_ObsessiveDrive",
                "ACD_Rat",
                "ACD_TwinReverb65NoFx",
                "ACD_CabSimTMS", // appended for the cab rule; nothing upstream moved
            ]
        );
        assert_eq!(
            p24["audioGraph"]["guitarNodes"]["G1"][4]["dspUnitParameters"]["outputLevel"], 1.0,
            "the saturated amp's own knob is untouched — the offline C table and the \
             `leveledParams` pedal curve both key off it"
        );
        assert!(p24["scenes"].as_array().expect("scenes").is_empty());
        let fs = crate::footswitch::enumerate_block_footswitches(&p24["ftsw"], &p24);
        assert_eq!(fs.len(), 4, "the four drive-pedal switches (ftsw 5-8)");
    }

    /// NON-REGRESSION GATE for two defects found on a real 1.8.45 unit (2026-07-26).
    ///
    /// The e2e scenario fixtures were written with `info.product_id = "pro"`. Every
    /// preset the device itself creates uses **`tmStomp`**, and on the unit a `"pro"`
    /// preset is rejected with **"This preset was created using a newer firmware
    /// revision"** — the scene-selection ribbon refuses to open it. Any scene-related
    /// experiment or e2e step targeting these fixtures is silently invalid.
    ///
    /// The same fixtures also shared ONE `preset_id` across all four presets, which
    /// contradicts the documented invariant (`tmp-companion-data-model`: preset
    /// identity is "a UUID, unique per preset" and the join key for host-side
    /// metadata). Four presets sharing a key makes that mapping ambiguous.
    ///
    #[test]
    fn e2e_fixtures_use_device_product_id_and_unique_preset_ids() {
        let entries = fixtures();
        assert!(!entries.is_empty(), "no fixture presets found to check");

        let mut ids = Vec::new();
        for (_, _, _, p) in &entries {
            let info = &p["info"];
            let name = info["displayName"]
                .as_str()
                .unwrap_or("<unnamed>")
                .to_string();

            assert_eq!(
                info["product_id"].as_str(),
                Some("tmStomp"),
                "preset {name:?}: product_id must be \"tmStomp\" (what the device writes). \
             \"pro\" makes the unit report \"created using a newer firmware revision\" \
             and refuses scene selection."
            );
            let preset_id = info["preset_id"].as_str().unwrap_or_default().to_string();
            assert!(
                !preset_id.is_empty(),
                "preset {name:?}: preset_id must be present and non-empty — a missing field \
                 defaulting to \"\" would compare equal to another missing preset_id and pass \
                 the uniqueness check below vacuously"
            );
            ids.push((name, preset_id));
        }
        assert_eq!(
            ids.len(),
            entries.len(),
            "every fixture entry must expose a parseable presetJson; checked {} of {} — \
             a schema rename that made presetJson unreadable would otherwise leave `ids` \
             empty and this gate would pass vacuously",
            ids.len(),
            entries.len()
        );

        for (i, (n1, id1)) in ids.iter().enumerate() {
            for (n2, id2) in ids.iter().skip(i + 1) {
                assert_ne!(
                    id1, id2,
                    "presets {n1:?} and {n2:?} share preset_id {id1} — preset_id is the \
                 documented unique per-preset identity and the host-metadata join key"
                );
            }
        }
    }

    /// Same gate as above, applied to `backup-fixture.bin` — the OTHER committed
    /// fixture the same defect class can hide in.
    ///
    /// `backup_read::tests::scenario_fixture_matches_scenario_presets_json` (the
    /// drift lock between this file and `scenario-presets.json`) does NOT catch a
    /// stale `product_id`/`preset_id`: it compares decoded `BackupPresetRow`s, and
    /// that struct never carries either field, so two archives that disagree on
    /// them still compare equal. This test reads the raw `presetJson` column
    /// directly (mirroring `backup_read::read_backup_archive`'s own LZ4-frame +
    /// tar + `sqlite3` decode) instead of going through `BackupPresetRow`, so the
    /// two fields the drift lock is blind to are actually checked.
    #[test]
    fn backup_fixture_uses_device_product_id_and_unique_preset_ids() {
        use std::io::Read;

        let path = std::path::Path::new("../e2e/fixtures/backup-fixture.bin");
        assert!(
            path.is_file(),
            "{} is missing — it is git-tracked, so absence means a moved/renamed \
             fixture or a wrong relative path, and skipping would pass this gate \
             vacuously",
            path.display()
        );
        let blob = std::fs::read(path).expect("read backup-fixture.bin");

        let mut tar_bytes = Vec::new();
        lz4_flex::frame::FrameDecoder::new(std::io::Cursor::new(&blob))
            .read_to_end(&mut tar_bytes)
            .expect("LZ4-frame decode");
        let mut db_bytes = None;
        let mut ar = tar::Archive::new(std::io::Cursor::new(&tar_bytes));
        for entry in ar.entries().expect("tar entries") {
            let mut e = entry.expect("tar entry");
            let path = e
                .path()
                .expect("tar entry path")
                .to_string_lossy()
                .into_owned();
            if path == "databaseBackup" || path.ends_with("normalDb.db3") {
                let mut buf = Vec::new();
                e.read_to_end(&mut buf).expect("tar extract db");
                db_bytes = Some(buf);
            }
        }
        let db_bytes = db_bytes.expect("databaseBackup entry present");

        // Deleted on every exit (including a panic from an `expect` below), mirroring
        // `backup_read::read_backup_archive`'s own `TempDb` guard — without it a
        // failed assertion here leaks the extracted DB into the temp dir.
        struct TempDb(std::path::PathBuf);
        impl Drop for TempDb {
            fn drop(&mut self) {
                let _ = std::fs::remove_file(&self.0);
            }
        }
        let db_path = std::env::temp_dir().join(format!(
            "tmp-companion-fixture-gate-{}.db3",
            std::process::id()
        ));
        std::fs::write(&db_path, &db_bytes).expect("write temp db");
        let _guard = TempDb(db_path.clone());
        let out = std::process::Command::new("sqlite3")
            .arg("-json")
            .arg(&db_path)
            .arg("SELECT displayName, presetJson FROM UserPresets")
            .output()
            .expect("run sqlite3");
        assert!(out.status.success(), "sqlite3 query failed: {out:?}");
        let rows: serde_json::Value =
            serde_json::from_slice(&out.stdout).expect("sqlite3 -json output parses");
        let rows = rows.as_array().expect("UserPresets rows");
        assert!(!rows.is_empty(), "no UserPresets rows found to check");

        let mut ids = Vec::new();
        for row in rows {
            let name = row["displayName"]
                .as_str()
                .unwrap_or("<unnamed>")
                .to_string();
            let js = row["presetJson"].as_str().expect("presetJson is text");
            let p: serde_json::Value = serde_json::from_str(js).expect("presetJson parses");
            let info = &p["info"];

            assert_eq!(
                info["product_id"].as_str(),
                Some("tmStomp"),
                "preset {name:?}: product_id must be \"tmStomp\" — see \
                 e2e_fixtures_use_device_product_id_and_unique_preset_ids for why"
            );
            let preset_id = info["preset_id"].as_str().unwrap_or_default().to_string();
            assert!(
                !preset_id.is_empty(),
                "preset {name:?}: preset_id must be present and non-empty — a missing field \
                 defaulting to \"\" would compare equal to another missing preset_id and pass \
                 the uniqueness check below vacuously"
            );
            ids.push((name, preset_id));
        }
        assert_eq!(
            ids.len(),
            rows.len(),
            "every UserPresets row must expose a parseable presetJson; checked {} of {} — \
             a schema rename would otherwise leave `ids` empty and this gate would pass \
             vacuously",
            ids.len(),
            rows.len()
        );

        for (i, (n1, id1)) in ids.iter().enumerate() {
            for (n2, id2) in ids.iter().skip(i + 1) {
                assert_ne!(
                    id1, id2,
                    "presets {n1:?} and {n2:?} share preset_id {id1} in backup-fixture.bin"
                );
            }
        }
    }

    /// NON-REGRESSION GATE for the fixture-scene corruption class (real 1.8.45 unit,
    /// 2026-07-28). The device silently DROPS a preset's ENTIRE `scenes[]` (and
    /// re-stamps `info.source_id` to its placeholder) the first time a scene is
    /// materialised (`loadScene`) and the preset saved, when the scenes are not
    /// fully device-conformant. HW isolation (`probe --scene-write-cell`, recall-only,
    /// no scene-edit, no write): the hand-built "E2E Reference" wiped on every
    /// recall+save while the device-authored "E2E Hiwatt 3S" survived identical ops;
    /// conformance-rebuilding Reference/Realistic made them survive. This corrupted
    /// the on-device fixture after every ONLINE level run (specs green, slot 400
    /// marker-less, teardown's guarded clear refusing) — the failure mode this gate
    /// exists to keep dead.
    ///
    /// What "conformant" means (oracle: the device-authored Hiwatt scenes):
    ///   * every scene carries the full 12-key shape (a 3-key sparse scene wipes);
    ///   * `splitMix` holds BOTH `mixPoints` and `splitPoints` (scene shape: objects
    ///     keyed by nodeId — the missing `splitPoints` was the final discriminator);
    ///   * overlay numerics are floats (the device authors no int scene params);
    ///   * `ftswStates` is exactly as long as the preset's `ftsw` switch list.
    #[test]
    fn e2e_fixture_scenes_are_device_conformant() {
        const SCENE_KEYS: [&str; 12] = [
            "ampControl",
            "ftswStates",
            "fxLoop",
            "fxLoop1SceneEdit",
            "fxLoop2SceneEdit",
            "guitarNodes",
            "micNodes",
            "midi",
            "sceneName",
            "spillover",
            "splitMix",
            "uuid",
        ];
        // Per-fixture expected scene count (listIndex → count), replacing a single
        // cross-fixture sum: a failure now names the culprit fixture instead of just
        // reporting the total drifted. A slot absent here (401, 405) is expected to
        // carry NO scenes.
        let expected_scenes: std::collections::HashMap<u32, usize> =
            [(400u32, 4usize), (402, 8), (403, 4), (404, 4)]
                .into_iter()
                .collect();
        let entries = fixtures();
        for (idx, name, _, p) in &entries {
            let scenes = p["scenes"].as_array().map_or(0, Vec::len);
            let expected = expected_scenes.get(idx).copied().unwrap_or(0);
            assert_eq!(
                scenes, expected,
                "{name:?} ({idx}): expected {expected} scenes, found {scenes} — a schema \
                 rename that hid the scenes would pass this gate vacuously"
            );
        }
        for (_, name, _, p) in &entries {
            let ftsw_len = p["ftsw"].as_array().map_or(0, Vec::len);
            for scene in p["scenes"].as_array().into_iter().flatten() {
                let sn = scene["sceneName"].as_str().unwrap_or("<unnamed scene>");
                let mut keys: Vec<&str> = scene
                    .as_object()
                    .expect("scene is an object")
                    .keys()
                    .map(String::as_str)
                    .collect();
                keys.sort_unstable();
                assert_eq!(
                    keys, SCENE_KEYS,
                    "{name:?} scene {sn:?}: must carry exactly the 12-key device scene \
                     shape — a sparse scene makes the unit wipe the whole scenes[] on \
                     the first loadScene+save"
                );
                for part in ["mixPoints", "splitPoints"] {
                    assert!(
                        scene["splitMix"][part]
                            .as_object()
                            .is_some_and(|m| !m.is_empty()),
                        "{name:?} scene {sn:?}: splitMix.{part} must be a non-empty \
                         nodeId-keyed object (missing splitPoints was the HW-isolated \
                         wipe trigger)"
                    );
                }
                assert_eq!(
                    scene["ftswStates"].as_array().map_or(0, Vec::len),
                    ftsw_len,
                    "{name:?} scene {sn:?}: ftswStates must be as long as ftsw"
                );
                for group in ["guitarNodes", "micNodes"] {
                    for (gid, nodes) in scene[group].as_object().into_iter().flatten() {
                        for (nid, body) in nodes.as_object().into_iter().flatten() {
                            for (pk, pv) in
                                body["dspUnitParameters"].as_object().into_iter().flatten()
                            {
                                assert!(
                                    !pv.is_i64() && !pv.is_u64(),
                                    "{name:?} scene {sn:?} {group}/{gid}/{nid}.{pk}: \
                                     int-typed overlay param {pv} — the device authors \
                                     floats in scenes; write {pv}.0"
                                );
                            }
                        }
                    }
                }
            }
        }
    }

    /// Sidecar/fixture CROSS-CHECK: `scenario-loudness.json`'s C table references
    /// fixture shape — scene counts, `leveledParams` group/node/param triples,
    /// `offbranchSwitchNode` — that `scenario-presets.json` must actually carry. A
    /// mismatch fails SILENTLY as a flat response (the leveled-param predicate in
    /// `sim_device::model_lufs` simply never activates, or activates against a node
    /// that doesn't exist), which cost a wrong-root-cause debugging pass once
    /// (COVERAGE.md row 18's corrected cause). This gate makes that class loud.
    #[test]
    fn sidecar_references_resolve_against_the_fixtures() {
        let sidecar_raw = std::fs::read_to_string("../e2e/fixtures/scenario-loudness.json")
            .expect("read scenario-loudness.json");
        let sidecar: serde_json::Value =
            serde_json::from_str(&sidecar_raw).expect("scenario-loudness.json is JSON");
        let slots = sidecar["slots"]
            .as_object()
            .expect("sidecar has a slots object");
        assert!(!slots.is_empty(), "no sidecar slots found to check");

        let entries = fixtures();
        for (slot_key, entry) in slots {
            let idx: u32 = slot_key
                .parse()
                .unwrap_or_else(|_| panic!("sidecar slot key {slot_key:?} is not a list index"));
            // (a) a fixture exists at that list index.
            let (_, name, _, p) = entries
                .iter()
                .find(|(i, ..)| *i == idx)
                .unwrap_or_else(|| panic!("sidecar slot {idx} has no fixture at that list index"));

            // (b) a sidecar `scenes` array's length matches the fixture's own scene count.
            if let Some(sidecar_scenes) = entry.get("scenes").and_then(|v| v.as_array()) {
                let fixture_scenes = p["scenes"].as_array().map_or(0, Vec::len);
                assert_eq!(
                    sidecar_scenes.len(),
                    fixture_scenes,
                    "{name:?} ({idx}): sidecar declares {} scene C values but the fixture \
                     carries {fixture_scenes} scenes",
                    sidecar_scenes.len()
                );
            }

            // (c) every `leveledParams` entry resolves to a real node + param.
            for lp in entry
                .get("leveledParams")
                .and_then(|v| v.as_array())
                .into_iter()
                .flatten()
            {
                let group = lp["group"].as_str().expect("leveledParams.group");
                let node = lp["node"].as_str().expect("leveledParams.node");
                let param = lp["param"].as_str().expect("leveledParams.param");
                let node_entry = p["audioGraph"]["guitarNodes"][group]
                    .as_array()
                    .unwrap_or_else(|| {
                        panic!(
                            "{name:?} ({idx}): leveledParams group {group:?} has no \
                             guitarNodes entry"
                        )
                    })
                    .iter()
                    .find(|n| n["nodeId"].as_str() == Some(node))
                    .unwrap_or_else(|| {
                        panic!(
                            "{name:?} ({idx}): leveledParams node {node:?} not found in \
                             guitarNodes.{group}"
                        )
                    });
                assert!(
                    node_entry
                        .get("dspUnitParameters")
                        .and_then(|d| d.get(param))
                        .is_some(),
                    "{name:?} ({idx}): leveledParams param {param:?} not found on {node:?}'s \
                     dspUnitParameters — the leveled-param curve would silently never \
                     activate"
                );
            }

            // (d) `offbranchSwitchNode`, where present, also resolves to a real node.
            if let Some(node) = entry.get("offbranchSwitchNode").and_then(|v| v.as_str()) {
                let groups = p["audioGraph"]["guitarNodes"]
                    .as_object()
                    .expect("guitarNodes is an object");
                let found = groups.values().any(|nodes| {
                    nodes
                        .as_array()
                        .into_iter()
                        .flatten()
                        .any(|n| n["nodeId"].as_str() == Some(node))
                });
                assert!(
                    found,
                    "{name:?} ({idx}): offbranchSwitchNode {node:?} not found in any \
                     guitarNodes group"
                );
            }
        }
    }

    /// Every row `COVERAGE.md`'s coverage matrix marks as covered by a Playwright spec
    /// (its Spec cell names a `.spec.ts` file, parsed loosely — a drift alarm, not a
    /// parser) must be CITED by at least one `// COVERAGE row(s) N[, M...]` comment in
    /// some `e2e/specs/*.spec.ts` file. Without this, the matrix's own claim ("every
    /// structural fact is pinned by a test") is unverifiable prose — a row and the spec
    /// that's supposed to prove it can drift apart with nothing failing.
    fn coverage_matrix_rows_citing_a_spec_ts(md: &str) -> std::collections::HashSet<u32> {
        md.lines()
            .filter_map(|line| {
                let cells: Vec<&str> = line.split('|').map(str::trim).collect();
                if cells.len() < 4 {
                    return None;
                }
                let row: u32 = cells[1].parse().ok()?;
                let spec_cell = cells[cells.len() - 2];
                spec_cell.contains(".spec.ts").then_some(row)
            })
            .collect()
    }

    /// Row numbers cited by `// COVERAGE row N` / `// COVERAGE rows N, M, ...` comments in
    /// one spec file's text. Tolerant of the separators actually in use (comma, slash,
    /// whitespace) rather than a strict parser — a citation is read up to the first
    /// character that ISN'T a digit/`,`/`/`/space, then split into individual numbers on
    /// any non-digit run.
    fn coverage_row_citations(content: &str) -> std::collections::HashSet<u32> {
        let marker = "COVERAGE row";
        let bytes = content.as_bytes();
        let mut out = std::collections::HashSet::new();
        let mut search_from = 0usize;
        while let Some(rel) = content.get(search_from..).and_then(|s| s.find(marker)) {
            let start = search_from + rel + marker.len();
            let mut i = start;
            if bytes.get(i) == Some(&b's') {
                i += 1; // optional "rows"
            }
            let window_start = i;
            while matches!(bytes.get(i), Some(b'0'..=b'9' | b',' | b'/' | b' ')) {
                i += 1;
            }
            for tok in content[window_start..i].split(|c: char| !c.is_ascii_digit()) {
                if let Ok(n) = tok.parse::<u32>() {
                    out.insert(n);
                }
            }
            search_from = i.max(start + 1);
        }
        out
    }

    #[test]
    fn coverage_rows_marked_playwright_covered_are_cited_by_some_spec() {
        let md = std::fs::read_to_string("../e2e/fixtures/COVERAGE.md").expect("read COVERAGE.md");
        let covered_rows = coverage_matrix_rows_citing_a_spec_ts(&md);
        assert!(
            covered_rows.len() > 10,
            "expected a healthy number of Playwright-covered matrix rows ({} found) — a \
             table reformat that hid the Spec column would otherwise pass this gate \
             vacuously",
            covered_rows.len()
        );

        let specs_dir = std::path::Path::new("../e2e/specs");
        assert!(specs_dir.is_dir(), "{} is missing", specs_dir.display());
        let mut cited: std::collections::HashSet<u32> = std::collections::HashSet::new();
        let mut spec_files_seen = 0usize;
        for entry in std::fs::read_dir(specs_dir).expect("read e2e/specs") {
            let path = entry.expect("dir entry").path();
            if !path.to_string_lossy().ends_with(".spec.ts") {
                continue;
            }
            spec_files_seen += 1;
            let content = std::fs::read_to_string(&path)
                .unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
            cited.extend(coverage_row_citations(&content));
        }
        assert!(
            spec_files_seen > 10,
            "expected many *.spec.ts files under e2e/specs ({spec_files_seen} found) — a \
             directory layout change would otherwise make this gate pass vacuously"
        );
        assert!(
            cited.len() >= 8,
            "expected a healthy number of distinct cited rows ({} found) across every \
             spec file — a comment-format drift would otherwise pass this gate vacuously",
            cited.len()
        );

        let mut missing: Vec<u32> = covered_rows.difference(&cited).copied().collect();
        missing.sort_unstable();
        assert!(
            missing.is_empty(),
            "COVERAGE.md marks row(s) {missing:?} as covered by a Playwright spec, but \
             no e2e/specs/*.spec.ts file cites it with a `// COVERAGE row(s) N` comment \
             — either add the citation next to the covering test, or fix COVERAGE.md's \
             Spec cell"
        );
    }
}
