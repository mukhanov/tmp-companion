// src/views/overlays/BlockParamRow.tsx — the "which block parameter" row shared
// by FsParamPick and SceneLevelPick's dropdown menus: a BlockArt tile, the
// block's name, a "·" separator, the parameter's label, an optional sub-line
// (each caller's own note — a "Recommended" tag, a plain loudness note, or a
// warning), and a trailing check icon when selected. Same hover/selected
// background in both callers, so this is the ONE copy.
//
// Sizing was reconciled to FsParamPick's (larger) set — a 38px tile at art
// size 34, name at 14px — SceneLevelPick's rows were slightly smaller (34px
// tile, art 30, name 13.5px); the bigger set reads more comfortably and both
// menus now match.

import type { ReactNode } from "react";

import { useTheme } from "../../theme/ThemeContext";
import { Icon } from "../../ui/Icon";
import { BlockArt } from "../../ui/BlockArt";
import type { BlockArtFields } from "../../models/blockArt";

export interface BlockParamRowProps {
  /** Resolved via `blockArtTile(fenderId)`. */
  art: BlockArtFields;
  /** The parameter's friendly label (e.g. via `paramLabel(parameterId)`). */
  paramLabel: string;
  /** The caller's own annotation — a Tag, plain text, or a warning line.
   *  Undefined renders no sub-line at all. */
  note?: ReactNode;
  selected: boolean;
  /** Disabled rows (SceneLevelPick's `shared_with_base`/`unknown` scope) stay
   *  visible but inert — dimmed, default cursor, no click. */
  disabled?: boolean;
  /** Shown as the row's `title` while disabled. */
  disabledTitle?: string;
  onPick: () => void;
  /** e2e hook: `${nodeId}:${parameterId}` — both this row's callers (FsParamPick,
   *  SceneLevelPick) render their menu through a PORTAL, so a spec can't scope a
   *  query to the picker's own trigger the way `PresetOptionRow`'s `data-setup-row`
   *  scopes a Setup row; this is the portal-content equivalent. Optional: neither
   *  caller is required to pass it. */
  pickKey?: string;
}

const TILE = 38;
const ART_SIZE = 34;

export function BlockParamRow({
  art,
  paramLabel,
  note,
  selected,
  disabled,
  disabledTitle,
  onPick,
  pickKey,
}: BlockParamRowProps) {
  const { t } = useTheme();
  return (
    <div
      data-block-param-pick={pickKey}
      onClick={(e) => {
        e.stopPropagation();
        if (disabled) return;
        onPick();
      }}
      title={disabled ? disabledTitle : undefined}
      style={{
        display: "flex",
        alignItems: "center",
        gap: t.space5,
        padding: t.space4,
        borderRadius: 8,
        cursor: disabled ? "default" : "pointer",
        opacity: disabled ? 0.55 : 1,
        background: selected ? t.accentSoft : "transparent",
      }}
      onMouseEnter={(e) => {
        if (!selected && !disabled) e.currentTarget.style.background = t.hover;
      }}
      onMouseLeave={(e) => {
        if (!selected) e.currentTarget.style.background = "transparent";
      }}
    >
      <span
        style={{
          width: TILE,
          height: TILE,
          flexShrink: 0,
          display: "flex",
          alignItems: "center",
          justifyContent: "center",
        }}
      >
        <BlockArt
          icon={art.icon}
          tone={art.tone}
          footswitch={art.footswitch}
          bodyColor={art.body}
          panelColor={art.panel}
          accentColor={art.accent}
          label={false}
          size={ART_SIZE}
        />
      </span>
      <div style={{ flex: 1, minWidth: 0 }}>
        <div style={{ display: "flex", alignItems: "baseline", gap: t.space4 }}>
          <span
            style={{
              fontFamily: t.serif,
              fontSize: 14,
              color: t.ink,
              whiteSpace: "nowrap",
            }}
          >
            {art.fullName ?? art.name}
          </span>
          <span style={{ color: t.faint }}>·</span>
          <span
            style={{
              fontFamily: t.sans,
              fontSize: 12.5,
              fontWeight: 500,
              color: t.ink2,
              whiteSpace: "nowrap",
            }}
          >
            {paramLabel}
          </span>
        </div>
        {/* Always rendered (even with no note) — both original rows kept this spacer
            present so the label line sits the same distance from the row edge whether
            or not a note follows. */}
        <div style={{ marginTop: t.space2 }}>{note}</div>
      </div>
      {selected && (
        <span style={{ flexShrink: 0 }}>
          <Icon name="check" size={15} stroke={t.accentDeep} strokeWidth={2} />
        </span>
      )}
    </div>
  );
}

/** A shared sub-line shape: warn-tri icon + message, used by both callers'
 *  warning notes (FsParamPick's "may change the tone", SceneLevelPick's scope
 *  warning). Not part of `BlockParamRowProps` — each caller composes its own
 *  `note` from this plus its non-warning cases (a Tag, a plain muted line). */
export function BlockParamWarnNote({ children }: { children: ReactNode }) {
  const { t } = useTheme();
  return (
    <span
      style={{
        display: "inline-flex",
        alignItems: "center",
        gap: t.space3,
        fontFamily: t.sans,
        fontSize: 10.5,
        color: t.sevWarn,
      }}
    >
      <Icon name="warn-tri" size={10} stroke={t.sevWarn} strokeWidth={1.7} />
      {children}
    </span>
  );
}

export default BlockParamRow;
