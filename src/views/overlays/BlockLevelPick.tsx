// src/views/overlays/BlockLevelPick.tsx — the Set up step's COMBINED block+param
// leveling-handle picker (D2). ONE component drives all three row kinds:
//   • Base rows — candidates from `list_level_blocks`; a "Preset level" pseudo-option
//     (the master `presetLevel`) is the default first entry.
//   • Scene rows — candidates from `list_scene_level_handles`'s `allCandidates`; an
//     "Amp output level" pseudo-option (the per-scene amp joint-k path) is the default.
//   • Footswitch rows — candidates are the switch's own `level_params` (no device
//     read — already in hand). NO pseudo-option: every FS row must carry a real handle
//     (the backend removed the verify-only "no handle" row entirely).
// A pseudo-option submits `handle: null` on the wire — for Base/Scene that is a
// DIFFERENT, richer path than any single block param (the backend's own per-scene amp
// auto-pick, or the preset-level path), never just "the first candidate".
//
// Candidates are grouped BY BLOCK, blocks with a level-class param sorted first; within
// a block, level-class params (`level_linear`/`level_db`) list before `wet_mix`. The
// single best candidate overall (first in that order) is flagged "Recommended". A
// `disabled` candidate (Scene's `shared_with_base`/`unknown` scope) stays visible but
// inert, with its reason. A `wet_mix` candidate is flagged "may change the tone" (it
// also moves the effect's presence, not just its loudness).
//
// DANGER-rule guard (`Pick`/`BlockPick` trap): a stored `handle` the current candidate
// list doesn't cover must render VERBATIM + a warning, never silently fall back to the
// pseudo-option or `candidates[0]`.
//
// Click-only. Reuses the wizard's card-portaled dropdown, same machinery as Pick.

import { useContext } from "react";

import { useTheme } from "../../theme/ThemeContext";
import { Icon } from "../../ui/Icon";
import { Tag } from "../../ui/Tag";
import { blockArtTile } from "../../models/blockArt";
import { paramLabel } from "../level/leveling";
import { DialogCardCtx } from "./wizardContext";
import { PickPortalMenu } from "./PickPortalMenu";
import { usePickAnchor } from "./usePickAnchor";
import { pickTriggerBorder } from "./pickTriggerChrome";
import { BlockParamRow, BlockParamWarnNote } from "./BlockParamRow";
import type { ParamClass } from "../../lib/types";

export type BlockLevelCandidate = {
  groupId: string;
  nodeId: string;
  fenderId: string;
  parameterId: string;
  /** The classifier's verdict — undefined when the source carries none (Base's
   *  `list_level_blocks`, which is already gated to level-safe params but doesn't
   *  annotate the class on the wire; see `session::LevelBlock`). An undefined-class
   *  candidate sorts after every classified one and never shows a tone-risk note. */
  paramClass?: ParamClass;
  /** `true` ⇒ this control can only make the sound QUIETER (already at/near the top
   *  of its range). Scene rows only. */
  lowersOnly?: boolean;
} & (
  | { disabled?: false; disabledTitle?: undefined }
  /** Scene rows only: this control's overlay scope — `shared_with_base`/`unknown`
   *  disable the row (the backend refuses that write). A disabled row always
   *  carries its reason — the producer (`sceneDisabledTitle`) never emits one
   *  without the other. */
  | { disabled: true; disabledTitle: string }
);

export interface BlockLevelHandle {
  groupId: string;
  nodeId: string;
  parameterId: string;
}

export type BlockLevelFetch =
  | { status: "unfetched" | "loading" | "error" }
  | { status: "resolved"; list: BlockLevelCandidate[] };

export interface BlockLevelPickProps {
  /** Pseudo first entry's label ("Preset level" / "Amp output level"). Omit for
   *  footswitch rows — D2: every FS row must carry a real handle. */
  pseudoLabel?: string;
  /** `null` = the pseudo-option (when offered) or, on a footswitch row, "not yet
   *  resolved" (the row always seeds a real default, so this stays defensive). */
  handle: BlockLevelHandle | null;
  onHandleChange: (h: BlockLevelHandle | null) => void;
  candidates: BlockLevelFetch;
  /** Fire the lazy per-preset fetch (Base/Scene rows only). Idempotent — safe to call
   *  on every open. A no-op prop (`() => undefined`) for footswitch rows, whose
   *  candidates are already in hand. */
  onOpen: () => void;
}

