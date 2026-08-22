// src/views/doctor/TonePlanCard.tsx — the "Balance plan" card: the backend's
// `DoctorSoundResult.plan` (doctor_plan.rs) rendered as a per-block knob table
// ("'65 Twin Reverb · Bass 6.0 → 4.0"), a before→predicted band picture, and
// the honest re-diagnosed outcome (what clears, what still fires and from which
// volume). The apply/A-B/save lifecycle is PrescriptionCard's — the plan's
// `rx` (every move as a `param` op) is handed to it, with this card's table as
// the body, so the plan gets the same one-unsaved-edit lock, BEFORE-clip cache
// and measured-change readout as every other prescription.

import type { ReactNode } from "react";

import { useTheme } from "../../theme/ThemeContext";
import { Tag } from "../../ui/Tag";
import { signedDb } from "../../lib/format";
import { PlanMoves } from "./PlanMoves";
import { PrescriptionCard, type DoctorStimulus } from "./PrescriptionCard";
import { planOutcomeSentence } from "./planModel";
import { sevTone } from "./severity";
import type {
  DoctorSoundResult,
  DoctorTonePlan,
  FootswitchInfo,
  GraphNode,
  SceneNodeOverlay,
} from "../../lib/types";

const ESTIMATE_LINE =
  "Estimated from a nominal tone-stack model, not a measurement — Apply, listen to the A/B, and trust the measured numbers over the prediction.";

/** Compact before→predicted band picture: per band two bars from a zero
 *  line (before faint, predicted accent), on a ±12 dB scale. */
function PlanBands({ plan }: { plan: DoctorTonePlan }) {
  const { t } = useTheme();
  const n = plan.bandLabels.length;
  const W = 200;
  const H = 44;
  const mid = H / 2;
  const scale = (db: number) => {
    const clamped = Math.max(-12, Math.min(12, db));
    return (clamped / 12) * (mid - 3);
  };
  const colW = W / Math.max(1, n);
  return (
    <div
      style={{ display: "flex", alignItems: "center", gap: t.space5 }}
      aria-label="Band balance before and predicted after the plan"
    >
      <svg
        width={W}
        height={H}
        viewBox={`0 0 ${String(W)} ${String(H)}`}
        style={{ flexShrink: 0, overflow: "visible" }}
        role="img"
      >
        <line
          x1={0}
          x2={W}
          y1={mid}
          y2={mid}
          stroke={t.hairline}
          strokeWidth={0.5}
        />
        {plan.bandLabels.map((label, i) => {
          const before = i < plan.beforeDb.length ? plan.beforeDb[i] : 0;
          const after = i < plan.predictedDb.length ? plan.predictedDb[i] : 0;
          const x0 = i * colW + colW * 0.2;
          const bw = colW * 0.25;
          const hb = scale(before);
          const ha = scale(after);
          return (
            <g key={label}>
              <rect
                x={x0}
                y={hb >= 0 ? mid - hb : mid}
                width={bw}
                height={Math.max(1, Math.abs(hb))}
                fill={t.faint}
                rx={1}
              />
              <rect
                x={x0 + bw + 2}
                y={ha >= 0 ? mid - ha : mid}
                width={bw}
                height={Math.max(1, Math.abs(ha))}
                fill={t.accent}
                rx={1}
              />
            </g>
          );
        })}
      </svg>
      <div
        style={{
          display: "flex",
          flexDirection: "column",
          gap: 2,
          fontFamily: t.mono,
          fontSize: t.fsData2,
          color: t.mutedInk,
          minWidth: 0,
        }}
      >
        {plan.bandLabels.map((label, i) => {
          const before = i < plan.beforeDb.length ? plan.beforeDb[i] : 0;
          const after = i < plan.predictedDb.length ? plan.predictedDb[i] : 0;
          if (Math.abs(after - before) < 0.5) return null;
          return (
            <span key={label} style={{ whiteSpace: "nowrap" }}>
              {label} {signedDb(before)} → {signedDb(after)} dB
            </span>
          );
        })}
      </div>
    </div>
  );
}

export interface TonePlanCardProps {
  sound: DoctorSoundResult;
  plan: DoctorTonePlan;
  listIndex: number;
  presetName: string;
  nodes: GraphNode[];
  footswitches: FootswitchInfo[];
  /** The diagnosed scene's node overlay — threaded into the A/B capture. */
  sceneOverlay?: SceneNodeOverlay[];
  stimulus?: DoctorStimulus;
}

export function TonePlanCard({
  sound,
  plan,
  listIndex,
  presetName,
  nodes,
  footswitches,
  sceneOverlay,
  stimulus,
}: TonePlanCardProps) {
  const { t } = useTheme();
  const good = sevTone(t, "ok");
  const outcome = planOutcomeSentence(plan, sound.diags);
  const loud = plan.loudnessDeltaDb;
  const loudLine =
    Math.abs(loud) >= 0.5
      ? `About ${signedDb(loud)} dB ${loud > 0 ? "louder" : "quieter"} overall — re-level it from the Level tab after saving.`
      : null;

  const body: ReactNode = (
    <div
      style={{
        display: "flex",
        flexDirection: "column",
        gap: t.space5,
        marginTop: t.space5,
      }}
    >
      <PlanMoves moves={plan.moves} />
      <PlanBands plan={plan} />
      <div
        style={{
          fontFamily: t.sans,
          fontSize: t.fsLabel,
          lineHeight: 1.5,
          color: plan.clears.length > 0 ? good.fg : t.ink2,
        }}
      >
        {outcome}
      </div>
      {loudLine && (
        <div
          style={{
            fontFamily: t.sans,
            fontSize: t.fsLabel,
            lineHeight: 1.5,
            color: t.mutedInk,
          }}
        >
          {loudLine}
        </div>
      )}
      <div
        style={{
          fontFamily: t.sans,
          fontSize: t.fsLabel,
          lineHeight: 1.5,
          color: t.mutedInk,
        }}
      >
        {ESTIMATE_LINE}
      </div>
    </div>
  );

  return (
    <div data-testid="tone-plan-card">
      <div
        style={{
          display: "flex",
          alignItems: "center",
          gap: t.space4,
          marginBottom: t.space3,
        }}
      >
        <span
          style={{
            fontFamily: t.sans,
            fontSize: t.fsLabel,
            fontWeight: 600,
            color: t.ink2,
          }}
        >
          Balance plan
        </span>
        <Tag tone="neutral" uppercase>
          Estimated
        </Tag>
      </div>
      <PrescriptionCard
        rx={{
          ...plan.rx,
          detail:
            "Turn these on the blocks already in the chain — no new block, no CPU change.",
        }}
        listIndex={listIndex}
        presetName={presetName}
        soundScene={sound.scene}
        soundFootswitch={sound.footswitch}
        nodes={nodes}
        footswitches={footswitches}
        sceneOverlay={sceneOverlay}
        stimulus={stimulus}
        badge="Your own knobs"
      >
        {body}
      </PrescriptionCard>
    </div>
  );
}

export default TonePlanCard;
