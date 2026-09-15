/**
 * Sand 页 —— 给本机 Cursor 打补丁，让 Agent 面板走 sand/bot 额度通道。
 *
 * 这一页的第一原则是**如实**：补丁追着 Cursor 版本跑（docs/SAND.md），所以
 *  - 版本不匹配时明说「等待适配」，不给按钮让人去试；
 *  - 装之前先给 dry-run：会改几个文件、哪些锚点齐不齐；
 *  - 失败措辞分「写盘前 / 写盘后」两种（`outcome.wrote`），用户会照着它决定要不要再来一次。
 */
import { openUrl } from "@tauri-apps/plugin-opener";
import { useCallback, useEffect, useState } from "react";
import { gateway, onSandProgress, sand } from "../ipc/api";
import type {
  CursorDownload,
  CursorRelease,
  GatewayStatus,
  GrokBotAuthMode,
  MarkerCounts,
  ModeGate,
  SandBackup,
  SandOutcome,
  SandProgress,
  SandStatus,
} from "../ipc/types";
import { go, type Route } from "../shell/nav";
import { Banner, Empty, ErrorNote, Icon, Modal, Opt, Spinner, Switch, Tag } from "../ui/primitives";
import { timeAgo } from "../ui/format";
import { InterceptCard } from "./sand/InterceptCard";
import { RemoteHosts } from "./sand/RemoteHosts";

const GROK45_CUA_KEY = "nexus.sand.grok45ViaCua";

function readGrok45ViaCua(): boolean | null {
  try {
    const v = window.localStorage.getItem(GROK45_CUA_KEY);
    if (v === "1") return true;
    if (v === "0") return false;
  } catch {
    /* 读不到就当没选过 */
  }
  return null;
}

export const STEP_LABEL: Record<SandProgress["step"], string> = {
  preflight: "预检版本与锚点",
  backup: "备份改动前的文件",
  quit_cursor: "退出 Cursor",
  write: "写入补丁",
  verify: "校验完整性",
  launch: "启动 Cursor",
  done: "完成",
};

/**
 * marker 行：显示名 + 一句人话 + 期望值。与 Rust `RuleId::name()/expected()` 对齐；
 * eligibility 不校验。
 *
 * `desc` 存在的理由：`managedSubagentRoute` 这种名字对着代码才读得懂，而这一页要给
 * 「装之前先看看会改什么」的人看 —— 只摆英文标识等于什么都没说。
 */
export const MARKER_ROWS: Array<{
  key: keyof MarkerCounts;
  label: string;
  desc: string;
  need: number | null;
}> = [
  { key: "clientType", label: "client-type", desc: "客户端类型标识", need: 23 },
  { key: "managedLocalRoute", label: "managed-local route", desc: "托管本地路由", need: 1 },
  { key: "localRuntimeLoad", label: "local runtime load", desc: "本地运行时加载", need: 1 },
  { key: "inferenceStream", label: "inference stream", desc: "推理引擎（直连）", need: 1 },
  { key: "agentHostEnablement", label: "agent host enable", desc: "Agent 宿主启用", need: 2 },
  { key: "agentHostIdentity", label: "agent host identity", desc: "Agent 宿主身份", need: 1 },
  { key: "agentHostMoveExec", label: "move_exec", desc: "执行文件迁移", need: 1 },
  { key: "managedSubagentRoute", label: "subagent route", desc: "子代理路由（3.19.x 官方已做，不强制）", need: null },
  { key: "managedSubagentSession", label: "subagent session", desc: "开 browser-use 子代理", need: 1 },
  { key: "managedTaskTool", label: "task tool", desc: "任务工具注册", need: 1 },
  { key: "managedActionRoute", label: "action route", desc: "动作路由", need: 1 },
  { key: "subagentResumeMode", label: "subagent resume mode", desc: "子代理续接模式", need: 1 },
  { key: "subagentCompletionWake", label: "completion wake", desc: "补全唤醒机制", need: 2 },
  { key: "subagentInteractionBubble", label: "subagent web bubble", desc: "子代理网页气泡", need: 1 },
  { key: "subagentModelVariants", label: "subagent model variants", desc: "子代理模型档位", need: 2 },
  { key: "contextWindow", label: "context window", desc: "上下文窗口", need: 1 },
  { key: "eligibility", label: "eligibility", desc: "资格校验（不强制）", need: null },
  // 可选项：把推理改道到本机网关的透传口（本机开「推理经本机网关」、或远程主机默认带）。
  // 装了是 2 处，没装是 0，两种都合法，所以不进硬校验；装没装另有专门的校验。
  { key: "inferenceEndpoint", label: "inference endpoint", desc: "推理经本机网关（可选）", need: null },
  // 三种形态（关 / Box Relay / 开）；装了是 2 处，没装是 0，不进硬校验。
  { key: "grokbotStreamAuth", label: "grokbot auth", desc: "Bot 通道（可选）", need: null },
];

