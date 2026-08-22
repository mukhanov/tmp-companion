// src/views/doctor/MeasuredChange.tsx — the MEASURED per-band change between an
// apply's before/after captures (`DoctorApplyResult.measured`), shown under the
// A/B player: "Measured · Low-mids −2.8 · Highs +1.1 · +0.4 dB louder". The
// numbers are the device's own answer to a prescription — the balance plan's
// prediction is checked against these, never the other way round.

import { useTheme } from "../../theme/ThemeContext";
import { signedDb } from "../../lib/format";
import { measuredBandLine, MEASURED_MIN_DB } from "./planModel";
import type { DoctorApplyMeasure } from "../../lib/types";

export interface MeasuredChangeProps {
  measured: DoctorApplyMeasure;
}

export function MeasuredChange({ measured }: MeasuredChangeProps) {
  const { t } = useTheme();
  const loud = measured.loudnessDeltaDb;
  const loudLine =
    Math.abs(loud) >= MEASURED_MIN_DB
      ? ` · ${signedDb(loud)} dB ${loud > 0 ? "louder" : "quieter"}`
      : "";
  return (
    <div
      data-testid="measured-change"
      style={{
        marginTop: t.space4,
        fontFamily: t.mono,
        fontSize: t.fsData2,
        color: t.mutedInk,
        lineHeight: 1.6,
      }}
    >
      <span
        style={{
          letterSpacing: t.lsTag,
          textTransform: "uppercase",
          color: t.ink2,
        }}
      >
        Measured
      </span>{" "}
      {measuredBandLine(measured)}
      {loudLine}
    </div>
  );
}

export default MeasuredChange;
