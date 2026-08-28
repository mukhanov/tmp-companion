// src/views/doctor/PlanMoves.tsx — the per-block knob-move table shared by the
// one-shot balance plan (TonePlanCard) and the tune loop (TuneCard): one row per
// block (its art tile + catalog name) with every "Knob from → to" move on it.

import { useTheme } from "../../theme/ThemeContext";
import { BlockArt } from "../../ui/BlockArt";
import { nodeTileArt } from "../../models/blockArt";
import { isComboBid } from "../../models/catalog";
import { groupByBlock } from "./planModel";
import type { DoctorPlanMove } from "../../lib/types";

export interface PlanMovesProps {
  moves: DoctorPlanMove[];
}

export function PlanMoves({ moves }: PlanMovesProps) {
  const { t } = useTheme();
  const groups = groupByBlock(moves);
  return (
    <div
      style={{ display: "flex", flexDirection: "column", gap: t.space4 }}
      data-testid="plan-moves"
    >
      {groups.map((g) => {
        const art = nodeTileArt(g.model, undefined, isComboBid(g.model));
        return (
          <div
            key={g.nodeId}
            style={{ display: "flex", alignItems: "flex-start", gap: t.space5 }}
          >
            <div style={{ flexShrink: 0 }}>
              <BlockArt
                icon={art.icon}
                tone={art.tone}
                lab={art.lab}
                footswitch={art.footswitch}
                bodyColor={art.body}
                accentColor={art.accent}
                panelColor={art.panel}
                size={34}
                label={false}
              />
            </div>
            <div style={{ flex: 1, minWidth: 0 }}>
              <div
                style={{
                  fontFamily: t.serif,
                  fontSize: t.fsName,
                  color: t.ink,
                  lineHeight: 1.3,
                }}
              >
                {g.blockName}
              </div>
              <div
                style={{
                  display: "flex",
                  flexWrap: "wrap",
                  gap: `${String(t.space2)}px ${String(t.space6)}px`,
                  marginTop: t.space2,
                }}
              >
                {g.rows.map((m) => (
                  <span
                    key={m.param}
                    style={{
                      fontFamily: t.mono,
                      fontSize: t.fsControl,
                      color: t.ink2,
                      fontVariantNumeric: "tabular-nums",
                      whiteSpace: "nowrap",
                    }}
                  >
                    <span style={{ color: t.mutedInk }}>{m.controlLabel} </span>
                    {m.fromLabel}
                    <span style={{ color: t.accentDeep }}> → </span>
                    <span style={{ color: t.ink, fontWeight: 600 }}>
                      {m.toLabel}
                    </span>
                    {m.why !== "" && (
                      <span
                        style={{
                          fontFamily: t.sans,
                          color: t.mutedInk,
                          whiteSpace: "normal",
                        }}
                      >
                        {" "}
                        — {m.why}
                      </span>
                    )}
                  </span>
                ))}
              </div>
            </div>
          </div>
        );
      })}
    </div>
  );
}

export default PlanMoves;