/** 界面上只说开 / 关；具体走法不写在选项里。 */
export const GROKBOT_MODE_LABEL: Record<GrokBotAuthMode, string> = {
  off: "关",
  box_relay: "Box Relay",
  direct: "开",
};

/** Box Relay 不主动露出；盘上已经装着的机器要能看见并切走，所以按盘上状态决定是否列出来。 */
export function grokbotModeChoices(installed: GrokBotAuthMode): GrokBotAuthMode[] {
  return installed === "box_relay" ? ["off", "box_relay", "direct"] : ["off", "direct"];
}

/** 网关透传口的地址：在跑就用真实绑定的，没在跑就按设置里的端口算——补丁写进盘上的是这个串。 */
export function passthroughUrlOf(gw: GatewayStatus | null): string | null {
  if (!gw) return null;
  return gw.running?.passthroughBaseUrl ?? `http://127.0.0.1:${gw.settings.passthroughPort}`;
}

const MODE_LABEL: Record<ModeGate, string> = {
  agent: "仅 Agent",
  agent_plan: "Agent + Plan",
  all: "全部模式（默认）",
};

const CURSOR_ARCHITECTURE_LABEL: Record<CursorDownload["architecture"], string> = {
  universal: "通用版",
  x64: "x64",
  arm64: "ARM64",
};

export function cursorDownloadLabel(
  release: CursorRelease,
  download: CursorDownload,
): string {
  const architecture =
    release.downloads.length > 1
      ? ` · ${CURSOR_ARCHITECTURE_LABEL[download.architecture]}`
      : "";
  return `下载 Cursor ${release.version}${architecture}`;
}

