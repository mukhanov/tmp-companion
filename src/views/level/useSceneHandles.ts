// src/views/level/useSceneHandles.ts — the Set-up step's per-preset scene-handle
// candidate cache (moved out of SetupBody: a lazy, fetch-once-per-mount tri-state
// cache — `list_scene_level_handles` is a real device read, so it fires on first
// open of any row's picker, once per PRESET, never eagerly; Set up otherwise does
// no device reads).
//
// `candidatesFor` returns an EXPLICIT fetch-state discriminant rather than
// `SceneHandleCandidate[] | "loading" | "error" | undefined` — the old shape let a
// bare `undefined` ("not fetched yet") and an empty/mismatched list ("fetched,
// this handle is gone") collapse into the same falsy check, so a carried-forward
// VALID handle rendered "(removed)" until the fetch resolved (BUG→GATE). Naming
// "unfetched" as its own state is what SceneLevelPick derives BOTH `stale` and
// `triggerLabel` from now — see that component.
//
// No self-heal: an "error" result for a slot is cached for the rest of THIS
// mount (`fetchedSlotsRef` marks the slot fetched even on failure) — re-opening
// the wizard (a fresh SetupBody mount) is the only retry path. Same behavior as
// the pre-hook code; documented here because it's easy to assume a picker retry
// re-fetches.

import { useRef, useState } from "react";

import { listSceneLevelHandles } from "../../lib/invoke";
import type { SceneHandleRow, SceneHandleCandidate } from "../../lib/types";

export type HandleFetchState =
  | { status: "unfetched" }
  | { status: "loading" }
  | { status: "error" }
  | { status: "resolved"; candidates: SceneHandleCandidate[] };

const UNFETCHED: HandleFetchState = { status: "unfetched" };
const LOADING: HandleFetchState = { status: "loading" };
const ERROR: HandleFetchState = { status: "error" };

export interface UseSceneHandlesResult {
  /** Fire the lazy fetch for `slot`'s scene-handle rows. Idempotent — safe to call
   *  on every menu open; only the first call per slot per mount actually reads. */
  prefetch: (slot: number) => void;
  /** This preset+scene's candidate fetch state, right now. */
  candidatesFor: (slot: number, sceneSlot: number) => HandleFetchState;
}

export function useSceneHandles(): UseSceneHandlesResult {
  const [handlesBySlot, setHandlesBySlot] = useState<
    Partial<Record<number, SceneHandleRow[] | "loading" | "error">>
  >({});
  const fetchedSlotsRef = useRef(new Set<number>());

  const prefetch = (slot: number) => {
    if (fetchedSlotsRef.current.has(slot)) return;
    fetchedSlotsRef.current.add(slot);
    setHandlesBySlot((p) => ({ ...p, [slot]: "loading" }));
    listSceneLevelHandles(slot)
      .then((rows) => {
        setHandlesBySlot((p) => ({ ...p, [slot]: rows }));
      })
      .catch(() => {
        setHandlesBySlot((p) => ({ ...p, [slot]: "error" }));
      });
  };

  const candidatesFor = (slot: number, sceneSlot: number): HandleFetchState => {
    const v = handlesBySlot[slot];
    if (v === undefined) return UNFETCHED;
    if (v === "loading") return LOADING;
    if (v === "error") return ERROR;
    return {
      status: "resolved",
      candidates: v.find((r) => r.sceneSlot === sceneSlot)?.candidates ?? [],
    };
  };

  return { prefetch, candidatesFor };
}
