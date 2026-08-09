// src/views/overlays/SceneLevelPick.tsx — the Set up step's per-scene control picker.
//
// One compact affordance combines the two scene-row choices P2 adds:
//   • Target mode — "Match target" (every scene solves to the named target, the
//     default) or "Keep its offset" (preserve the scene's authored loudness
//     RELATIONSHIP to the batch reference).
//   • Control — the amp `outputLevel` (default, every existing caller) or a
//     user-chosen block param INSTEAD of it (`list_scene_level_handles`).
//
// The candidate list is a real device read, so it is fetched LAZILY (on first open,
// one call per PRESET — SetupBody caches it) rather than eagerly for every row; Set
// up otherwise does no device reads. A `shared_with_base` candidate is shown but
// DISABLED with its reason (the backend refuses that write); a `lowers_only` one is
// annotated, not hidden.
//
// Click-only. Reuses the wizard's card-portaled dropdown, same machinery as Pick and
// FsParamPick.

import { useContext } from "react";

import { useTheme } from "../../theme/ThemeContext";
import { Icon } from "../../ui/Icon";
import { Tag } from "../../ui/Tag";
import { blockArtTile } from "../../models/blockArt";
import { paramLabel, type SceneHandlePick } from "../level/leveling";
import { DialogCardCtx } from "./wizardContext";
import { PickPortalMenu } from "./PickPortalMenu";
import { usePickAnchor } from "./usePickAnchor";
import { pickTriggerBorder } from "./pickTriggerChrome";
import { BlockParamRow, BlockParamWarnNote } from "./BlockParamRow";
import type { HandleFetchState } from "../level/useSceneHandles";
import type { SceneHandleCandidate } from "../../lib/types";

export interface SceneLevelPickProps {
  targetMode: "match" | "offset";
  onTargetModeChange: (m: "match" | "offset") => void;
  /** null = the amp `outputLevel` default. */
  handle: SceneHandlePick | null;
  onHandleChange: (h: SceneHandlePick | null) => void;
  /** This scene's candidate fetch state — an explicit discriminant (never a bare
   *  `undefined`), so "not fetched yet" can never be mistaken for "fetched and
   *  genuinely gone" (see `stale`/`triggerLabel` below). SetupBody fetches +
   *  caches per preset on first open via `useSceneHandles`. */
  candidates: HandleFetchState;
  /** Fire the lazy per-preset fetch. Idempotent — safe to call on every open. */
  onOpen: () => void;
}

const SCOPE_WARN =
  "shared with the base preset — changes every scene sharing it";
const HEADROOM_NOTE = "can only lower";