export function SandPage({ onGo }: { onGo: (r: Route) => void }) {
  const [release, setRelease] = useState<CursorRelease | null>(null);
  const [status, setStatus] = useState<SandStatus | null>(null);
  const [backups, setBackups] = useState<SandBackup[]>([]);
  const [error, setError] = useState<unknown>(null);
  const [busy, setBusy] = useState(false);
  const [progress, setProgress] = useState<SandProgress[]>([]);
  const [outcome, setOutcome] = useState<SandOutcome | null>(null);
  const [showBackups, setShowBackups] = useState(false);

  // 默认值和 Rust `InstallOptions::default()` 一致。故意不跟着盘上状态初始化：老安装是
  // 自摘要关，这一页要做的正是把它们带到新默认——选项摆在明处、旁边标着「盘上：…」，点
  // 「重新安装」就原地切。
  const [selfSummary, setSelfSummary] = useState(true);
  // 跟盘上走，并记住上次点过的值。默认关；重启 Nexus 后如果仍写死 false，
  // 卸装再装会把已经开着的 4.5→CUA 又写回去。
  const [grok45ViaCuaChoice, setGrok45ViaCuaChoice] = useState<boolean | null>(readGrok45ViaCua);
  const [modeGate, setModeGate] = useState<ModeGate>("all");
  const [relaunch, setRelaunch] = useState(true);
  // 「推理经本机网关」和自摘要相反：**跟着盘上状态初始化**（用户没碰之前 null = 盘上是什么就是什么）。
  // 它不是要迁移的默认值，而是一条已经装着的改道——重装时静默把它剥掉，Agent 面板会当场断线。
  const [viaGatewayChoice, setViaGatewayChoice] = useState<boolean | null>(null);
  // Grok 鉴权形态同理跟着盘上：null = 盘上是什么就是什么；盘上没装时默认直连（Box Relay 暂不露出）。
  const [grokbotChoice, setGrokbotChoice] = useState<GrokBotAuthMode | null>(null);
  const [gw, setGw] = useState<GatewayStatus | null>(null);

  // 网关状态：透传口地址、以及面板拦截那张卡的数据。网关模块坏了不该把 Sand 页一起拖倒，所以单独兜。
  const reloadGateway = useCallback(async () => {
    try {
      setGw(await gateway.status());
    } catch {
      setGw(null);
    }
  }, []);

  const reload = useCallback(async () => {
    setError(null);
    const results = await Promise.allSettled([
      sand.release().then(setRelease),
      sand.status().then(setStatus),
      sand.backups().then(setBackups),
    ]);
    for (const result of results) {
      if (result.status === "rejected") {
        setError(result.reason);
        break;
      }
    }
    await reloadGateway();
  }, [reloadGateway]);

  useEffect(() => {
    void reload();
  }, [reload]);

  // 拦截那张卡上的计数随 Agent 面板的请求变；补丁装着、改道开着的时候每 5 秒刷一次网关状态。
  useEffect(() => {
    if (!gw?.running || !status?.inferenceEndpoint) return;
    const t = window.setInterval(() => void reloadGateway(), 5000);
    return () => window.clearInterval(t);
  }, [gw?.running, status?.inferenceEndpoint, reloadGateway]);

  useEffect(() => {
    const off = onSandProgress((p) => setProgress((prev) => [...prev, p]));
    return () => {
      void off.then((f) => f());
    };
  }, []);

  async function run(action: () => Promise<SandOutcome>) {
    setBusy(true);
    setError(null);
    setOutcome(null);
    setProgress([]);
    try {
      const o = await action();
      setOutcome(o);
      setStatus(o.status);
      setBackups(await sand.backups());
    } catch (e) {
      setError(e);
    } finally {
      setBusy(false);
    }
  }

  const canInstall = !!status && status.versionSupported && status.foreignMarkers === 0 && !busy;
  const dryRunBlocks = status?.dryRun ? !status.dryRun.anchorsComplete : false;
  // 盘上装的自摘要开关和界面选的不同：重新安装会原地切换（只改 4884.js 两个字符）。
  const selfSummaryDiffers =
    !!status && status.selfSummary !== null && status.selfSummary !== selfSummary;
  const grok45ViaCua = grok45ViaCuaChoice ?? status?.grok45ViaCua ?? false;
  const grok45ViaCuaDiffers =
    !!status && status.grok45ViaCua !== null && status.grok45ViaCua !== grok45ViaCua;

  function setGrok45ViaCua(next: boolean) {
    setGrok45ViaCuaChoice(next);
    try {
      window.localStorage.setItem(GROK45_CUA_KEY, next ? "1" : "0");
    } catch {
      /* 记不住就当次有效 */
    }
  }
  const installedEndpoint = status?.inferenceEndpoint ?? null;
  const viaGateway = viaGatewayChoice ?? installedEndpoint !== null;
  const passthroughUrl = passthroughUrlOf(gw);
  // 要装进盘上的端点：开着就是透传口地址；关着是 null（盘上有的话安装时剥掉）。
  // 读不到网关状态时退回盘上现有的那个——不能因为一次读取失败就把装着的改道剥掉。
  const wantEndpoint = viaGateway ? (passthroughUrl ?? installedEndpoint) : null;
  const endpointDiffers = !!status && wantEndpoint !== installedEndpoint;
  const gatewayRunning = !!gw?.running;
  const installedGrokbot: GrokBotAuthMode = status?.grokbotAuth ?? "off";
  const grokbotAuth: GrokBotAuthMode =
    grokbotChoice ?? (installedGrokbot === "off" && !status?.installed ? "direct" : installedGrokbot);
  const grokbotDiffers = !!status && status.installed && grokbotAuth !== installedGrokbot;
  // Box Relay 会把 Stream 的 URL 改到 Box 去，根本到不了本机网关——两者互斥，Rust 侧也会拒。
  const grokbotConflictsWithGateway = viaGateway && grokbotAuth === "box_relay";
  const grokbotPrereqMissing =
    (grokbotAuth === "box_relay" && !status?.grokbotRelayConfigured) ||
    (grokbotAuth === "direct" && !status?.grokbotDirectConfigured);
  const optionsForInstall = {
    selfSummary,
    grok45ViaCua,
    modeGate,
    relaunch,
    inferenceEndpoint: wantEndpoint,
    grokbotAuth,
  };

  return (
    <div>
      <div className="page-head">
        <h1>Sand 通道</h1>
        <div className="row" style={{ flexWrap: "wrap", justifyContent: "flex-end" }}>
          {release?.downloads.map((download) => (
            <button
              key={download.architecture}
              type="button"
              className="btn btn-sm"
              title={`从 Cursor 官方 CDN 下载 ${release.version} ${CURSOR_ARCHITECTURE_LABEL[download.architecture]}`}
              onClick={() => void openUrl(download.url)}
            >
              {cursorDownloadLabel(release, download)}
              <Icon name="external" size={12} />
            </button>
          ))}
          {backups.length > 0 ? (
            <button type="button" className="btn btn-sm" onClick={() => setShowBackups(true)}>
              <Icon name="archive" size={13} />
              备份 {backups.length}
            </button>
          ) : null}
          <button type="button" className="btn btn-sm btn-icon btn-soft" onClick={() => void reload()} disabled={busy} title="刷新" aria-label="刷新">
            <Icon name="refresh" size={13} />
          </button>
        </div>
      </div>

      <ErrorNote error={error} onRetry={() => void reload()} />

      {!status ? (
        <div className="skeleton" style={{ height: 96 }} />
      ) : (
        <>
          <StatusCard status={status} />

          {!status.versionSupported ? (
            <div style={{ marginTop: 12 }}>
              <Banner
                tone="warn"
                title={`当前 Cursor ${status.cursorVersion ?? "未知"}，补丁只适配 ${status.supportedVersion}。`}
                hint={
                  release?.version === status.supportedVersion
                    ? `可用右上角的链接安装 ${status.supportedVersion}，或等待适配。Cursor 不会被改动。`
                    : "等待适配即可，Cursor 不会被改动。"
                }
              />
            </div>
          ) : null}

          {status.foreignMarkers > 0 ? (
            <div style={{ marginTop: 12 }}>
              <Banner
                tone="bad"
                title={`检测到 ${status.foreignMarkers} 处其它工具留下的改动。`}
                hint="先用原来的工具卸载，再回来安装。"
              />
            </div>
          ) : null}

          {status.legacyMarkers > 0 ? (
            <div style={{ marginTop: 12 }}>
              <Banner
                tone="warn"
                title={`有 ${status.legacyMarkers} 处旧版本标记。`}
                hint="点「安装」会原地升级到当前版本，不需要先卸载。"
              />
            </div>
          ) : null}

          {outcome ? <OutcomeBanner outcome={outcome} /> : null}

          <div className="section-head">
            <h2>{status.complete ? "已安装的补丁" : "安装预演"}</h2>
            <span className="faint tiny">{MARKER_ROWS.length} 个锚点</span>
          </div>

          <MarkerTable
            markers={status.markers}
            wouldHit={status.dryRun?.wouldHit ?? null}
          />

          {status.dryRun && !status.dryRun.anchorsComplete ? (
            <div style={{ marginTop: 12 }}>
              <Banner
                tone="bad"
                title="锚点不齐，安装会中止（Cursor 不会被改动）。"
                hint={status.dryRun.missing.join("；")}
              />
            </div>
          ) : null}

          {status.versionSupported ? (
            <>
              <div className="section-head">
                <h2>选项</h2>
              </div>
              <div className="opts">
                <Opt
                  icon="list"
                  title="上下文自动摘要"
                  desc="接近上限时后台压缩历史，长会话不卡死"
                  tone={selfSummaryDiffers ? "warn" : selfSummary ? "on" : undefined}
                >
                  {status.selfSummary !== null ? (
                    <Tag tone={selfSummaryDiffers ? "warn" : status.selfSummary ? "ok" : "default"}>盘上：{status.selfSummary ? "开" : "关"}</Tag>
                  ) : null}
                  <Switch checked={selfSummary} disabled={busy} onChange={setSelfSummary} label="上下文自动摘要" />
                </Opt>
                <Opt
                  icon="shield"
                  title="模式放行"
                  desc="哪些模式走这条通道；没放行的模式会直接报错"
                >
                  <select
                    className="input"
                    style={{ width: "auto" }}
                    aria-label="模式放行"
                    value={modeGate}
                    disabled={busy}
                    onChange={(e) => setModeGate(e.target.value as ModeGate)}
                  >
                    {(Object.keys(MODE_LABEL) as ModeGate[]).map((k) => (
                      <option key={k} value={k}>
                        {MODE_LABEL[k]}
                      </option>
                    ))}
                  </select>
                </Opt>
                <Opt
                  icon="shield"
                  title="Bot 通道"
                  desc="Agent 面板用 Grok Bot 的额度；选 GLM 5.2 会改走 premium，其它模型原样"
                  hint={
                    grokbotAuth === "box_relay"
                      ? "经 Grok Bot 客户端；和「推理经本机网关」互斥。"
                      : grokbotAuth === "direct"
                        ? "用哪个号，在账号页打开该账号的「Grok Bot」页选。要看实际解析模型，请同时开「推理经本机网关」。"
                        : undefined
                  }
                  tone={grokbotConflictsWithGateway ? "warn" : grokbotDiffers ? "warn" : grokbotAuth !== "off" ? "on" : undefined}
                >
                  {status.installed ? (
                    <Tag tone={grokbotDiffers ? "warn" : installedGrokbot !== "off" ? "ok" : "default"}>盘上：{GROKBOT_MODE_LABEL[installedGrokbot]}</Tag>
                  ) : null}
                  <select
                    className="input"
                    style={{ width: "auto" }}
                    aria-label="Bot 通道"
                    value={grokbotAuth}
                    disabled={busy}
                    onChange={(e) => setGrokbotChoice(e.target.value as GrokBotAuthMode)}
                  >
                    {grokbotModeChoices(installedGrokbot).map((k) => (
                      <option key={k} value={k}>
                        {GROKBOT_MODE_LABEL[k]}
                      </option>
                    ))}
                  </select>
                </Opt>
                <Opt
                  icon="shield"
                  title="Grok 4.5 走 CUA"
                  desc="面板选 Grok 4.5 时改发 sand-cua。只有部分号会落到 4.7，其余仍是 luna"
                  hint={
                    grokbotAuth === "off"
                      ? "Bot 通道关着时这项写不进补丁。"
                      : grok45ViaCua && status.grok45ViaCua === false
                        ? "开关已开，但盘上还是关：必须再点一次安装，选 4.5 才会改发 sand-cua。"
                        : "看「盘上」标签。开关开了还要再安装一次。Agent 日志里的 modelId 仍会写 grok-4.5，成功时会出现 [nexus-sand] resolved。"
                  }
                  tone={grok45ViaCuaDiffers ? "warn" : grok45ViaCua ? "on" : undefined}
                >
                  {status.grok45ViaCua !== null ? (
                    <Tag tone={grok45ViaCuaDiffers ? "warn" : status.grok45ViaCua ? "ok" : "default"}>
                      盘上：{status.grok45ViaCua ? "开" : "关"}
                    </Tag>
                  ) : null}
                  <Switch
                    checked={grok45ViaCua}
                    disabled={busy || grokbotAuth === "off"}
                    onChange={setGrok45ViaCua}
                    label="Grok 4.5 走 CUA"
                  />
                </Opt>
                <Opt
                  icon="gateway"
                  title="推理经本机网关"
                  desc="Agent 面板的模型调用先经本机网关；用量统计和面板拦截都靠它"
                  hint={passthroughUrl ? (viaGateway ? "开着时网关必须在跑，否则 Agent 面板连不上。" : undefined) : "读不到网关设置，这一项暂时装不了。"}
                  tone={endpointDiffers ? "warn" : viaGateway ? "on" : undefined}
                >
                  {installedEndpoint !== null ? (
                    <Tag tone={endpointDiffers ? "warn" : "ok"}>盘上：开</Tag>
                  ) : status.installed ? (
                    <Tag tone={endpointDiffers ? "warn" : "default"}>盘上：关</Tag>
                  ) : null}
                  <Switch
                    checked={viaGateway}
                    disabled={busy || (!passthroughUrl && !viaGateway)}
                    onChange={setViaGatewayChoice}
                    label="推理经本机网关"
                  />
                </Opt>
                <Opt icon="power" title="完成后重新启动 Cursor" tone={relaunch ? "on" : undefined}>
                  <Switch checked={relaunch} disabled={busy} onChange={setRelaunch} label="完成后重新启动 Cursor" />
                </Opt>
              </div>

              {grokbotConflictsWithGateway ? (
                <div style={{ marginTop: 12 }}>
                  <Banner tone="bad" title="Box Relay 和「推理经本机网关」不能同开。" hint="把 Bot 通道改成「开」或「关」。" />
                </div>
              ) : grokbotPrereqMissing ? (
                <div style={{ marginTop: 12 }}>
                  <Banner
                    tone="warn"
                    title={grokbotAuth === "box_relay" ? "Box Relay 还没就绪。" : "还没选用哪个号的 Grok 额度。"}
                    hint={grokbotAuth === "box_relay" ? "安装时会从 Grok Bot 客户端取（需要它已登录）。" : "到账号页打开要用的号 → 「Grok Bot」页 → 「用这个号」。"}
                    action={
                      <button type="button" className="btn btn-sm" onClick={() => onGo(go("accounts"))}>
                        去账号页
                      </button>
                    }
                  />
                </div>
              ) : null}

              {viaGateway && !gatewayRunning ? (
                <div style={{ marginTop: 12 }}>
                  <Banner
                    tone="warn"
                    title="网关没在跑。"
                    hint="安装时会自动打开网关和 Bot 通道。现在装也可以，不必先去「本地网关」页点。"
                  />
                </div>
              ) : viaGateway && gw && !gw.grokbotStream.enabled ? (
                <div style={{ marginTop: 12 }}>
                  <Banner
                    tone="warn"
                    title="Bot 通道还没开。"
                    hint="经网关的 Stream 必须用 grokBotToken。现在装会自动打开；也可以在下面拦截卡里先开。"
                  />
                </div>
              ) : null}

              <div className="row" style={{ marginTop: 16, justifyContent: "flex-end" }}>
                {status.installed ? (
                  <button
                    type="button"
                    className="btn btn-danger"
                    disabled={busy}
                    onClick={() => void run(() => sand.uninstall(relaunch))}
                  >
                    卸载，恢复原版
                  </button>
                ) : null}
                {status.installed ? (
                  <button
                    type="button"
                    className="btn btn-soft"
                    disabled={!canInstall || dryRunBlocks || grokbotConflictsWithGateway}
                    title="先卸再装，逼 Cursor 丢掉内存里的旧补丁。开关保持现在这样。"
                    onClick={() =>
                      void run(async () => {
                        await sand.uninstall(false);
                        if (viaGateway) {
                          if (!gw?.running) await gateway.start();
                          if (!gw?.grokbotStream.enabled) await gateway.setGrokbotStream(true);
                          await reloadGateway();
                        }
                        return sand.install(optionsForInstall);
                      })
                    }
                  >
                    卸载并重装
                  </button>
                ) : null}
                <button
                  type="button"
                  className="btn btn-primary"
                  disabled={!canInstall || dryRunBlocks || grokbotConflictsWithGateway}
                  title={
                    dryRunBlocks
                      ? "锚点不齐，装不上"
                      : grokbotConflictsWithGateway
                        ? "Box Relay 与经本机网关互斥"
                        : "会退出 Cursor，改动前自动备份"
                  }
                  onClick={() =>
                    void run(async () => {
                      if (viaGateway) {
                        if (!gw?.running) await gateway.start();
                        if (!gw?.grokbotStream.enabled) await gateway.setGrokbotStream(true);
                        await reloadGateway();
                      }
                      return sand.install(optionsForInstall);
                    })
                  }
                >
                  {busy ? <Spinner /> : <Icon name="sand" size={14} />}
                  {status.complete ? "重新安装（应用当前选项）" : "安装 Sand 补丁"}
                </button>
              </div>
            </>
          ) : null}
        </>
      )}

      {/* 面板拦截：Sand 改道过来的 Agent 面板流量在透传口上被看见、被记账、可被改写。它归这一页，
          因为没有补丁 + 改道就没有东西可拦；网关只是它借的那条管子。 */}
      {gw ? (
        <div style={{ marginTop: 28 }}>
          <InterceptCard status={gw} onChanged={reloadGateway} onGo={onGo} />
        </div>
      ) : null}

      <RemoteHosts />

      {busy || (progress.length > 0 && !outcome && error) ? (
        <ProgressModal steps={progress} busy={busy} onClose={() => setProgress([])} />
      ) : null}

      {showBackups ? (
        <BackupsModal
          backups={backups}
          busy={busy}
          onClose={() => setShowBackups(false)}
          onRestore={(id) => {
            const b = backups.find((x) => x.id === id);
            const ok = window.confirm(`把 ${b ? `「${b.operation}」` : "这次改动"}之前的原始文件写回？${relaunch ? "\n\n完成后会退出并重启 Cursor。" : ""}`);
            if (!ok) return;
            setShowBackups(false);
            void run(() => sand.restoreBackup(id, relaunch));
          }}
          onRemove={async (id) => {
            if (!window.confirm("删除这份备份？")) return;
            await sand.removeBackup(id);
            setBackups(await sand.backups());
          }}
        />
      ) : null}
    </div>
  );
}