/** Level-class-first rank, shared by the "Recommended" pick and the group order —
 *  mirrors `leveling.ts`'s `CLASS_RANK` (kept local: that table is keyed on the WIRE
 *  `ParamClass`, this one also has to rank the classless Base candidates). */
function rank(c: BlockLevelCandidate): number {
  if (c.paramClass === "level_linear" || c.paramClass === "level_db") return 0;
  if (c.paramClass === "wet_mix") return 1;
  return 2;
}

export function BlockLevelPick({
  pseudoLabel,
  handle,
  onHandleChange,
  candidates,
  onOpen,
}: BlockLevelPickProps) {
  const { t } = useTheme();
  const cardRef = useContext(DialogCardCtx);
  const list = candidates.status === "resolved" ? candidates.list : [];

  // Blocks (groupId:nodeId) ordered by their best-ranked candidate; candidates within a
  // block ordered by rank. One stable pass — `Map` preserves first-insertion key order.
  const byBlock = new Map<string, BlockLevelCandidate[]>();
  list.forEach((c) => {
    const key = `${c.groupId}:${c.nodeId}`;
    const g = byBlock.get(key);
    if (g) g.push(c);
    else byBlock.set(key, [c]);
  });
  const blocks = [...byBlock.values()]
    .map((g) => [...g].sort((a, b) => rank(a) - rank(b)))
    .sort((a, b) => {
      const ra = a.length > 0 ? rank(a[0]) : 2;
      const rb = b.length > 0 ? rank(b[0]) : 2;
      return ra - rb;
    });
  const recommended =
    blocks.length > 0 && blocks[0].length > 0 ? blocks[0][0] : null;

  const { open, anchor, pos, cardEl, menuRef, triggerRef, openMenu, close } =
    usePickAnchor(cardRef, {
      onOpen,
      // Grows AFTER it opens (the lazy fetch resolves a moment later) — same key
      // shape as SceneLevelPick's, so the menu re-clamps/re-flips once real rows land.
      contentKey: `${candidates.status}:${String(list.length)}`,
    });

  const matched = handle
    ? list.find(
        (c) =>
          c.groupId === handle.groupId &&
          c.nodeId === handle.nodeId &&
          c.parameterId === handle.parameterId,
      )
    : undefined;
  // DANGER-rule guard: a stored handle absent from the current (resolved) candidate
  // list must render verbatim + a warning — never silently fall back to the pseudo
  // option or `candidates[0]`. Gated on `resolved` so a not-yet-fetched list doesn't
  // flag a perfectly valid carried-forward handle as stale.
  const stale = handle != null && candidates.status === "resolved" && !matched;

  const triggerArt = matched ? blockArtTile(matched.fenderId) : null;
  const triggerLabel = handle
    ? matched
      ? `${triggerArt?.fullName ?? triggerArt?.name ?? ""} · ${paramLabel(handle.parameterId)}`
      : candidates.status === "resolved"
        ? `${paramLabel(handle.parameterId)} (removed)`
        : paramLabel(handle.parameterId)
    : (pseudoLabel ?? "Choose a control");

  const candidateRow = (c: BlockLevelCandidate) => {
    const on =
      handle?.groupId === c.groupId &&
      handle.nodeId === c.nodeId &&
      handle.parameterId === c.parameterId;
    const rec = c === recommended;
    const loud = c.paramClass !== "wet_mix";
    return (
      <BlockParamRow
        key={`${c.nodeId}:${c.parameterId}`}
        pickKey={`${c.nodeId}:${c.parameterId}`}
        art={blockArtTile(c.fenderId)}
        paramLabel={paramLabel(c.parameterId)}
        selected={on}
        disabled={c.disabled}
        disabledTitle={c.disabled ? c.disabledTitle : undefined}
        onPick={() => {
          onHandleChange({
            groupId: c.groupId,
            nodeId: c.nodeId,
            parameterId: c.parameterId,
          });
          close();
        }}
        note={
          c.disabled ? (
            <BlockParamWarnNote>{c.disabledTitle}</BlockParamWarnNote>
          ) : rec ? (
            <Tag tone="good" uppercase>
              {loud ? "Recommended - loudness only" : "Recommended"}
            </Tag>
          ) : !loud ? (
            <BlockParamWarnNote>may change the tone</BlockParamWarnNote>
          ) : c.lowersOnly ? (
            <span
              style={{ fontFamily: t.sans, fontSize: 10.5, color: t.mutedInk }}
            >
              can only lower
            </span>
          ) : undefined
        }
      />
    );
  };

  const sectionHead = (label: string) => (
    <div
      key={`h:${label}`}
      style={{
        padding: `${String(t.space2)}px ${String(t.space4)}px ${String(t.space2)}px`,
        fontFamily: t.mono,
        fontSize: 9,
        letterSpacing: "0.12em",
        textTransform: "uppercase",
        color: t.faint,
      }}
    >
      {label}
    </div>
  );

  return (
    <div
      ref={triggerRef}
      style={{ position: "relative", width: "100%", minWidth: 0 }}
    >
      <div
        onClick={openMenu}
        title="Choose this sound's leveling control"
        style={{
          display: "flex",
          alignItems: "center",
          gap: t.space3,
          height: 26,
          padding: `0 ${String(t.space4)}px`,
          boxSizing: "border-box",
          border: pickTriggerBorder(t, { open, warn: stale }),
          borderRadius: 6,
          background: t.bg,
          cursor: "pointer",
          whiteSpace: "nowrap",
          overflow: "hidden",
        }}
      >
        {stale && (
          <Icon
            name="warn-tri"
            size={12}
            stroke={t.sevWarn}
            strokeWidth={1.7}
          />
        )}
        <span
          style={{
            flex: 1,
            minWidth: 0,
            fontFamily: t.sans,
            fontSize: 11,
            color: stale ? t.sevWarn : t.ink2,
            overflow: "hidden",
            textOverflow: "ellipsis",
          }}
        >
          {triggerLabel}
        </span>
        <Icon
          name="chev-down"
          size={11}
          stroke={open ? t.accentDeep : t.faint}
        />
      </div>

      {open && anchor && cardEl && (
        <PickPortalMenu
          cardEl={cardEl}
          menuRef={menuRef}
          left={pos ? pos.left : anchor.left}
          top={pos ? pos.top : anchor.below}
          visible={pos != null}
          minWidth={Math.max(anchor.width, 280)}
          onClose={close}
        >
          {pseudoLabel != null && (
            <div
              onClick={(e) => {
                e.stopPropagation();
                onHandleChange(null);
                close();
              }}
              style={{
                display: "flex",
                alignItems: "center",
                gap: t.space5,
                padding: `${String(t.space3)}px ${String(t.space4)}px`,
                borderRadius: 5,
                cursor: "pointer",
                background: handle == null ? t.accentSoft : "transparent",
              }}
              onMouseEnter={(e) => {
                if (handle != null) e.currentTarget.style.background = t.hover;
              }}
              onMouseLeave={(e) => {
                if (handle != null)
                  e.currentTarget.style.background = "transparent";
              }}
            >
              <span
                style={{
                  fontFamily: t.mono,
                  fontSize: 11,
                  color: handle == null ? t.accentDeep : t.ink2,
                }}
              >
                {pseudoLabel} (default)
              </span>
              {handle == null && (
                <span style={{ marginLeft: "auto" }}>
                  <Icon
                    name="check"
                    size={13}
                    stroke={t.accentDeep}
                    strokeWidth={2}
                  />
                </span>
              )}
            </div>
          )}

          {candidates.status === "loading" && (
            <div
              style={{
                padding: t.space4,
                fontFamily: t.sans,
                fontSize: 11,
                color: t.mutedInk,
              }}
            >
              Loading controls…
            </div>
          )}
          {candidates.status === "error" && (
            <div
              style={{
                padding: t.space4,
                fontFamily: t.sans,
                fontSize: 11,
                color: t.sevWarn,
              }}
            >
              Couldn’t read this preset’s controls.
            </div>
          )}
          {blocks.flatMap((group) => {
            if (group.length === 0) return [];
            const art = blockArtTile(group[0].fenderId);
            return [
              sectionHead(art.fullName ?? art.name),
              ...group.map(candidateRow),
            ];
          })}
          {stale && (
            <div
              style={{
                display: "flex",
                alignItems: "center",
                gap: t.space3,
                padding: t.space4,
                fontFamily: t.sans,
                fontSize: 10.5,
                color: t.sevWarn,
              }}
            >
              <Icon
                name="warn-tri"
                size={11}
                stroke={t.sevWarn}
                strokeWidth={1.7}
              />
              <span>
                stored pick no longer offered — pick again
                {pseudoLabel ? " or use the default" : ""}.
              </span>
            </div>
          )}
        </PickPortalMenu>
      )}
    </div>
  );
}

export default BlockLevelPick;
