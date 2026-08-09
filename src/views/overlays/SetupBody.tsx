// src/views/overlays/SetupBody.tsx — wizard step 2, "Set up".
//
// Everything chosen in the LIST (the scene tree) WILL be leveled — this step never
// re-gates inclusion. Its single job is to set each sound's INSTRUMENT + TARGET:
//   • A top "Apply to" bar is a brush that writes to every row at once — or, when the
//     user ticks a few rows, to just those. Ticking is a bulk-edit convenience only.
//   • Each row also carries its OWN instrument + target pickers.
// On "Level N sounds" it hands the flow one SetupChoice per option. The footer's
// "I've backed up with Pro Control" checkbox gates the button (an inline backup
// acknowledgment — there is no separate Back-up step). Re-level skips the ack (the
// user already acknowledged when the initial run started).
//
// History (do not reintroduce): an earlier build put inclusion checkboxes here,
// forcing users to pick sounds twice (list + dialog). The list is the single place
// you choose WHAT to level; this step only chooses HOW.

import { useEffect, useMemo, useRef, useState, type ReactNode } from "react";

import { useTheme, useStyles } from "../../theme/ThemeContext";
import { Icon } from "../../ui/Icon";
import { Button, Toggle } from "../../ui/primitives";
import { Tag } from "../../ui/Tag";
import { BackupAckLabel } from "../../ui/BackupAckLabel";
import { SetupGroupHeader } from "../../ui/SetupGroupHeader";
import { PresetOptionRow } from "../../ui/PresetOptionRow";
import { ApplyToBar } from "../../ui/ApplyToBar";
import { usePickedRows } from "../../lib/usePickedRows";
import { WizardFooter, WizTitle } from "./WizardShell";
import { ByEarChip } from "./ByEarChip";
import { Pick, type PickOption } from "./Pick";
import { FsParamPick } from "./FsParamPick";
import { SceneLevelPick } from "./SceneLevelPick";
import { useSceneHandles } from "../level/useSceneHandles";
import {
  defaultParamIndex,
  footswitchNameForCandidate,
  instCalState,
  setupRowHookKey,
  targetFromCandidate,
  verifyFootswitchTarget,
} from "../level/leveling";
import type {
  SetupOption,
  SetupChoice,
  SceneHandlePick,
} from "../level/leveling";

export type { SetupChoice };

/** The "calibrate" word — an inviting next-step cue. Dotted terracotta underline
 *  that solidifies on hover; clicking jumps to Settings → Instruments (`onCalibrate`,
 *  threaded from App down through LevelView/LevelingWizard). Click-only app, so no
 *  keyboard handler — the click IS the affordance. Intentional detour: it unmounts
 *  LevelView and discards any in-progress wizard setup. */
function CalibrateCue({
  children,
  onCalibrate,
}: {
  children: ReactNode;
  onCalibrate?: () => void;
}) {
  const { t } = useTheme();
  const [hover, setHover] = useState(false);
  return (
    <span
      title="Calibrate instruments in Settings"
      onClick={onCalibrate}
      onMouseEnter={() => {
        setHover(true);
      }}
      onMouseLeave={() => {
        setHover(false);
      }}
      style={{
        color: t.accentDeep,
        fontWeight: 500,
        cursor: "pointer",
        textDecoration: "underline",
        textDecorationStyle: "dotted",
        textDecorationColor: hover ? t.accentDeep : t.warnBorder,
        textUnderlineOffset: "2.5px",
      }}
    >
      {children}
    </span>
  );
}

/** Quiet good → better → best caption beneath the apply-to-all instrument picker.
 *  `cal` removes the element entirely (no reserved height) so the list below reclaims
 *  the space. Not a warning — muted body with a single accent cue on "calibrate". */
function InstrumentNudge({
  state,
  onCalibrate,
}: {
  state: "none" | "uncal" | "cal";
  onCalibrate?: () => void;
}) {
  const { t } = useTheme();
  if (state === "cal") return null;
  return (
    <div
      aria-live="polite"
      style={{
        marginTop: t.space4,
        fontFamily: t.sans,
        fontSize: 12,
        lineHeight: 1.45,
        color: t.mutedInk,
      }}
    >
      {state === "none" ? (
        <span>
          Set an instrument for better results —{" "}
          <CalibrateCue onCalibrate={onCalibrate}>calibrate</CalibrateCue> it
          for the best.
        </span>
      ) : (
        <span>
          <CalibrateCue onCalibrate={onCalibrate}>Calibrate</CalibrateCue> this
          instrument for the best results.
        </span>
      )}
    </div>
  );
}