/** 已匹配 / 需要匹配的锚点总数。`wouldHit` 是装上之后会补齐的那部分。 */
function tally(markers: MarkerCounts, wouldHit: MarkerCounts | null) {
  let need = 0;
  let have = 0;
  let add = 0;
  for (const row of MARKER_ROWS) {
    if (row.need === null) continue;
    need += row.need;
    const h = Math.min(markers[row.key], row.need);
    have += h;
    add += Math.min(wouldHit?.[row.key] ?? 0, row.need - h);
  }
  return { need, have, add };
}

/**
 * 顶部那张状态卡。
 *
 * 一眼要答的是「现在装成什么样了」，所以主角是**一个比例**：37 / 37 处锚点匹配。
 * 之前只有一行文字，看不出「不完整」到底差多少 —— 差 1 处和差 30 处是两回事。
 * 条子上第二段是「装上之后会补齐的部分」，用半透明品牌色，和已匹配的那段区分开。
 */
function StatusCard({ status }: { status: SandStatus }) {
  const t = tally(status.markers, status.dryRun?.wouldHit ?? null);
  const state = status.complete ? "Sand 通道已启用" : status.installed ? "安装不完整" : "Cursor 原版";
  const tone = status.complete ? "ok" : status.installed ? "warn" : "default";
  const dot = status.complete ? "is-ok" : status.installed ? "is-warn" : "is-off";

  return (
    <div className={`card sandcard ${status.complete ? "is-live" : ""}`}>
      <div className="sandcard-head">
        <span className={`sand-dot ${dot}`} />
        <strong className="sandcard-state">{state}</strong>
        <Tag tone={tone}>{status.complete ? "已启用" : status.installed ? "不完整" : "未安装"}</Tag>
        <span className="grow" />
        <span className="sandcard-n">
          <b>{t.have}</b>
          <span className="faint"> / {t.need}</span> 处匹配
        </span>
      </div>

      <div className="sand-bar">
        <i className="is-have" style={{ width: `${t.need ? (t.have / t.need) * 100 : 0}%` }} />
        <i className="is-add" style={{ width: `${t.need ? (t.add / t.need) * 100 : 0}%` }} />
      </div>

      <div className="sandcard-meta">
        <span className="mono">Cursor {status.cursorVersion ?? "未知"}</span>
        {status.versionSupported ? (
          <Tag tone="ok">已适配</Tag>
        ) : (
          <Tag tone="warn">适配 {status.supportedVersion}</Tag>
        )}
        {status.patchedFiles.length > 0 ? <span>已改 {status.patchedFiles.length} 个文件</span> : null}
        {status.dryRun && status.dryRun.filesToChange > 0 ? (
          <span>将改动 {status.dryRun.filesToChange} 个文件</span>
        ) : null}
        {status.remainingIde > 0 ? <span>残留 ide {status.remainingIde} 处</span> : null}
      </div>
    </div>
  );
}

