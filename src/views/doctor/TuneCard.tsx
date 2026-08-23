// src/views/doctor/TuneCard.tsx — the closed-loop balance search for one
// diagnosed sound ("Search for a better balance"). Each round the backend
// (`doctor_tune_step`) proposes knob/band moves from the current baseline,
// applies them live (unsaved), captures and diagnoses the candidate; this card
// shows the moves, the A/B (baseline vs candidate), the MEASURED band change and
// which findings cleared, and asks the player: better → the candidate becomes
// the baseline and the next round runs from it; not better → a different
// variant; save → persist the kept state; stop → discard back to the saved
// preset. Rounds calibrate the response model from what the device measured,
// so later rounds move with this amp's real sensitivities. Holds the app-wide
// apply lock while a candidate sits unsaved on the unit (one edit buffer).

import { useEffect, useId, useRef, useState } from "react";
import type { CSSProperties } from "react";

import { useTheme } from "../../theme/ThemeContext";
import { Icon } from "../../ui/Icon";
import { Tag } from "../../ui/Tag";
import { Spinner } from "../../ui/Spinner";
import { Button } from "../../ui/primitives";
import { BackupAckLabel } from "../../ui/BackupAckLabel";
import { doctorSave, doctorTuneEnd, doctorTuneStep } from "../../lib/invoke";
import { patchLibraryAfterDoctorSave } from "../level/libraryScan";
import { ABAudition } from "./ABAudition";
import { DiagnosisChip } from "./DiagnosisChip";
import { MeasuredChange } from "./MeasuredChange";
import { PlanMoves } from "./PlanMoves";
import { useApplyLock } from "./applyLock";
import { findingLabel } from "./planModel";
import type { DoctorStimulus } from "./PrescriptionCard";
import { doctorCard, isPossible, possibleLabel } from "./severity";
import type {
  DoctorApplyJob,
  DoctorDiag,
  DoctorSoundResult,
  DoctorTuneStep,
  FootswitchInfo,
  GraphNode,
  SceneNodeOverlay,
  TuneDecision,
} from "../../lib/types";

type Phase = "idle" | "running" | "step" | "saved";

export interface TuneCardProps {
  sound: DoctorSoundResult;
  listIndex: number;
  presetName: string;
  nodes: GraphNode[];
  footswitches: FootswitchInfo[];
  sceneOverlay?: SceneNodeOverlay[];
  stimulus?: DoctorStimulus;
}

const DEFAULT_STIMULUS: DoctorStimulus = {
  topologyId: null,
  calibrationLufs: null,
  profileId: null,
};

const INTRO =
  "Runs rounds on the unit: try a move, listen, measure, then you decide — better, not better, or save. Each round learns how far this amp's knobs really move, and it keeps going past the coarse findings until every band sits within ±1 dB of the reference balance or the knobs can't get closer.";

/** One round's verdict row in the history strip. */
interface RoundMark {
  round: number;
  verdict: "better" | "worse";
  line: string;
}