export function SceneLevelPick({
  targetMode,
  onTargetModeChange,
  handle,
  onHandleChange,
  candidates,
  onOpen,
}: SceneLevelPickProps) {
  const { t } = useTheme();
  const cardRef = useContext(DialogCardCtx);
  const list = candidates.status === "resolved" ? candidates.candidates : [];
  const { open, anchor, pos, cardEl, menuRef, triggerRef, openMenu, close } =
    usePickAnchor(cardRef, {
      onOpen,
      // This menu is the one that grows AFTER it opens: `onOpen` fires the lazy per-preset
      // candidate read, so the first paint is the one-line "Loading controls…" and the real
      // body arrives a moment later. Without this key the placement stays the skeleton's
      // and a long candidate list renders off the bottom of the card, unclamped and
      // unflipped. Status + row count is enough — every content change that alters the
      // menu's height moves one of the two.
      contentKey: `${candidates.status}:${String(list.length)}`,
    });
  // Match on the FULL submitted identity — `groupId` included. `SceneHandlePick` carries
  // all three and all three go to the backend, so a two-field match can bind the trigger's
  // art/label to a same-named node in a DIFFERENT group (a parallel preset's two lanes each
  // holding an `outputLevel`) while the run writes the other one. That is `danger.md`'s
  // "UI values" rule verbatim: the UI showing one thing while the submitted value is
  // another. It also keeps `stale` honest — a handle whose group no longer exists must read
  // as removed, not silently re-point at its namesake.
  const matched = handle
    ? list.find(
        (c) =>
          c.groupId === handle.groupId &&
          c.nodeId === handle.nodeId &&
          c.parameterId === handle.parameterId,
      )
    : undefined;
  // DANGER-rule guard: the stored `handle` (from a re-level round, or a preset re-read
  // that dropped a candidate) may no longer appear in the current candidate list —
  // display it verbatim with a warning rather than silently falling back to the amp
  // default, which would submit a DIFFERENT control than the one shown. Gated on
  // `status === "resolved"`, not just "a list exists": before the fetch resolves there
  // is nothing to compare against, so a carried-forward VALID handle must render
  // plainly, not as "removed" (BUG→GATE — the old `Array.isArray(candidates)` check
  // couldn't tell "not fetched yet" from "fetched and genuinely gone").
  const stale = handle != null && candidates.status === "resolved" && !matched;

  const triggerArt = matched ? blockArtTile(matched.fenderId) : null;
  const triggerLabel = handle
    ? matched
      ? `${triggerArt?.fullName ?? triggerArt?.name ?? ""} · ${paramLabel(handle.parameterId)}`
      : candidates.status === "resolved"
        ? `${paramLabel(handle.parameterId)} (removed)`
        : paramLabel(handle.parameterId)
    : "Amp";
  const modeLabel = targetMode === "offset" ? "keep offset" : "match target";

  const levelGroup = list.filter(
    (c) => c.class === "level_linear" || c.class === "level_db",
  );
  const wetGroup = list.filter((c) => c.class === "wet_mix");

  const candidateRow = (c: SceneHandleCandidate) => {
    const disabled = c.scope === "shared_with_base" || c.scope === "unknown";
    // Same full-identity rule as `matched` above — the tick must mark the row that IS the
    // submitted handle, not a same-named node in another group.
    const on =
      handle?.groupId === c.groupId &&
      handle.nodeId === c.nodeId &&
      handle.parameterId === c.parameterId;
    return (
      <BlockParamRow
        key={`${c.nodeId}:${c.parameterId}`}
        pickKey={`${c.nodeId}:${c.parameterId}`}
        art={blockArtTile(c.fenderId)}
        paramLabel={paramLabel(c.parameterId)}
        selected={on}
        disabled={disabled}
        disabledTitle={SCOPE_WARN}
        onPick={() => {
          onHandleChange({
            groupId: c.groupId,
            nodeId: c.nodeId,
            parameterId: c.parameterId,
          });
          close();
        }}
        note={
          disabled ? (
            <BlockParamWarnNote>{SCOPE_WARN}</BlockParamWarnNote>
          ) : c.headroom === "lowers_only" ? (
            <span
              style={{ fontFamily: t.sans, fontSize: 10.5, color: t.mutedInk }}
            >
              {HEADROOM_NOTE}
            </span>
          ) : undefined
        }
      />
    );
  };

  const sectionHead = (label: string) => (
    <div
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
        title="Choose this scene's target mode + leveling control"
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
          {triggerLabel} · {modeLabel}
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
          {sectionHead("Target mode")}
          {(
            [
              { id: "match" as const, label: "Match target" },
              { id: "offset" as const, label: "Keep its offset" },
            ] satisfies { id: "match" | "offset"; label: string }[]
          ).map((m) => (
            <div
              key={m.id}
              onClick={(e) => {
                e.stopPropagation();
                onTargetModeChange(m.id);
              }}
              style={{
                display: "flex",
                alignItems: "center",
                gap: t.space5,
                padding: `${String(t.space3)}px ${String(t.space4)}px`,
                borderRadius: 5,
                cursor: "pointer",
                background: targetMode === m.id ? t.accentSoft : "transparent",
              }}
              onMouseEnter={(e) => {
                if (targetMode !== m.id)
                  e.currentTarget.style.background = t.hover;
              }}
              onMouseLeave={(e) => {
                if (targetMode !== m.id)
                  e.currentTarget.style.background = "transparent";
              }}
            >
              <span
                style={{
                  fontFamily: t.mono,
                  fontSize: 11,
                  color: targetMode === m.id ? t.accentDeep : t.ink2,
                }}
              >
                {m.label}
              </span>
              {targetMode === m.id && (
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
          ))}

          {sectionHead("Control")}
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
              Amp output level (default)
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
          {levelGroup.length > 0 && (
            <>
              {sectionHead("Level")}
              {levelGroup.map(candidateRow)}
            </>
          )}
          {wetGroup.length > 0 && (
            <>
              {sectionHead("Mix")}
              {wetGroup.map(candidateRow)}
            </>
          )}
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
                <Tag tone="warn">stored pick</Tag> no longer offered — pick
                again or use the amp default.
              </span>
            </div>
          )}
        </PickPortalMenu>
      )}
    </div>
  );
}

export default SceneLevelPick;