/** Onboarding nudge toward Tier-2 calibration (capture-as-stimulus) — a small
 *  dismissable banner shown once per wizard open, only while the chosen instrument
 *  is a real, uncalibrated profile (an unset/"None" instrument or an already-
 *  calibrated one shows nothing). Local `dismissed` state, so re-entering the Set
 *  up step (a fresh SetupBody mount) shows it again — cheap enough not to thread
 *  through the flow. No navigation coupling: plain text points at Settings. */
function CalibrationOnboardingBanner({ show }: { show: boolean }) {
  const { t } = useTheme();
  const [dismissed, setDismissed] = useState(false);
  if (!show || dismissed) return null;
  return (
    <div
      role="status"
      style={{
        flexShrink: 0,
        display: "flex",
        alignItems: "flex-start",
        gap: t.space4,
        margin: `${String(t.space6)}px ${String(t.space10)}px 0`,
        padding: `${String(t.space4)}px ${String(t.space5)}px`,
        borderRadius: t.rCard,
        border: `0.5px solid ${t.hairlineStrong}`,
        background: t.bgAlt,
      }}
    >
      <span style={{ display: "flex", flexShrink: 0, marginTop: t.space1 }}>
        <Icon name="info" size={14} stroke={t.accentDeep} strokeWidth={1.5} />
      </span>
      <span
        style={{
          flex: 1,
          fontFamily: t.sans,
          fontSize: 12,
          lineHeight: 1.45,
          color: t.ink2,
        }}
      >
        Level with your own guitar — a 2-minute calibration makes leveling match
        your instrument. Settings → Instruments → Calibrate.
      </span>
      <button
        type="button"
        aria-label="Dismiss"
        title="Dismiss"
        onClick={() => {
          setDismissed(true);
        }}
        style={{
          cursor: "pointer",
          display: "flex",
          flexShrink: 0,
          background: "transparent",
          border: 0,
          padding: 0,
        }}
      >
        <Icon name="x" size={12} stroke={t.mutedInk} />
      </button>
    </div>
  );
}

/** A footswitch row's mode cell: VERIFY (default) shows a compact "Verify only" chip
 *  + a one-click "Make level-neutral" opt-in that reveals the param picker; LEVEL
 *  shows the picker + a small revert-to-verify affordance. P2's core UX decision:
 *  a row is only ever WRITTEN once the user has explicitly opted in here — never a
 *  silent auto-pick-and-write. */
function FsModeControl({
  mode,
  paramsNode,
  onMakeLevel,
  onRevertVerify,
}: {
  mode: "level" | "verify";
  paramsNode: ReactNode;
  onMakeLevel: () => void;
  onRevertVerify: () => void;
}) {
  const { t } = useTheme();
  return (
    <div
      style={{
        display: "flex",
        alignItems: "center",
        gap: t.space3,
        minWidth: 0,
      }}
    >
      {mode === "verify" ? (
        <>
          <Tag tone="neutral">Verify only</Tag>
          <button
            type="button"
            onClick={onMakeLevel}
            title="Pick a control to solve + write, instead of only measuring the ON/OFF difference"
            style={{
              cursor: "pointer",
              background: "none",
              border: "none",
              padding: 0,
              fontFamily: t.sans,
              fontSize: 10.5,
              color: t.accentDeep,
              textDecoration: "underline",
              textDecorationStyle: "dotted",
              whiteSpace: "nowrap",
            }}
          >
            Make level-neutral
          </button>
        </>
      ) : (
        <>
          <span style={{ flex: 1, minWidth: 0 }}>{paramsNode}</span>
          <button
            type="button"
            title="Verify only — measure the ON/OFF difference, write nothing"
            onClick={onRevertVerify}
            style={{
              cursor: "pointer",
              background: "none",
              border: "none",
              padding: 0,
              flexShrink: 0,
              display: "flex",
            }}
          >
            <Icon name="undo" size={12} stroke={t.mutedInk} />
          </button>
        </>
      )}
    </div>
  );
}

