/**
 * Sand 页两张手写表的护栏。类型系统只能保证 `MARKER_ROWS` 的 key 合法、`STEP_LABEL` 的键齐全，
 * 保证不了「不漏、不重、期望值与 Rust `RuleId::expected()` 一致」。这里把后者钉死
 * （docs/SAND.md §8：marker 与期望计数各处必须一致）。
 */
import { describe, expect, it } from "vitest";
import { cursorDownloadLabel, MARKER_ROWS, passthroughUrlOf, STEP_LABEL } from "./SandPage";
import type {
  CursorDownload,
  CursorRelease,
  GatewayStatus,
  MarkerCounts,
  SandStep,
} from "../ipc/types";

/** 显式列全 19 个字段：`MarkerCounts` 增删字段时这里编译不过，测试就跟着更新。 */
const SAMPLE_COUNTS: MarkerCounts = {
  clientType: 0,
  eligibility: 0,
  managedLocalRoute: 0,
  localRuntimeLoad: 0,
  inferenceStream: 0,
  agentHostEnablement: 0,
  agentHostIdentity: 0,
  agentHostMoveExec: 0,
  managedSubagentRoute: 0,
  managedSubagentSession: 0,
  managedTaskTool: 0,
  managedActionRoute: 0,
  subagentResumeMode: 0,
  subagentCompletionWake: 0,
  subagentInteractionBubble: 0,
  subagentModelVariants: 0,
  contextWindow: 0,
  inferenceEndpoint: 0,
  grokbotStreamAuth: 0,
};
const FIELDS = Object.keys(SAMPLE_COUNTS) as Array<keyof MarkerCounts>;

/**
 * 与 Rust `RuleId::expected()` 逐项对应：client-type 23（isGlass 16 + 对象头 3 + header.set 4），
 * agent host enable / completion wake / subagent model variants 每文件一处共 2，eligibility 与
 * inference endpoint（可选项：本机开「推理经本机网关」或远程默认带）以及 3.19.x 官方已做的
 * subagent route / session 不校验，其余都是 1。
 */
const EXPECTED_NEED: Record<keyof MarkerCounts, number | null> = {
  clientType: 23,
  eligibility: null,
  managedLocalRoute: 1,
  localRuntimeLoad: 1,
  inferenceStream: 1,
  agentHostEnablement: 2,
  agentHostIdentity: 1,
  agentHostMoveExec: 1,
  managedSubagentRoute: null,
  managedSubagentSession: 1,
  managedTaskTool: 1,
  managedActionRoute: 1,
  subagentResumeMode: 1,
  subagentCompletionWake: 2,
  subagentInteractionBubble: 1,
  subagentModelVariants: 2,
  contextWindow: 1,
  inferenceEndpoint: null,
  grokbotStreamAuth: null,
};

/** 同样的招：`SandStep` 增删值时这里编译不过。 */
const ALL_STEPS: Record<SandStep, true> = {
  preflight: true,
  backup: true,
  quit_cursor: true,
  write: true,
  verify: true,
  launch: true,
  done: true,
};
const STEPS = Object.keys(ALL_STEPS) as SandStep[];

describe("MARKER_ROWS", () => {
  it("covers every MarkerCounts field exactly once", () => {
    expect(FIELDS).toHaveLength(19);
    const keys = MARKER_ROWS.map((r) => r.key);
    expect(new Set(keys).size).toBe(keys.length);
    expect([...keys].sort()).toEqual([...FIELDS].sort());
  });

  it("expects the same counts as Rust RuleId::expected()", () => {
    const need = Object.fromEntries(MARKER_ROWS.map((r) => [r.key, r.need]));
    expect(need).toEqual(EXPECTED_NEED);
  });

  it("explains every anchor in words", () => {
    // 只摆 `managedSubagentRoute` 这种标识，等于什么都没告诉「装之前想看看会改什么」的人。
    for (const row of MARKER_ROWS) {
      expect(row.desc.trim(), `MARKER_ROWS.${row.key}.desc`).not.toBe("");
      expect(row.desc, `MARKER_ROWS.${row.key}.desc`).not.toBe(row.label);
    }
  });
});

describe("STEP_LABEL", () => {
  it("has a non-empty label for all 7 SandStep values", () => {
    expect(STEPS).toHaveLength(7);
    expect(Object.keys(STEP_LABEL).sort()).toEqual([...STEPS].sort());
    for (const step of STEPS) {
      expect(STEP_LABEL[step].trim(), `STEP_LABEL.${step}`).not.toBe("");
    }
  });
});

/**
 * 写进补丁的端点必须和网关真正监听的地址一致，否则 Agent 面板一发推理就连不上：
 * 在跑用绑定到的真实地址（端口被占会自动挪），没在跑按设置里的端口算。
 */
describe("passthroughUrlOf", () => {
  const base: GatewayStatus = {
    running: null,
    settings: { port: 8787, passthroughPort: 8788, clientType: "sand", autostart: false, forceModel: null, defaultChannel: "cursor" },
    restartNeeded: false,
    apiKeySet: true,
    channels: [],
    mediaJobs: [],
    entrances: [],
    intercept: { rule: { enabled: false, position: "tail", marker: "[nexus-mark]" }, calls: 0, rewritten: 0, errors: 0, recent: [] },
  grokbotStream: { enabled: false, credential: null },
    lane: { current: null, candidates: [], missing: [], available: [] },
  };

  it("is null when the gateway status is unavailable", () => {
    expect(passthroughUrlOf(null)).toBeNull();
  });

  it("falls back to the configured passthrough port when the gateway is stopped", () => {
    expect(passthroughUrlOf(base)).toBe("http://127.0.0.1:8788");
  });

  it("prefers the actually bound address when the gateway is running", () => {
    const running: GatewayStatus = {
      ...base,
      running: {
        addr: "127.0.0.1:8790",
        baseUrl: "http://127.0.0.1:8790",
        passthroughAddr: "127.0.0.1:8791",
        passthroughBaseUrl: "http://127.0.0.1:8791",
        startedAt: "2026-09-05T00:00:00Z",
      },
    };
    expect(passthroughUrlOf(running)).toBe("http://127.0.0.1:8791");
  });
});

describe("cursorDownloadLabel", () => {
  const universalDownload: CursorDownload = {
    architecture: "universal",
    url: "https://downloads.cursor.com/cursor.dmg",
  };

  it("keeps the single macOS universal download compact", () => {
    const release: CursorRelease = {
      version: "3.18.25",
      platform: "macos",
      downloads: [universalDownload],
    };
    expect(cursorDownloadLabel(release, universalDownload)).toBe(
      "下载 Cursor 3.18.25",
    );
  });

  it("distinguishes x64 and ARM64 when a platform has two packages", () => {
    const x64Download: CursorDownload = {
      architecture: "x64",
      url: "https://downloads.cursor.com/cursor-x64.exe",
    };
    const arm64Download: CursorDownload = {
      architecture: "arm64",
      url: "https://downloads.cursor.com/cursor-arm64.exe",
    };
    const release: CursorRelease = {
      version: "3.18.25",
      platform: "windows",
      downloads: [x64Download, arm64Download],
    };

    expect(cursorDownloadLabel(release, x64Download)).toBe(
      "下载 Cursor 3.18.25 · x64",
    );
    expect(cursorDownloadLabel(release, arm64Download)).toBe(
      "下载 Cursor 3.18.25 · ARM64",
    );
  });
});
