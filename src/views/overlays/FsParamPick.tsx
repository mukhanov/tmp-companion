// src/views/overlays/FsParamPick.tsx — the Set up step's per-footswitch
// "which block parameter to level" picker.
//
// A footswitch can act on several blocks, and a block exposes several levelable
// params. Adjusting a LOUDNESS-only param (level / output / mix…) changes volume
// without touching the sound; gain / tone / drive also change the TONE — which
// leveling tries to avoid. So the default is the first loudness-only candidate, the
// menu marks it "Recommended", and tone-affecting candidates are flagged. With a
// single candidate there is nothing to choose: the control renders read-only (it
// still shows what is being adjusted).
//
// Click-only. The menu reuses the wizard's card-portaled dropdown (PickPortalMenu via
// DialogCardCtx) so it flips ABOVE near the fixed frame's bottom edge — same machinery
// as the sibling instrument/target `Pick`.

import { useContext } from "react";

import { useTheme } from "../../theme/ThemeContext";
import { Icon } from "../../ui/Icon";
import { Tag } from "../../ui/Tag";
import { BlockArt } from "../../ui/BlockArt";
import { blockArtTile } from "../../models/blockArt";
import { defaultParamIndex, paramLabel } from "../level/leveling";
import { DialogCardCtx } from "./wizardContext";
import { PickPortalMenu } from "./PickPortalMenu";
import { usePickAnchor } from "./usePickAnchor";
import { pickTriggerBorder } from "./pickTriggerChrome";
import { BlockParamRow, BlockParamWarnNote } from "./BlockParamRow";
import type { LevelParamCandidate } from "../../lib/types";

export interface FsParamPickProps {
  /** The footswitch's levelable-parameter candidates (the backend `level_params`). */
  params: LevelParamCandidate[];
  /** Selected candidate index. */
  index: number;
  onChange: (i: number) => void;
}

export function FsParamPick({ params, index, onChange }: FsParamPickProps) {
  const { t } = useTheme();
  const cardRef = useContext(DialogCardCtx);

  // DANGER-rule guard (danger.md's Pick/BlockPick trap): a stored `index` the current
  // `params` doesn't cover (out of range, or -1 = "no classifiable default yet") must
  // NEVER silently render as `params[0]` — that showed one control while a different
  // one (or none) was actually about to be swept. A single un-picked candidate still
  // forces an explicit click (interactive), not a silent auto-select.
  const hasSelection = index >= 0 && index < params.length;
  const interactive = params.length > 1 || !hasSelection;
  const defIdx = defaultParamIndex(params);

  const { open, anchor, pos, cardEl, menuRef, triggerRef, openMenu, close } =
    usePickAnchor(cardRef, { guard: () => interactive });
  const pick = (i: number) => {
    close();
    onChange(i);
  };

  if (params.length === 0) return null;
  const cur = hasSelection ? params[index] : null;
  const curArt = cur ? blockArtTile(cur.fender_id) : null;

  // Built only while the menu is open — the rows (and their per-candidate
  // `blockArtTile` lookups) are otherwise never rendered.
  const optionRows = open
    ? params.map((c, i) => {
        const on = hasSelection && i === index;
        const rec = i === defIdx;
        // Every candidate the backend offers is already a real (non-"other") loudness
        // control — only a wet/dry mix ALSO moves the effect's presence, so that's the
        // one class flagged "may change the tone" rather than "loudness only".
        const loud = c.class !== "wet_mix";
        return (
          <BlockParamRow
            key={`${c.node_id}:${c.parameter_id}`}
            art={blockArtTile(c.fender_id)}
            paramLabel={paramLabel(c.parameter_id)}
            selected={on}
            onPick={() => {
              pick(i);
            }}
            note={
              rec ? (
                <Tag tone="good" uppercase>
                  Recommended · loudness only
                </Tag>
              ) : loud ? (
                <span
                  style={{
                    fontFamily: t.sans,
                    fontSize: 10.5,
                    color: t.mutedInk,
                  }}
                >
                  changes loudness only
                </span>
              ) : (
                <BlockParamWarnNote>may change the tone</BlockParamWarnNote>
              )
            }
          />
        );
      })
    : [];

  return (
    <div
      ref={triggerRef}
      style={{ position: "relative", width: "100%", minWidth: 0 }}
    >
      <div
        onClick={openMenu}
        title={
          !hasSelection
            ? "Choose which block parameter is leveled"
            : interactive
              ? "Choose which block parameter is leveled"
              : "Only one option — nothing to choose"
        }
        style={{
          display: "flex",
          alignItems: "center",
          gap: t.space3,
          height: 26,
          padding: `0 ${String(t.space4)}px`,
          boxSizing: "border-box",
          border: pickTriggerBorder(t, { open, warn: !hasSelection }),
          borderRadius: 6,
          background: interactive ? t.bg : t.bgAlt,
          cursor: interactive ? "pointer" : "default",
          whiteSpace: "nowrap",
          overflow: "hidden",
        }}
      >
        <span
          style={{
            width: 16,
            height: 16,
            flexShrink: 0,
            display: "flex",
            alignItems: "center",
            justifyContent: "center",
          }}
        >
          {cur && curArt ? (
            <BlockArt
              icon={curArt.icon}
              tone={curArt.tone}
              footswitch={curArt.footswitch}
              bodyColor={curArt.body}
              panelColor={curArt.panel}
              accentColor={curArt.accent}
              label={false}
              size={16}
            />
          ) : (
            <Icon
              name="warn-tri"
              size={13}
              stroke={t.sevWarn}
              strokeWidth={1.7}
            />
          )}
        </span>
        <span
          style={{
            flex: 1,
            minWidth: 0,
            fontFamily: t.sans,
            fontSize: 11,
            color: !hasSelection
              ? t.sevWarn
              : interactive
                ? t.ink2
                : t.mutedInk,
            overflow: "hidden",
            textOverflow: "ellipsis",
          }}
        >
          {cur ? paramLabel(cur.parameter_id) : "Choose a parameter"}
        </span>
        {interactive && (
          <Icon
            name="chev-down"
            size={11}
            stroke={open ? t.accentDeep : t.faint}
          />
        )}
      </div>

      {open && anchor && cardEl && (
        <PickPortalMenu
          cardEl={cardEl}
          menuRef={menuRef}
          left={pos ? pos.left : anchor.left}
          top={pos ? pos.top : anchor.below}
          visible={pos != null}
          minWidth={Math.max(anchor.width, 268)}
          onClose={close}
        >
          <div
            style={{
              padding: `${String(t.space2)}px ${String(t.space4)}px ${String(t.space4)}px`,
              fontFamily: t.mono,
              fontSize: 9,
              letterSpacing: "0.12em",
              textTransform: "uppercase",
              color: t.faint,
            }}
          >
            Level this footswitch by adjusting
          </div>
          {optionRows}
        </PickPortalMenu>
      )}
    </div>
  );
}

export default FsParamPick;