/** One row's editable choices, keyed by `SetupOption.key`. `param`/`fsMode` are
 *  footswitch-row-only fields; `targetMode`/`handle` are scene-row-only — undefined
 *  on a row of the other kind (Base rows carry neither). */
interface RowChoice {
  inst: string;
  target: string;
  param: number | undefined;
  fsMode: "level" | "verify" | undefined;
  targetMode: "match" | "offset" | undefined;
  handle: SceneHandlePick | null | undefined;
}

export interface SetupBodyProps {
  /** The exact scenes picked in the list — all of them WILL be leveled. */
  options: SetupOption[];
  /** How many presets the flow is leveling (for the sub-line). */
  presetCount: number;
  /** True ⇒ re-leveling a clamped subset (title prefix + backup ack hidden). */
  isRelevel: boolean;
  instrumentOptions: PickOption[];
  targetOptions: PickOption[];
  /** Store-backed defaults (never hard-coded ids). */
  defaultInst: string;
  defaultTarget: string;
  onCancel: () => void;
  onStart: (choices: SetupChoice[]) => void;
  /** Opt-in: equalize a path-MERGE preset's two parallel-amp lanes before leveling.
   * A no-op on series / single-amp / split-output presets. */
  onRebalanceChange?: (on: boolean) => void;
  /** Jump to Settings → Instruments (the "calibrate" cue in the instrument nudge). */
  onCalibrate?: () => void;
}