/**
 * 补丁清单。
 *
 * 每行是「第几个 · 锚点名 · 这是干嘛的 · 匹配了几处 · 齐没齐」。之前只有英文标识和一个
 * 数字，对着代码才读得懂；这一页恰恰是给「装之前想看看会改什么」的人看的。
 */
function MarkerTable({
  markers,
  wouldHit,
}: {
  markers: MarkerCounts;
  wouldHit: MarkerCounts | null;
}) {
  return (
    <div className="patches">
      {MARKER_ROWS.map((row, i) => {
        const have = markers[row.key];
        const add = wouldHit?.[row.key] ?? 0;
        const optional = row.need === null;
        // 三态，不是两态：**已经到位**和**装完才会到位**不该长得一样 —— 后者现在还没生效。
        const state = optional
          ? "none"
          : have === row.need
            ? "ok"
            : have + add === row.need
              ? "will"
              : "bad";
        return (
          <div key={row.key} className={`patch${optional ? " is-optional" : ""}`}>
            <span className="patch-i">{String(i + 1).padStart(2, "0")}</span>
            <span className="patch-k truncate">{row.label}</span>
            <span className="patch-d truncate">{row.desc}</span>
            <span className="patch-n">
              <b>{have}</b>
              {add > 0 ? <span className="patch-add">+{add}</span> : null}
              {!optional ? <span className="faint"> / {row.need}</span> : null}
            </span>
            <span
              className={`patch-s is-${state}`}
              title={
                state === "will" ? "装完才会到位" : state === "bad" ? "锚点没对上" : undefined
              }
            >
              {state === "none" ? "—" : state === "bad" ? <Icon name="close" size={11} /> : <Icon name="check" size={12} />}
            </span>
          </div>
        );
      })}
    </div>
  );
}