export function TuneCard({
  sound,
  listIndex,
  presetName,
  nodes,
  footswitches,
  sceneOverlay = [],
  stimulus = DEFAULT_STIMULUS,
}: TuneCardProps) {
  const { t } = useTheme();
  const [phase, setPhase] = useState<Phase>("idle");
  const [step, setStep] = useState<DoctorTuneStep | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [acked, setAcked] = useState(false);
  const [history, setHistory] = useState<RoundMark[]>([]);

  const cardId = useId();
  const lock = useApplyLock();
  const lockedByOther =
    lock.activeCard !== null && lock.activeCard.id !== cardId;

  // Unmount with a candidate on the unit → end the loop with a discard so the
  // edit buffer and the app-wide lock never strand (the PrescriptionCard rule).
  const activeRef = useRef(false);
  const { discardIfMine } = lock;
  useEffect(() => {
    return () => {
      if (activeRef.current) {
        void doctorTuneEnd(listIndex, true).catch(() => undefined);
      }
      discardIfMine(cardId, listIndex);
    };
  }, [discardIfMine, cardId, listIndex]);

  const ctx: DoctorApplyJob = {
    listIndex,
    name: presetName,
    ops: [],
    topologyId: stimulus.topologyId,
    calibrationLufs: stimulus.calibrationLufs,
    profileId: stimulus.profileId,
    scene: sound.scene,
    footswitch: sound.footswitch,
    nodes,
    footswitches,
    sceneOverlay,
  };

  async function runStep(decision: TuneDecision) {
    setError(null);
    setPhase("running");
    if (decision === "start") {
      lock.acquire(cardId, listIndex);
      activeRef.current = true;
      setHistory([]);
    } else if (step) {
      setHistory((h) => [
        ...h,
        {
          round: step.round,
          verdict: decision,
          line: step.candidate?.rx.detail ?? "",
        },
      ]);
    }
    try {
      const res = await doctorTuneStep(ctx, decision);
      setStep(res);
      setPhase("step");
    } catch (e) {
      setError(e instanceof Error ? e.message : "The round couldn't run.");
      // A failed round restores the stored preset backend-side; the session is
      // gone, so release everything and go back to idle.
      activeRef.current = false;
      lock.release(cardId);
      setPhase("idle");
    }
  }

  async function runSave(ops: DoctorTuneStep["ops"]) {
    setError(null);
    setPhase("running");
    try {
      await doctorSave(listIndex, presetName, sound.scene, ops, sceneOverlay);
      patchLibraryAfterDoctorSave(listIndex, sound.scene, ops);
      await doctorTuneEnd(listIndex, false);
      activeRef.current = false;
      lock.release(cardId);
      setPhase("saved");
    } catch (e) {
      setError(e instanceof Error ? e.message : "Couldn't save to the preset.");
      setPhase("step");
    }
  }

  async function runStop() {
    setError(null);
    setPhase("running");
    try {
      await doctorTuneEnd(listIndex, true);
    } catch (e) {
      setError(e instanceof Error ? e.message : "Couldn't restore the preset.");
    } finally {
      activeRef.current = false;
      lock.release(cardId);
      setStep(null);
      setHistory([]);
      setAcked(false);
      setPhase("idle");
    }
  }

  const card = doctorCard(t, {
    border: phase === "saved" ? t.good : undefined,
  });
  const label: CSSProperties = {
    fontFamily: t.sans,
    fontSize: t.fsLabel,
    lineHeight: 1.5,
    color: t.mutedInk,
  };
  const errorBlock =
    error != null ? (
      <div style={{ ...label, color: t.warn, marginTop: t.space4 }}>
        {error}
      </div>
    ) : null;

  const header = (
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
        Balance search
      </span>
      <Tag tone="accent" uppercase>
        Round by round
      </Tag>
    </div>
  );

  if (phase === "saved") {
    return (
      <div data-testid="tune-card">
        {header}
        <div style={card}>
          <div style={{ display: "flex", alignItems: "center", gap: t.space4 }}>
            <Icon name="check" size={14} stroke={t.good} />
            <span
              style={{ fontFamily: t.serif, fontSize: t.fsName, color: t.ink }}
            >
              Saved to the preset.
            </span>
          </div>
          <div style={{ ...label, marginTop: t.space3 }}>
            Run the check again to see the new numbers — it waits for the unit
            to commit the save first.
          </div>
        </div>
      </div>
    );
  }

  if (phase === "idle") {
    return (
      <div data-testid="tune-card">
        {header}
        <div style={card}>
          <div
            style={{ fontFamily: t.serif, fontSize: t.fsName, color: t.ink }}
          >
            Search for a better balance
          </div>
          <div style={{ ...label, color: t.ink2, marginTop: t.space2 }}>
            {INTRO}
          </div>
          {errorBlock}
          <div style={{ marginTop: t.space6 }}>
            <Button
              variant="primary"
              small
              icon="refresh"
              disabled={lockedByOther}
              onClick={() => {
                void runStep("start");
              }}
            >
              Start the search
            </Button>
            {lockedByOther && (
              <div style={{ ...label, marginTop: t.space3 }}>
                Finish the applied fix first — the unit holds one unsaved edit
                at a time.
              </div>
            )}
          </div>
        </div>
      </div>
    );
  }

  if (phase === "running") {
    return (
      <div data-testid="tune-card">
        {header}
        <div style={card}>
          <div style={{ display: "flex", alignItems: "center", gap: t.space4 }}>
            <Spinner size={14} stroke={t.accent} strokeWidth={1.8} />
            <span
              style={{ fontFamily: t.sans, fontSize: t.fsBody, color: t.ink2 }}
            >
              {step
                ? `Round ${String(step.round + 1)} — applying and listening…`
                : "Listening to the saved sound, then round 1…"}
            </span>
          </div>
          <div style={{ ...label, marginTop: t.space3 }}>
            About 15 seconds per round. Nothing is saved until you say so.
          </div>
        </div>
      </div>
    );
  }

  // phase === "step"
  if (!step) return null;
  const candidate = step.candidate;
  const hasBaselineEdits = step.baselineOps.length > 0;

  const findingsRow = (title: string, keys: string[], diags: DoctorDiag[]) =>
    keys.length > 0 ? (
      <div
        style={{
          display: "flex",
          alignItems: "center",
          gap: t.space3,
          flexWrap: "wrap",
        }}
      >
        <span style={{ ...label, minWidth: 64 }}>{title}</span>
        {keys.map((k) => {
          const d = diags.find((x) => x.key === k);
          return d ? (
            <DiagnosisChip
              key={k}
              label={possibleLabel(d)}
              sev={d.sev}
              possible={isPossible(d)}
            />
          ) : (
            <Tag key={k} tone="neutral">
              {findingLabel(k, diags)}
            </Tag>
          );
        })}
      </div>
    ) : null;

  return (
    <div data-testid="tune-card">
      {header}
      <div style={card}>
        <div
          style={{
            display: "flex",
            alignItems: "center",
            gap: t.space4,
            flexWrap: "wrap",
          }}
        >
          <span
            style={{ fontFamily: t.serif, fontSize: t.fsName, color: t.ink }}
          >
            {candidate
              ? `Round ${String(step.round)}`
              : step.status === "converged"
                ? "Nothing left to fix"
                : "No further suggestion"}
          </span>
          {candidate && (
            <span
              style={{
                fontFamily: t.mono,
                fontSize: t.fsData2,
                letterSpacing: t.lsTag,
                textTransform: "uppercase",
                color: t.accentDeep,
                background: t.accentSoft,
                padding: `${String(t.space2)}px ${String(t.space4)}px`,
                borderRadius: t.rPill,
              }}
            >
              Applied to the unit · not saved
            </span>
          )}
        </div>
        <div
          style={{ ...label, color: t.ink2, marginTop: t.space2 }}
          data-testid="tune-message"
        >
          {step.message}
        </div>

        {history.length > 0 && (
          <div
            style={{
              display: "flex",
              flexDirection: "column",
              gap: t.space2,
              marginTop: t.space4,
            }}
            data-testid="tune-history"
          >
            {history.map((h) => (
              <div
                key={h.round}
                style={{
                  ...label,
                  fontFamily: t.mono,
                  fontSize: t.fsData2,
                  color: h.verdict === "better" ? t.good : t.mutedInk,
                }}
              >
                Round {h.round} · {h.verdict === "better" ? "kept" : "rejected"}
                {h.line ? ` · ${h.line}` : ""}
              </div>
            ))}
          </div>
        )}

        {candidate && (
          <div style={{ marginTop: t.space5 }}>
            <PlanMoves moves={candidate.moves} />
            <div
              style={{ ...label, marginTop: t.space4 }}
              data-testid="tune-balance-error"
            >
              Distance to the reference balance:{" "}
              {candidate.balanceErrorBeforeDb.toFixed(1)} →{" "}
              <span
                style={{
                  color:
                    candidate.balanceErrorAfterDb <
                    candidate.balanceErrorBeforeDb
                      ? t.good
                      : t.ink2,
                  fontWeight: 600,
                }}
              >
                {candidate.balanceErrorAfterDb.toFixed(1)} dB
              </span>{" "}
              beyond ±1 dB (measured).
            </div>
          </div>
        )}

        {candidate && step.candidateClip && (
          <div style={{ marginTop: t.space6 }}>
            <ABAudition
              beforeClip={step.baselineClip}
              afterClip={step.candidateClip}
            />
            {step.measured && <MeasuredChange measured={step.measured} />}
          </div>
        )}

        <div
          style={{
            display: "flex",
            flexDirection: "column",
            gap: t.space3,
            marginTop: t.space5,
          }}
        >
          {findingsRow("Cleared", step.cleared, step.baselineDiags)}
          {findingsRow(
            "Still",
            step.remained,
            candidate ? step.candidateDiags : step.baselineDiags,
          )}
          {findingsRow("New", step.introduced, step.candidateDiags)}
        </div>

        {step.note && step.note.learned.length > 0 && (
          <div style={{ ...label, marginTop: t.space4 }}>
            Learned so far:{" "}
            {step.note.learned
              .map(([l, s]) => `${l} ×${s.toFixed(2)}`)
              .join(", ")}
            .
          </div>
        )}

        {errorBlock}

        <div
          style={{
            marginTop: t.space6,
            paddingTop: t.space6,
            borderTop: `0.5px solid ${t.hairline}`,
            display: "flex",
            alignItems: "center",
            justifyContent: "space-between",
            gap: t.space6,
            flexWrap: "wrap",
          }}
        >
          <BackupAckLabel checked={acked} onChange={setAcked} />
          <div
            style={{
              display: "flex",
              gap: t.space4,
              flexShrink: 0,
              flexWrap: "wrap",
            }}
          >
            <Button
              variant="ghost"
              small
              onClick={() => {
                void runStop();
              }}
            >
              Stop & discard
            </Button>
            {candidate ? (
              <>
                <Button
                  variant="ghost"
                  small
                  onClick={() => {
                    void runStep("worse");
                  }}
                >
                  Not better — try another
                </Button>
                <Button
                  variant="ghost"
                  small
                  icon="check"
                  onClick={() => {
                    void runStep("better");
                  }}
                >
                  Better — next round
                </Button>
                <Button
                  variant="primary"
                  small
                  icon="save"
                  disabled={!acked}
                  onClick={() => {
                    void runSave(step.ops);
                  }}
                >
                  Save this
                </Button>
              </>
            ) : (
              hasBaselineEdits && (
                <Button
                  variant="primary"
                  small
                  icon="save"
                  disabled={!acked}
                  onClick={() => {
                    void runSave(step.baselineOps);
                  }}
                >
                  Save what I kept
                </Button>
              )
            )}
          </div>
        </div>
      </div>
    </div>
  );
}

export default TuneCard;
