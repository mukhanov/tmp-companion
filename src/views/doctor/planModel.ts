// src/views/doctor/planModel.ts — pure helpers behind the balance-plan card
// (TonePlanCard.tsx) and the measured-change readout (MeasuredChange.tsx): the
// per-block grouping of moves, the outcome sentence, and the measured band
// line. No React, no device I/O — unit-tested directly.

import { signedDb } from "../../lib/format";
import type {
  DoctorApplyMeasure,
  DoctorDiag,
  DoctorPlanMove,
  DoctorTonePlan,
  PlaybackLevel,
} from "../../lib/types";

/** Player-facing label for a finding key, from the sound's own diagnoses
 *  (the backend's labels) with a capitalized-key fallback. */
export function findingLabel(key: string, diags: DoctorDiag[]): string {
  const d = diags.find((x) => x.key === key);
  if (d) return d.label;
  return key.charAt(0).toUpperCase() + key.slice(1);
}

export const LEVEL_WORDING: Record<PlaybackLevel, string> = {
  quiet: "at any volume",
  rehearsal: "from rehearsal volume",
  stage: "at stage volume only",
};

export interface PlanBlockGroup {
  nodeId: string;
  model: string;
  blockName: string;
  rows: DoctorPlanMove[];
}

/** Group the moves per block, in first-seen order — one art tile per block. */
export function groupByBlock(moves: DoctorPlanMove[]): PlanBlockGroup[] {
  const out: PlanBlockGroup[] = [];
  for (const m of moves) {
    const g = out.find((x) => x.nodeId === m.nodeId);
    if (g) g.rows.push(m);
    else
      out.push({
        nodeId: m.nodeId,
        model: m.model,
        blockName: m.blockName,
        rows: [m],
      });
  }
  return out;
}

/** The one-sentence outcome: what clears, what remains (and from which
 *  volume). */
export function planOutcomeSentence(
  plan: DoctorTonePlan,
  diags: DoctorDiag[],
): string {
  const parts: string[] = [];
  if (plan.clears.length > 0) {
    parts.push(
      `Predicted to clear ${plan.clears
        .map((k) => findingLabel(k, diags))
        .join(", ")} at any volume.`,
    );
  }
  if (plan.remains.length > 0) {
    parts.push(
      `${plan.remains
        .map(
          (r) => `${findingLabel(r.key, diags)} ${LEVEL_WORDING[r.fromLevel]}`,
        )
        .join(", ")} — still expected after this.`,
    );
  }
  return parts.join(" ");
}

/** Bands that moved less than this (dB) are left out of the measured line. */
export const MEASURED_MIN_DB = 0.5;

/** The measured band-change line; no band over the floor → a "nothing
 *  moved" sentence. */
export function measuredBandLine(m: DoctorApplyMeasure): string {
  const parts: string[] = [];
  m.bandLabels.forEach((label, i) => {
    const d = i < m.deltaDb.length ? m.deltaDb[i] : 0;
    if (Math.abs(d) >= MEASURED_MIN_DB) parts.push(`${label} ${signedDb(d)}`);
  });
  if (parts.length === 0) {
    return `no band moved more than ${String(MEASURED_MIN_DB)} dB`;
  }
  return parts.join(" · ");
}