export function SetupBody({
  options,
  presetCount,
  isRelevel,
  instrumentOptions,
  targetOptions,
  defaultInst,
  defaultTarget,
  onCancel,
  onStart,
  onRebalanceChange,
  onCalibrate,
}: SetupBodyProps) {
  const { t } = useTheme();
  const s = useStyles();
  // Inline backup acknowledgment — gates the primary button (mirrors the Copy save
  // bar). Required only on a fresh run; re-level already acknowledged. Default off.
  const requireBackup = !isRelevel;
  const [backedUp, setBackedUp] = useState(false);
  // Advanced, opt-in run option — applies to the whole run; default off. Toggling it
  // both updates the local pill and notifies the flow (read at run time as `rebalance`).
  const [rebalance, setRebalance] = useState(false);
  const toggleRebalance = () => {
    const next = !rebalance;
    setRebalance(next);
    onRebalanceChange?.(next);
  };
  // The flow holds `rebalance` in a ref that survives this body's unmount/remount, but
  // the pill resets to its default each mount — sync the ref to the VISIBLE state on
  // mount so a stale ON from a prior run (re-level / Back→Continue / a new flow) can't
  // silently rebalance against an OFF-looking pill.
  const didSyncRebalance = useRef(false);
  useEffect(() => {
    if (didSyncRebalance.current) return;
    didSyncRebalance.current = true;
    onRebalanceChange?.(rebalance);
  }, [onRebalanceChange, rebalance]);

  const groups = useMemo(() => {
    const by = new Map<
      number,
      { slot: number; name: string; opts: SetupOption[] }
    >();
    options.forEach((o) => {
      let group = by.get(o.slot);
      if (!group) {
        group = { slot: o.slot, name: o.presetName, opts: [] };
        by.set(o.slot, group);
      }
      group.opts.push(o);
    });
    return [...by.values()].sort((a, b) => a.slot - b.slot);
  }, [options]);

  // One per-row choice map — instrument/target (every row) + the footswitch-only and
  // scene-only fields, all seeded in one pass and patched with one setter. Replaces
  // six parallel `Record<string, X>` maps (one seeding loop + setter each): every row
  // used to be assembled by re-keying into six different objects, which made adding a
  // 7th field (or reading "the whole row") six times more code than it needed to be.
  const [rows, setRows] = useState<Partial<Record<string, RowChoice>>>(() => {
    const m: Partial<Record<string, RowChoice>> = {};
    options.forEach((o) => {
      m[o.key] = {
        inst: defaultInst,
        target: defaultTarget,
        // Footswitch rows: the tone-safe default param index; undefined elsewhere.
        param:
          o.footswitch != null && o.levelParams && o.levelParams.length > 0
            ? defaultParamIndex(o.levelParams)
            : undefined,
        // VERIFY is the P2 default; a row only becomes LEVEL once the user clicks
        // "Make level-neutral" (or a re-level round carried LEVEL forward via
        // `runItemToOption`).
        fsMode: o.footswitch != null ? o.footswitch.mode : undefined,
        // Scene rows: target mode ("match" the default, or "offset" — keep the
        // scene's authored loudness relationship) + an optional user-chosen control,
        // both seeded from the option's own carried-forward pick or the defaults.
        targetMode:
          o.sceneSlot != null && !o.isBase
            ? (o.sceneTargetMode ?? "match")
            : undefined,
        handle:
          o.sceneSlot != null && !o.isBase
            ? (o.sceneHandle ?? null)
            : undefined,
      };
    });
    return m;
  });
  const patchRow = (k: string, partial: Partial<RowChoice>) => {
    setRows((p) => {
      const cur = p[k];
      // Every option's key was seeded at mount — an unseeded key is unreachable, but
      // guard rather than assume so the map's real (possibly-undefined) index type
      // holds (this tsconfig has no `noUncheckedIndexedAccess`, so the honest guard
      // has to be written by hand rather than inferred).
      if (!cur) return p;
      return { ...p, [k]: { ...cur, ...partial } };
    });
  };

  // Scene-control candidates: a real device read (`list_scene_level_handles`), so it
  // is fetched LAZILY — once per PRESET, on the first time any of that preset's scene
  // rows opens its picker — never eagerly (Set up otherwise does no device reads).
  const { prefetch: fetchHandlesFor, candidatesFor } = useSceneHandles();

  // Bulk-edit selection (which rows the "Apply to" bar writes to). Empty = all.
  const {
    picked,
    togglePick,
    clearPicked,
    somePicked,
    targetsForBulk,
    scopeLabel,
  } = usePickedRows(options);

  // The "Apply to" bar's current value (also the brush applied on change).
  const [bulkInst, setBulkInst] = useState(defaultInst);
  const [bulkTarget, setBulkTarget] = useState(defaultTarget);
  const applyBulkInst = (v: string) => {
    setBulkInst(v);
    setRows((p) => {
      const n = { ...p };
      targetsForBulk().forEach((k) => {
        const cur = n[k];
        if (cur) n[k] = { ...cur, inst: v };
      });
      return n;
    });
  };
  const applyBulkTarget = (v: string) => {
    setBulkTarget(v);
    setRows((p) => {
      const n = { ...p };
      targetsForBulk().forEach((k) => {
        const cur = n[k];
        if (cur) n[k] = { ...cur, target: v };
      });
      return n;
    });
  };

  const total = options.length;

  const start = () => {
    const choices: SetupChoice[] = options.map((o) => {
      const row = rows[o.key];
      let option = o;
      if (o.footswitch != null) {
        // A row is written (LEVEL) only once the user explicitly opted in AND picked a
        // real candidate; every other case — still VERIFY, or opted in with no valid
        // pick yet (e.g. an unclassifiable-only candidate list) — stays VERIFY. Never
        // silently sweep an unpicked/ambiguous parameter.
        const wantsLevel = row?.fsMode === "level";
        const idx = row?.param;
        const picked =
          wantsLevel &&
          o.levelParams &&
          idx != null &&
          idx >= 0 &&
          idx < o.levelParams.length
            ? o.levelParams[idx]
            : null;
        option = {
          ...o,
          footswitch: picked
            ? targetFromCandidate(o.footswitch.switchIndex, picked)
            : verifyFootswitchTarget(o.footswitch.switchIndex),
          // LABEL PROVENANCE. `sceneName` is not display-only for a footswitch row: it
          // becomes `displayLabel`, which the backend writes as the switch's on-device
          // `customLabel` when an assign appends a function to an UNLABELED switch. The
          // name was chosen back in `chosenFrom`, which only knew the tone-safe DEFAULT
          // candidate — so a user who overrode that pick here would have had the unit
          // renamed after a block the run never touched. Re-derive it from the candidate
          // actually being leveled. A LABELED switch keeps its own name: that string is
          // the player's, and nothing about picking a different knob makes it wrong.
          ...(o.fsUnlabeled === true && picked
            ? { sceneName: footswitchNameForCandidate(picked) }
            : {}),
        };
      } else if (o.sceneSlot != null && !o.isBase) {
        option = {
          ...o,
          sceneTargetMode: row?.targetMode ?? "match",
          sceneHandle: row?.handle ?? null,
        };
      }
      return {
        option,
        instId: row?.inst ?? defaultInst,
        targetName: row?.target ?? defaultTarget,
      };
    });
    if (choices.length) onStart(choices);
  };

  return (
    <>
      <div
        style={{
          flexShrink: 0,
          padding: `${String(t.space8)}px ${String(t.space10)}px ${String(t.space6)}px`,
          borderBottom: `0.5px solid ${t.hairline}`,
        }}
      >
        <WizTitle>
          {isRelevel
            ? "Re-level — set instrument & target"
            : "Set instrument & target"}
        </WizTitle>
        <div
          style={{
            fontFamily: t.mono,
            fontSize: 10.5,
            letterSpacing: "0.04em",
            color: t.mutedInk,
            marginTop: t.space4,
          }}
        >
          {total} sound{total === 1 ? "" : "s"} · {presetCount} preset
          {presetCount === 1 ? "" : "s"}
        </div>
      </div>

      <CalibrationOnboardingBanner
        show={instCalState(bulkInst, instrumentOptions) === "uncal"}
      />

      {/* apply-to bar — writes to all rows, or to the ticked rows */}
      <ApplyToBar
        label={`Apply to ${scopeLabel}`}
        somePicked={somePicked}
        onClear={clearPicked}
      >
        <div
          style={{
            display: "grid",
            gridTemplateColumns: "1fr 1fr",
            gap: t.space8,
          }}
        >
          <Pick
            grow
            value={bulkInst}
            options={instrumentOptions}
            onChange={applyBulkInst}
          />
          <Pick
            grow
            value={bulkTarget}
            options={targetOptions}
            onChange={applyBulkTarget}
          />
        </div>
        <InstrumentNudge
          state={instCalState(bulkInst, instrumentOptions)}
          onCalibrate={onCalibrate}
        />
      </ApplyToBar>

      {/* every sound that will be leveled — set any row directly, or tick for bulk */}
      <div
        style={{
          flex: 1,
          minHeight: 0,
          overflowY: "auto",
          padding: `${String(t.space3)}px 0`,
        }}
      >
        {groups.map((g) => (
          <div
            key={g.slot}
            style={{
              padding: `${String(t.space5)}px ${String(t.space10)}px ${String(t.space6)}px`,
            }}
          >
            <SetupGroupHeader slot={g.slot} name={g.name} />
            {g.opts.map((o) => {
              const row = rows[o.key];
              const tag = o.isBase ? (o.hasScenes ? "BASE" : null) : o.tag;
              const nameLabel = o.isBase ? "Whole preset" : o.sceneName;
              const fsMode =
                o.footswitch != null
                  ? (row?.fsMode ?? o.footswitch.mode)
                  : null;
              const sub = o.isBase
                ? "levels this preset against the others"
                : o.footswitch != null
                  ? fsMode === "level"
                    ? "evens this footswitch out to your target"
                    : "measures this footswitch's ON/OFF loudness difference"
                  : "levels this scene against the preset’s base";
              return (
                <PresetOptionRow
                  key={o.key}
                  setupRowKey={setupRowHookKey(o)}
                  name={nameLabel}
                  tag={tag ?? undefined}
                  isBase={o.isBase}
                  sub={sub}
                  isPicked={picked.has(o.key)}
                  onTogglePick={() => {
                    togglePick(o.key);
                  }}
                  title="Tick to bulk-edit this row with the bar above"
                  columns="132px 108px 108px"
                >
                  {/* Footswitch rows: verify-only by default, with a "Make level-
                      neutral" opt-in that reveals the param picker. Scene rows: the
                      target-mode + control picker. Base rows keep the column empty
                      so the pickers stay aligned. */}
                  {o.footswitch != null &&
                  o.levelParams &&
                  o.levelParams.length > 0 ? (
                    <FsModeControl
                      mode={fsMode ?? "verify"}
                      paramsNode={
                        <FsParamPick
                          params={o.levelParams}
                          index={row?.param ?? -1}
                          onChange={(i) => {
                            patchRow(o.key, { param: i });
                          }}
                        />
                      }
                      onMakeLevel={() => {
                        patchRow(o.key, { fsMode: "level" });
                      }}
                      onRevertVerify={() => {
                        patchRow(o.key, { fsMode: "verify" });
                      }}
                    />
                  ) : o.sceneSlot != null && !o.isBase ? (
                    <SceneLevelPick
                      targetMode={row?.targetMode ?? "match"}
                      onTargetModeChange={(m) => {
                        patchRow(o.key, { targetMode: m });
                      }}
                      handle={row?.handle ?? null}
                      onHandleChange={(h) => {
                        patchRow(o.key, { handle: h });
                      }}
                      candidates={candidatesFor(o.slot, o.sceneSlot)}
                      onOpen={() => {
                        fetchHandlesFor(o.slot);
                      }}
                    />
                  ) : (
                    <div />
                  )}
                  <Pick
                    grow
                    value={row?.inst ?? defaultInst}
                    options={instrumentOptions}
                    onChange={(v) => {
                      patchRow(o.key, { inst: v });
                    }}
                  />
                  <Pick
                    grow
                    tid={`target:${g.name}`}
                    value={row?.target ?? defaultTarget}
                    options={targetOptions}
                    onChange={(v) => {
                      patchRow(o.key, { target: v });
                    }}
                  />
                </PresetOptionRow>
              );
            })}
          </div>
        ))}
      </div>

      {/* run option — advanced, opt-in, applies to the whole run. Mirrors the apply-to
          bar at the top (same tint + hairline) so the two config zones bookend the list.
          ALWAYS visible: the engine no-ops on non-merged sounds, and setup does no device
          reads (topology is only known once each preset loads at run time). */}
      <div
        style={{
          flexShrink: 0,
          padding: `${String(t.space6)}px ${String(t.space10)}px ${String(t.space6)}px`,
          background: t.bgAlt,
          borderTop: `0.5px solid ${t.hairline}`,
        }}
      >
        <div style={{ ...s.kickerWide(t.faint), marginBottom: t.space4 }}>
          Run option
        </div>
        <button
          type="button"
          role="switch"
          aria-checked={rebalance}
          onClick={toggleRebalance}
          style={{
            display: "flex",
            alignItems: "flex-start",
            gap: t.space6,
            cursor: "pointer",
            userSelect: "none",
            width: "100%",
            textAlign: "left",
            background: "none",
            border: "none",
            padding: 0,
            font: "inherit",
            color: "inherit",
          }}
        >
          <span aria-hidden style={{ paddingTop: t.space1, flexShrink: 0 }}>
            <Toggle on={rebalance} />
          </span>
          <div style={{ minWidth: 0 }}>
            <div
              style={{
                fontFamily: t.sans,
                fontSize: 13,
                fontWeight: 500,
                color: t.ink,
              }}
            >
              Even out parallel amps
            </div>
            <div
              style={{
                fontFamily: t.sans,
                fontSize: 11,
                lineHeight: 1.5,
                color: t.mutedInk,
                marginTop: t.space1,
                textWrap: "pretty",
              }}
            >
              When a sound blends two amps into one, match their levels before
              leveling. No effect on single-amp sounds.
            </div>
            {rebalance && (
              <div
                style={{
                  display: "flex",
                  alignItems: "center",
                  gap: t.space4,
                  marginTop: t.space4,
                }}
              >
                <ByEarChip />
                <span
                  style={{
                    fontFamily: t.sans,
                    fontSize: 11,
                    color: t.mutedInk,
                  }}
                >
                  Rebalanced sounds come back flagged for a listen.
                </span>
              </div>
            )}
          </div>
        </button>
      </div>

      <WizardFooter
        left={
          <Button
            variant="ghost"
            small
            onClick={onCancel}
            style={{ height: 32, padding: `0 ${String(t.space8)}px` }}
          >
            Cancel
          </Button>
        }
        right={
          <>
            {requireBackup && (
              <BackupAckLabel
                checked={backedUp}
                onChange={setBackedUp}
                style={{ userSelect: "none", paddingRight: t.space2 }}
              />
            )}
            <Button
              variant="primary"
              small
              icon="gauge"
              disabled={total === 0 || (requireBackup && !backedUp)}
              onClick={start}
              style={{ height: 32, padding: `0 ${String(t.space8)}px` }}
            >
              {`Level ${String(total)} sound${total === 1 ? "" : "s"}`}
            </Button>
          </>
        }
      />
    </>
  );
}

export default SetupBody;
