// src/views/level/useLevelBlocks.ts — the Set-up step's per-preset BASE handle
// candidate cache. `list_level_blocks` is a real device read (load + discovery), so
// it fires LAZILY — on first open of a Base row's combined block+param picker, once
// per PRESET. A thin wrapper over `useLazySlotCache` — see that hook for the shared
// fetch/cache mechanics and its no-self-heal-within-a-mount contract.

import { listLevelBlocks } from "../../lib/invoke";
import type { LevelBlock } from "../../lib/types";
import { useLazySlotCache } from "./useLazySlotCache";

export type BaseBlockFetchState =
  | { status: "unfetched" }
  | { status: "loading" }
  | { status: "error" }
  | { status: "resolved"; blocks: LevelBlock[] };

export interface UseLevelBlocksResult {
  /** Fire the lazy fetch for `slot`'s level-type blocks. Idempotent — safe to call on
   *  every menu open; only the first call per slot per mount actually reads. */
  prefetch: (slot: number) => void;
  /** This preset's block-candidate fetch state, right now. */
  blocksFor: (slot: number) => BaseBlockFetchState;
}

export function useLevelBlocks(): UseLevelBlocksResult {
  const { prefetch, listFor } = useLazySlotCache(listLevelBlocks);

  const blocksFor = (slot: number): BaseBlockFetchState => {
    const st = listFor(slot);
    return st.status === "resolved"
      ? { status: "resolved", blocks: st.list }
      : st;
  };

  return { prefetch, blocksFor };
}