/** 结果横幅。`wrote` 决定能不能说「Cursor 没有被改动」。 */
function OutcomeBanner({ outcome }: { outcome: SandOutcome }) {
  const verb =
    outcome.operation === "install" ? "安装" : outcome.operation === "uninstall" ? "卸载" : "还原";
  if (!outcome.wrote) {
    return (
      <div style={{ marginTop: 12 }}>
        <Banner
          tone="default"
          title={`无需${verb}：文件已经是目标状态。`}
          hint={
            outcome.operation === "install"
              ? outcome.cursorRelaunched
                ? "已重启 Cursor，让它重新加载补丁。Agent 日志里的 modelId 仍会写 grok-4.5；成功时会出现 [nexus-sand] remap / resolved。"
                : "请完全退出 Cursor（活动监视器里确认没了）再打开，否则它还在用内存里的旧 4884.js。"
              : "Cursor 没有被改动。"
          }
        />
      </div>
    );
  }
  return (
    <div style={{ marginTop: 12 }}>
      <Banner
        tone="ok"
        title={`${verb}完成，改动了 ${outcome.filesWritten} 个文件。`}
        hint={
          (outcome.cursorRelaunched ? "Cursor 已重新启动。" : "请手动打开 Cursor。") +
          (outcome.backupId ? ` 改动前的文件已备份（${outcome.backupId}）。` : "")
        }
      />
    </div>
  );
}

