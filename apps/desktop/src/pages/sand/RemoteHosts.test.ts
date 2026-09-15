/**
 * 远程卡片的护栏：`SERVER_MARKER_NEED` 必须与 Rust `RuleId::expected_for(LayoutProfile::Server)`
 * 一致——那是拿原版远程 bundle 量出来的数字（client-type 2；enable / wake / variants 0，因为它们的
 * 锚点在远程不存在的 workbench 文件里）。两边漂移的后果是界面上「已打补丁」和后端的
 * `complete` 判断不一致。
 */
import { describe, expect, it } from "vitest";
import {
  DEFAULT_REMOTE_PORT,
  legacyEndpoint,
  pinnedTarget,
  probeVerdict,
  ROUTE_LABEL,
  ROUTES,
  SERVER_MARKER_NEED,
  serverTally,
  tunnelBlocker,
  tunnelLabel,
  unwrapStatus,
} from "./RemoteHosts";
import type { MarkerCounts, ProbeReport, RemoteHostView, RemoteStatus, TunnelStatus } from "../../ipc/types";

const ZERO: MarkerCounts = {
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

const tunnel = (over: Partial<TunnelStatus> = {}): TunnelStatus => ({
  spec: { host: "box", remotePort: 41777, localPort: 8788 },
  phase: "stopped",
  reconnects: 0,
  lastError: null,
  streams: 0,
  ...over,
});

const view = (over: Partial<RemoteHostView["host"]>, extra: Partial<RemoteHostView> = {}): RemoteHostView =>
  ({
    host: {
      host: "box",
      label: "",
      route: "proxy",
      remotePort: 41777,
      proxyPort: null,
      ...over,
    },
    status: { Ok: {} as RemoteStatus },
    tunnel: tunnel(),
    localPort: 7890,
    proxyConfigured: null,
    localListening: true,
    ...extra,
  }) as RemoteHostView;

describe("SERVER_MARKER_NEED", () => {
  it("matches the measured Server profile (client-type 2, three workbench-only rules absent)", () => {
    expect(SERVER_MARKER_NEED.clientType).toBe(2);
    // 这三类在远程根本没有锚点所在的文件；表里不出现 = 期望 0、不参与计数。
    expect(SERVER_MARKER_NEED).not.toHaveProperty("agentHostEnablement");
    expect(SERVER_MARKER_NEED).not.toHaveProperty("subagentCompletionWake");
    expect(SERVER_MARKER_NEED).not.toHaveProperty("subagentModelVariants");
    // 可选项不校验。
    expect(SERVER_MARKER_NEED).not.toHaveProperty("eligibility");
    expect(SERVER_MARKER_NEED).not.toHaveProperty("inferenceEndpoint");
    // 3.19.7 起 subagent route 官方已做，不进硬校验。
    expect(SERVER_MARKER_NEED).not.toHaveProperty("managedSubagentRoute");
    // 但 session 那条改挂了 enableBrowserSubagent（browser-use 子代理靠它），远程也要打：
    // 它落在 4883.js 上，那个文件远程有。
    expect(SERVER_MARKER_NEED.managedSubagentSession).toBe(1);
    // 其余都是 1，共 11 类 + client-type 2 = 13 处。
    const total = Object.values(SERVER_MARKER_NEED).reduce((a, b) => a + (b ?? 0), 0);
    expect(total).toBe(13);
  });

  it("tallies have/need without letting overshoot count", () => {
    expect(serverTally(ZERO)).toEqual({ need: 13, have: 0 });
    const full: MarkerCounts = { ...ZERO, clientType: 5, inferenceStream: 3, managedLocalRoute: 1 };
    // clientType 多出来的不算，inferenceStream 多出来的也不算
    expect(serverTally(full)).toEqual({ need: 13, have: 2 + 1 + 1 });
  });
});

describe("unwrapStatus", () => {
  it("unwraps Rust Result<RemoteStatus, String> from serde's externally tagged JSON", () => {
    const ok = { Ok: { host: "box" } as unknown as RemoteStatus };
    expect(unwrapStatus(ok).ok?.host).toBe("box");
    expect(unwrapStatus({ Err: "boom" }).err).toBe("boom");
  });
});

/**
 * 回归（2026-09-04 真机）：应用 17:50 换包重启后隧道没了，远程 bundle 里的端点还指着
 * `127.0.0.1:8688`，于是远程 Agent Host 连着 12 次 `ECONNREFUSED`，而这张卡片上「已打补丁」
 * 和「网关在跑」都是绿的，隧道那格只是一个中性的「隧道未开」。这条链路断了就必须说出来，
 * 而且要说清楚断的后果是什么。
 */
describe("tunnelBlocker", () => {
  it("stays quiet when the tunnel is up, or when nothing on the remote points at us", () => {
    expect(tunnelBlocker("http://127.0.0.1:41777", tunnel({ phase: "connected" }))).toBeNull();
    // Cursor 设置里没指着我们 = 远程直连 api2，隧道开不开都不影响它。
    expect(tunnelBlocker(null, tunnel())).toBeNull();
  });

  it("names the port and the symptom, and offers to start only when it is actually stopped", () => {
    const off = tunnelBlocker("http://127.0.0.1:41777", tunnel());
    expect(off?.canStart).toBe(true);
    expect(off?.title).toContain("http://127.0.0.1:41777");
    expect(off?.title).toContain("ECONNREFUSED");

    // 正在连 / 正在重连时按钮没有意义（start 是幂等的），但仍然要报「现在是断的」。
    expect(tunnelBlocker("http://127.0.0.1:41777", tunnel({ phase: "connecting" }))?.canStart).toBe(false);
    const retrying = tunnelBlocker("http://127.0.0.1:41777", tunnel({ phase: "reconnecting", reconnects: 3 }));
    expect(retrying?.canStart).toBe(false);
    expect(retrying?.title).toContain("第 3 次");
  });
});

/**
 * 「远程那侧已经指着我们」这个证据只有一处：写进 Cursor 设置的 HTTP_PROXY。看错地方的后果是
 * **代理模式永远不报警**——那正是 §9.5 那次静默失效的形状。
 */
describe("pinnedTarget", () => {
  it("reads the proxy setting for the proxy route and nothing for direct", () => {
    expect(pinnedTarget(view({ route: "proxy" }, { proxyConfigured: "http://127.0.0.1:41777" }))).toBe(
      "http://127.0.0.1:41777",
    );
    expect(pinnedTarget(view({ route: "proxy" }))).toBeNull();
    expect(pinnedTarget(view({ route: "direct" }))).toBeNull();
  });
});

/**
 * 早期版本「经本机网关」把推理端点改道写进了远程 bundle。那条路随透传口一起拆掉，端点指着一个
 * 永远没人听的口——远程每次推理都是 ECONNREFUSED。不论现在选哪条出网方式，盘上有它就要点名报出来，
 * 并且给「重新安装」（任何选项的重装都会把它剥掉）。
 */
describe("legacyEndpoint", () => {
  it("flags any endpoint rewrite still on the remote disk", () => {
    const msg = legacyEndpoint({ inferenceEndpoint: "http://127.0.0.1:7897" } as RemoteStatus);
    expect(msg).toContain("http://127.0.0.1:7897");
    expect(msg).toContain("重新安装");
  });

  it("is silent when the bundle is clean or unknown", () => {
    expect(legacyEndpoint({ inferenceEndpoint: null } as RemoteStatus)).toBeNull();
    expect(legacyEndpoint(undefined)).toBeNull();
  });
});

describe("probeVerdict", () => {
  const report = (over: Partial<ProbeReport>): ProbeReport => ({
    ok: false,
    stage: "tunnel",
    status: null,
    detail: null,
    remotePort: 41777,
    ...over,
  });

  it("calls any HTTP status a success — the probe checks the path, not the auth", () => {
    const v = probeVerdict(report({ ok: true, stage: "http", status: 404 }));
    expect(v.tone).toBe("ok");
    expect(v.title).toContain("404");
    expect(v.title).toContain("本机代理");
  });

  it("blames a different hop for each stage and says what to do next", () => {
    const tunnelHop = probeVerdict(report({ stage: "tunnel", detail: "ECONNREFUSED" }));
    expect(tunnelHop.tone).toBe("bad");
    expect(tunnelHop.title).toContain("41777");
    expect(tunnelHop.title).toContain("ECONNREFUSED");
    expect(tunnelHop.hint).toContain("代理");
    // 探针打的是常驻隧道的口，所以「断在第一跳」首先要去看隧道状态。
    expect(tunnelHop.hint).toContain("隧道");

    // 断在代理 = 端口不是 HTTP 代理，最常见是把 SOCKS 口填进来了。
    expect(probeVerdict(report({ stage: "proxy" })).hint).toContain("SOCKS");
    // 断在 TLS = 多半有中间人。
    expect(probeVerdict(report({ stage: "tls" })).hint).toContain("中间人");
  });
});

describe("tunnelLabel", () => {
  it("shows live connection count when connected and the far end is up", () => {
    expect(tunnelLabel(tunnel({ phase: "connected", streams: 3 }), true)).toEqual({ tone: "ok", text: "隧道已连接 · 3 条连接" });
    expect(tunnelLabel(tunnel({ phase: "connected" }), true).text).toBe("隧道已连接");
  });

  it("warns when the tunnel is up but nobody is listening on this side", () => {
    const l = tunnelLabel(tunnel({ phase: "connected" }), false);
    expect(l.tone).toBe("warn");
    expect(l.text).toContain("本机那头");
  });

  it("marks reconnecting as bad and stopped as neutral", () => {
    expect(tunnelLabel(tunnel({ phase: "reconnecting", reconnects: 2 }), true)).toEqual({ tone: "bad", text: "隧道重连中（第 2 次）" });
    expect(tunnelLabel(tunnel({ phase: "stopped" }), true).tone).toBe("default");
  });
});

describe("ROUTES", () => {
  it("covers exactly the two routes and every one has a label", () => {
    expect(ROUTES.map((r) => r.id)).toEqual(["proxy", "direct"]);
    for (const r of ROUTES) {
      expect(ROUTE_LABEL[r.id]).toBe(r.label);
      // 每条路都要把**代价**写出来，光有名字等于没得选。
      expect(r.desc.length).toBeGreaterThan(20);
    }
  });

  it("defaults new hosts to a high remote port that avoids common proxy ports", () => {
    expect(DEFAULT_REMOTE_PORT).toBeGreaterThan(10000);
    expect([7890, 7897, 8080, 1080, 8788]).not.toContain(DEFAULT_REMOTE_PORT);
  });
});
