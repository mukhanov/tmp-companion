// src/views/level/useSceneHandles.ts — the Set-up step's per-preset scene-handle
// candidate cache (moved out of SetupBody: a lazy, fetch-once-per-mount tri-state
// cache — `list_scene_level_handles` is a real device read, so it fires on first
// open of any row's picker, once per PRESET, never eagerly; Set up otherwise does
// no device reads). A thin wrapper over `useLazySlotCache` — see that hook for the
// shared fetch/cache mechanics and its no-self-heal-within-a-mount contract.
//
// `candidatesFor` returns an EXPLICIT fetch-state discriminant rather than
// `SceneHandleCandidate[] | "loading" | "error" | undefined` — the old shape let a
// bare `undefined` ("not fetched yet") and an empty/mismatched list ("fetched,
// this handle is gone") collapse into the same falsy check, so a carried-forward
// VALID handle rendered "(removed)" until the fetch resolved (BUG→GATE). Naming
// "unfetched" as its own state is what SceneLevelPick derives BOTH `stale` and
// `triggerLabel` from now — see that component.

import { listSceneLevelHandles } from "../../lib/invoke";
import type { SceneHandleRow, SceneHandleCandidate } from "../../lib/types";
import { useLazySlotCache } from "./useLazySlotCache";

export type HandleFetchState =
  | { status: "unfetched" }
  | { status: "loading" }
  | { status: "error" }
  | {
      status: "resolved";
      /** The safe-preselect list: level-safe candidates only, never `"other"`. */
      candidates: SceneHandleCandidate[];
      /** EVERY numeric control of every block in this scene, class-annotated and
       *  level-class first — the combined block+param picker's source (a superset of
       *  `candidates`). */
      allCandidates: SceneHandleCandidate[];
    };

export interface UseSceneHandlesResult {
  /** Fire the lazy fetch for `slot`'s scene-handle rows. Idempotent — safe to call
   *  on every menu open; only the first call per slot per mount actually reads. */
  prefetch: (slot: number) => void;
  /** This preset+scene's candidate fetch state, right now. */
  candidatesFor: (slot: number, sceneSlot: number) => HandleFetchState;
}

export function useSceneHandles(): UseSceneHandlesResult {
  const { prefetch, listFor } = useLazySlotCache(listSceneLevelHandles);

  const candidatesFor = (slot: number, sceneSlot: number): HandleFetchState => {
    const st = listFor(slot);
    if (st.status !== "resolved") return st;
    const row: SceneHandleRow | undefined = st.list.find(
      (r) => r.sceneSlot === sceneSlot,
    );
    return {
      status: "resolved",
      candidates: row?.candidates ?? [],
      allCandidates: row?.allCandidates ?? [],
    };
  };

  return { prefetch, candidatesFor };
}