function ProgressModal({
  steps,
  busy,
  onClose,
}: {
  steps: SandProgress[];
  busy: boolean;
  onClose: () => void;
}) {
  return (
    <Modal
      title={busy ? "正在处理" : "已中止"}
      subtitle={busy ? "会退出 Cursor；未保存的工作请先保存。" : undefined}
      onClose={busy ? () => {} : onClose}
      compact
    >
      <div className="steps">
        {steps.map((s, i) => (
          <div key={`${s.step}-${i}`} className="step">
            <span className="step-dot">{i === steps.length - 1 && busy ? <Spinner /> : "✓"}</span>
            <span>{STEP_LABEL[s.step]}</span>
            <span className="faint tiny">{s.detail}</span>
          </div>
        ))}
      </div>
    </Modal>
  );
}

function BackupsModal({
  backups,
  busy,
  onClose,
  onRestore,
  onRemove,
}: {
  backups: SandBackup[];
  busy: boolean;
  onClose: () => void;
  onRestore: (id: string) => void;
  onRemove: (id: string) => Promise<void>;
}) {
  return (
    <Modal title="备份" subtitle="改动前的原始文件，可整份写回。" onClose={onClose} wide>
      {backups.length === 0 ? (
        <Empty title="还没有备份" />
      ) : (
        <div className="opts is-flush">
          {[...backups].reverse().map((b) => (
            <Opt
              key={b.id}
              icon="archive"
              tone={b.state === "rolled_back" ? "bad" : b.state === "committed" ? undefined : "warn"}
              title={
                <>
                  <span className="mono">{b.operation}</span>
                  <Tag tone={b.state === "committed" ? "ok" : b.state === "rolled_back" ? "bad" : "warn"}>{b.state}</Tag>
                </>
              }
              desc={`${timeAgo(b.createdAt)} · Cursor ${b.cursorVersion} · ${b.files} 个文件${b.error ? ` · ${b.error}` : ""}`}
            >
              <span className="opt-acts">
                <button
                  type="button"
                  className="btn btn-sm btn-icon btn-quiet"
                  data-tip="写回这份备份"
                  aria-label="还原这份备份"
                  disabled={busy}
                  onClick={() => onRestore(b.id)}
                >
                  <Icon name="undo" size={13} />
                </button>
                <button
                  type="button"
                  className="btn btn-sm btn-icon btn-quiet btn-danger"
                  aria-label="删除"
                  disabled={busy}
                  onClick={() => void onRemove(b.id)}
                >
                  <Icon name="trash" size={13} />
                </button>
              </span>
            </Opt>
          ))}
        </div>
      )}
    </Modal>
  );
}
