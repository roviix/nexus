import { describe, expect, it } from "vitest";
import { installedMode, MODE_LABEL, MODES } from "./CursorPanelPage";
import type { CrsrStatus, SandStatus } from "../ipc/types";

const crsr = (over: Partial<CrsrStatus> = {}): CrsrStatus =>
  ({ installed: false, complete: false, ...over }) as CrsrStatus;
const sand = (over: Partial<SandStatus> = {}): SandStatus =>
  ({ installed: false, complete: false, ...over }) as SandStatus;

describe("installedMode", () => {
  it("is unknown until both statuses are in — guessing 原生 on a patched machine is a lie", () => {
    expect(installedMode(null, sand())).toBeNull();
    expect(installedMode(crsr(), null)).toBeNull();
  });

  it("names whichever patch is on disk, and 原生 when neither is", () => {
    expect(installedMode(crsr(), sand())).toBe("native");
    expect(installedMode(crsr({ installed: true }), sand())).toBe("crsr");
    expect(installedMode(crsr(), sand({ installed: true }))).toBe("sand");
    // 两条都在是 Rust 侧拒绝的形态；真出现了先报后装的那条。
    expect(installedMode(crsr({ installed: true }), sand({ installed: true }))).toBe("crsr");
  });
});

describe("MODES", () => {
  it("offers exactly the three answers, each with a label and a cost written out", () => {
    expect(MODES.map((m) => m.id)).toEqual(["native", "crsr", "sand"]);
    for (const m of MODES) {
      expect(MODE_LABEL[m.id]).toBe(m.label);
      expect(m.desc.length).toBeGreaterThan(10);
    }
  });
});
