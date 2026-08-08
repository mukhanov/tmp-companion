// Mirrors src-tauri/src/param_class.rs's test table 1:1 so a JSON drift breaks
// BOTH suites instead of silently diverging between the Rust and TS sides.

import { describe, it, expect } from "vitest";

import { classify } from "../paramClass";

describe("classify", () => {
  it("classifies a param-name default as level_linear", () => {
    const info = classify("ACD_SomeBlock", "outputLevel");
    expect(info.class).toBe("level_linear");
    expect(info.range).toEqual([0, 1]);
  });

  it("classifies a param-name default as level_db", () => {
    const info = classify("ACD_SomeBlock", "makeupgaindb");
    expect(info.class).toBe("level_db");
    expect(info.range).toEqual([0, 24]);
  });

  it("classifies a param-name default as wet_mix", () => {
    const info = classify("ACD_SomeBlock", "mix");
    expect(info.class).toBe("wet_mix");
    expect(info.range).toEqual([0, 1]);
  });

  it("classifies an unknown param as other", () => {
    const info = classify("ACD_SomeBlock", "totallyUnknownParam");
    expect(info.class).toBe("other");
  });

  it("overrides win over the default — the ACD_TMRumbleV3.level trap", () => {
    // `level` is a level_linear default everywhere EXCEPT ACD_TMRumbleV3,
    // where it's an amp knob that must never be swept for loudness.
    const default_ = classify("ACD_SomeOtherBlock", "level");
    expect(default_.class).toBe("level_linear");

    const trapped = classify("ACD_TMRumbleV3", "level");
    expect(trapped.class).toBe("other");
  });

  it("a block override only shadows its own overridden param", () => {
    // ACD_Boost overrides `gain`, but its other params still fall through to
    // the param-name defaults.
    const level = classify("ACD_Boost", "level");
    expect(level.class).toBe("level_linear");
  });

  it("ACD_Boost.gain is a block override classified as raw dB", () => {
    const info = classify("ACD_Boost", "gain");
    expect(info.class).toBe("level_db");
    expect(info.range).toEqual([0, 12]);
  });

  it("an override matches after device-suffix collapse", () => {
    // A hypothetical device id carrying a merged cab/IR suffix must still
    // collapse to the base "ACD_TMRumbleV3" override.
    const info = classify("ACD_TMRumbleV3CabIR", "level");
    expect(info.class).toBe("other");
  });

  it("amp volume is the breakup knob, not a level control", () => {
    // ACD_MarshallPlexi (Half Stacks / Amp Heads): `volume` is the
    // preamp/breakup knob, not an output level — sweeping it changes tone.
    const info = classify("ACD_MarshallPlexi", "volume");
    expect(info.class).toBe("other");
  });

  it("pedal volume is a genuine level control", () => {
    // ACD_KingOfTone (Effects): `volume` is a real output level, unlike the
    // same param name on an amp model.
    const info = classify("ACD_KingOfTone", "volume");
    expect(info.class).toBe("level_linear");
  });

  it("a block override beats an amp override", () => {
    // ACD_TMRumbleV3 is a Bass Amp AND carries its own `level` override — the
    // exact-block override must still win, unaffected by ampOverrides (which
    // doesn't even define "level").
    const info = classify("ACD_TMRumbleV3", "level");
    expect(info.class).toBe("other");
  });

  it("an amp override matches after device-suffix collapse", () => {
    // A suffixed amp device id (CabIR-style) must still reach the amp check
    // and bar `volume`.
    const info = classify("ACD_MarshallPlexiCabIR", "volume");
    expect(info.class).toBe("other");
  });

  it("one real id per AMP_CATEGORIES entry bars volume, guarding a category-list drift", () => {
    // The existing amp-ness tests above only ever exercise ACD_MarshallPlexi
    // (Half Stacks + Amp Heads) and ACD_KingOfTone (Effects, non-amp) — a
    // category silently dropped from this file's AMP_CATEGORIES (or from the
    // Rust source of truth, `scene_jobs::is_amp_category`) would pass both
    // suites regardless. One real catalogued id per named category (verified
    // against src/models/tmp-model-guide.json) narrows that gap.
    //
    // HONEST LIMIT: on this catalog every "Combo Amps" id and every "Half
    // Stacks" id ALSO carries "Amp Heads" (same underlying DSP block, sold in
    // combo/half-stack/head physical forms alike) — there is no combo-only or
    // half-stack-only id to pick instead. So this table only actually catches
    // a dropped "Amp Heads" (via ACD_StudioPreamp, Amp-Heads-only on this
    // catalog) or a dropped "Bass Amps" (via ACD_TMRumbleV3, Bass-Amps-only);
    // silently dropping "Combo Amps" or "Half Stacks" alone would still pass
    // this table, because ACD_Princeton6G2 and ACD_MarshallPlexi stay
    // amp-classified via their shared "Amp Heads" membership. Kept in the
    // table anyway for readable category coverage and because a future
    // catalog revision could add an exclusive id for either.
    //
    // The mirrored Rust test table (src-tauri/src/param_class.rs's test mod,
    // added alongside this one) uses this exact id list — keep the two in
    // sync.
    const amps: readonly [id: string, category: string][] = [
      ["ACD_Princeton6G2", "Combo Amps"],
      ["ACD_StudioPreamp", "Amp Heads"], // Amp-Heads-only on this catalog
      ["ACD_TMRumbleV3", "Bass Amps"], // Bass-Amps-only on this catalog
      ["ACD_MarshallPlexi", "Half Stacks"],
    ];
    for (const [id, category] of amps) {
      const info = classify(id, "volume");
      expect(info.class, `${id} (${category}) volume must be "other"`).toBe(
        "other",
      );
    }
  });

  it("a non-amp id's volume stays a genuine level control", () => {
    // Paired with the table above: the drift this guards against could also
    // run the other way (a non-amp category wrongly added to AMP_CATEGORIES).
    const info = classify("ACD_KingOfTone", "volume");
    expect(info.class).toBe("level_linear");
  });
});
