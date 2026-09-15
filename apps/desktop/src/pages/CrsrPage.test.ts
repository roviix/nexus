import { describe, expect, it } from "vitest";
import { STEP_LABEL } from "./CrsrPage";

describe("CRSR page copy", () => {
  it("labels every install step", () => {
    expect(STEP_LABEL.preflight).toContain("预检");
    expect(STEP_LABEL.quit_cursor).toContain("退出");
    expect(STEP_LABEL.write).toContain("写入");
    expect(STEP_LABEL.done).toBe("完成");
  });
});
