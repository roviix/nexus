/**
 * Sand 页的第二张卡：远程主机（remote SSH）。同一份补丁，对象在远端。
 *
 * 出网方式是主轴（见 `ROUTES`）：经本机网关 / 经本机代理 / 远程自己出网。前两条都要一条回到
 * 本机的隧道（ssh 会话里的多路复用中继，不是 `ssh -R`）。ssh 认证不归我们管。
 * 一台主机一张卡：三盏灯（补丁 / 隧道 / 本机那头）加一句结论；「验一遍」从远程实地打一次，报出断在哪一跳。
 */
import { useCallback, useEffect, useState } from "react";
import { gateway, onSandRemoteProgress, sandRemote } from "../../ipc/api";
import type {
  MarkerCounts,
  ProbeReport,
  RemoteHost,
  RemoteHostView,
  RemoteOutcome,
  RemoteOverview,
  RemoteRoute,
  RemoteStatus,
  SandProgress,
  TunnelStatus,
} from "../../ipc/types";
import { Banner, Empty, ErrorNote, Icon, Modal, Opt, Spinner, Tag } from "../../ui/primitives";
import { STEP_LABEL } from "../SandPage";

/** 新主机默认的远程端口；与 Rust `DEFAULT_REMOTE_PORT` 一致。 */
export const DEFAULT_REMOTE_PORT = 41777;

/** 三条出网路线。每条把代价写出来：经网关有号池接力，经代理链路短但只用远程当前的号。 */
export const ROUTES: Array<{ id: RemoteRoute; label: string; desc: string; icon: string }> = [
  {
    id: "gateway",
    label: "经本机网关",
    desc: "经隧道回到本机网关。多号接力、额度用尽自动换号、面板拦截都在这条路上。",
    icon: "gateway",
  },
  {
    id: "proxy",
    label: "经本机代理",
    desc: "经隧道走本机的代理出网。链路短一跳，但只用远程当前登录的号，没有轮换。",
    icon: "globe",
  },
  {
    id: "direct",
    label: "远程自己出网",
    desc: "远程本来就能出网时用。不改端点、不起隧道。",
    icon: "external",
  },
];

export const ROUTE_LABEL: Record<RemoteRoute, string> = {
  gateway: "经本机网关",
  proxy: "经本机代理",
  direct: "远程自己出网",
};

/**
 * Server profile 的期望值——与 Rust `RuleId::expected_for(LayoutProfile::Server)` 对齐。
 * 远程只有 7 个目标文件（少了 Electron/UI 侧那 4 个），所以 client-type 是 2 而不是 23，
 * 有三类干脆是 0（它们的锚点全在 workbench 里）。这些数字是拿原版远程 bundle 量出来的。
 */
export const SERVER_MARKER_NEED: Partial<Record<keyof MarkerCounts, number>> = {
  clientType: 2,
  managedSubagentSession: 1,
  managedLocalRoute: 1,
  localRuntimeLoad: 1,
  inferenceStream: 1,
  agentHostIdentity: 1,
  agentHostMoveExec: 1,
  managedTaskTool: 1,
  managedActionRoute: 1,
  subagentResumeMode: 1,
  subagentInteractionBubble: 1,
  contextWindow: 1,
};

/** 远程一共要匹配多少处、已匹配多少处。 */
export function serverTally(markers: MarkerCounts) {
  let need = 0;
  let have = 0;
  for (const [key, n] of Object.entries(SERVER_MARKER_NEED) as Array<[keyof MarkerCounts, number]>) {
    need += n;
    have += Math.min(markers[key], n);
  }
  return { need, have };
}

/** Rust `Result<RemoteStatus, String>` 的 JSON 形状。 */
export function unwrapStatus(
  r: RemoteHostView["status"],
): { ok: RemoteStatus; err?: undefined } | { ok?: undefined; err: string } {
  return "Ok" in r ? { ok: r.Ok } : { err: r.Err };
}

/**
 * 「远程盘上 / Cursor 设置里已经指着我们、隧道却不通」是所有故障形态里最没有提示的一种，
 * 所以单独判一次。
 *
 * 两条路都有这个毛病，只是那份「永久状态」放在不同地方：网关模式是**写进远程 bundle 的端点**，
 * 代理模式是**写进 Cursor 设置的 `HTTP_PROXY`**。隧道却只活在本应用进程里。两者一旦不同步，
 * 远程每次请求都打在一个没人监听的端口上（`ECONNREFUSED` → 重试），而用户在 Cursor 那边只看得到
 * 一直转圈——桌面端这边「已打补丁」「网关在跑」还都是绿的。返回 `null` = 这条链路没问题。
 */
export function tunnelBlocker(
  /** 远程那一侧指着我们的证据：端点地址，或代理地址。null = 没指着我们，隧道断了也无妨。 */
  pinned: string | null,
  tunnel: TunnelStatus,
): { title: string; canStart: boolean } | null {
  if (!pinned) return null;
  const tail = `远程的请求会打在没人监听的 ${pinned} 上（ECONNREFUSED），Cursor 那边表现为一直转圈。`;
  if (tunnel.phase === "connected") return null;
  if (tunnel.phase === "stopped") return { title: `隧道没开——${tail}`, canStart: true };
  return {
    title: `隧道还没连上（${tunnel.phase === "connecting" ? "连接中" : `重连中，第 ${tunnel.reconnects} 次`}）——${tail}`,
    canStart: false,
  };
}

/** 这台主机的「远程那侧已经指着我们」是什么。两条路各看各的永久状态。 */
export function pinnedTarget(view: RemoteHostView, status: RemoteStatus | undefined): string | null {
  if (view.host.route === "proxy") return view.proxyConfigured;
  if (view.host.route === "direct") return null;
  return status?.inferenceEndpoint ?? null;
}

/**
 * 网关模式：盘上写的端点和现在设置该写的端点不一样（改过远程端口、或早期版本装的是
 * 「两端同口」那种）。隧道再通也没用——远程打的是旧端口。返回要说的那句话，null = 一致。
 */
export function endpointDrift(view: RemoteHostView, status: RemoteStatus | undefined): string | null {
  if (view.host.route !== "gateway" || !view.expectedEndpoint) return null;
  const onDisk = status?.inferenceEndpoint ?? null;
  if (!onDisk || onDisk === view.expectedEndpoint) return null;
  return `远程写的端点是 ${onDisk}，现在的设置是 ${view.expectedEndpoint}，重新安装一次才会改过来。`;
}

/** 探针结果翻成一句结论 + 下一步。分阶段说断在哪一跳，含糊成一句「失败」等于没验。 */
export function probeVerdict(r: ProbeReport, route: RemoteRoute): { tone: "ok" | "bad"; title: string; hint: string } {
  if (r.ok) {
    return {
      tone: "ok",
      title: route === "proxy" ? `链路通了：远程 → 隧道 → 本机代理 → 外网（HTTP ${r.status}）。` : `链路通了：远程 → 隧道 → 本机网关（HTTP ${r.status}）。`,
      hint: "这里验的是链路，不是鉴权。",
    };
  }
  const detail = r.detail ? `（${r.detail}）` : "";
  switch (r.stage) {
    case "tunnel":
      return {
        tone: "bad",
        title: `断在第一跳：远程连不上 127.0.0.1:${r.remotePort}${detail}`,
        hint: route === "proxy" ? "看隧道是不是「已连接」、本机代理开着没、端口对不对。" : "看隧道是不是「已连接」，再把本机网关开起来。",
      };
    case "proxy":
      return {
        tone: "bad",
        title: `隧道通了，但本机那个端口不像 HTTP 代理${detail}`,
        hint: "确认这个端口是代理的 HTTP / 混合口，不是 SOCKS 专用口。",
      };
    case "tls":
      return {
        tone: "bad",
        title: `代理接受了 CONNECT，但 TLS 握手失败${detail}`,
        hint: "多半是代理在做 TLS 中间人，或它连不上外网。换一条出口试试。",
      };
    default:
      return {
        tone: "bad",
        title: `拿不到 HTTP 应答${detail}`,
        hint: "前几跳都通了，最后一跳没回话。",
      };
  }
}

/** 隧道那一格怎么写。`farEndUp` = 本机那头（网关 / 代理）有人听。 */
export function tunnelLabel(
  tunnel: TunnelStatus,
  farEndUp: boolean,
): { tone: "ok" | "warn" | "bad" | "default"; text: string } {
  switch (tunnel.phase) {
    case "connected":
      return farEndUp
        ? { tone: "ok", text: tunnel.streams > 0 ? `隧道已连接 · ${tunnel.streams} 条连接` : "隧道已连接" }
        : { tone: "warn", text: "隧道已连接，本机那头没人接" };
    case "connecting":
      return { tone: "warn", text: "隧道连接中" };
    case "reconnecting":
      return { tone: "bad", text: `隧道重连中（第 ${tunnel.reconnects} 次）` };
    default:
      return { tone: "default", text: "隧道未开" };
  }
}

export function RemoteHosts() {
  const [overview, setOverview] = useState<RemoteOverview | null>(null);
  const [error, setError] = useState<unknown>(null);
  const [loading, setLoading] = useState(false);
  const [busyHost, setBusyHost] = useState<string | null>(null);
  const [progress, setProgress] = useState<SandProgress[]>([]);
  const [outcome, setOutcome] = useState<RemoteOutcome | null>(null);
  const [adding, setAdding] = useState(false);
  /** 每台主机最近一次探针结果。探一次要几秒，所以留在界面上直到下一次。 */
  const [probes, setProbes] = useState<Record<string, ProbeReport>>({});
  const [probing, setProbing] = useState<string | null>(null);

  const reload = useCallback(async () => {
    setError(null);
    setLoading(true);
    try {
      setOverview(await sandRemote.overview());
    } catch (e) {
      setError(e);
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    void reload();
  }, [reload]);

  useEffect(() => {
    const off = onSandRemoteProgress((p) => setProgress((prev) => [...prev, p]));
    return () => {
      void off.then((f) => f());
    };
  }, []);

  // 隧道状态在后台自己变（重连、连接数），列表开着时每 3 秒扫一遍。overview 会走 ssh，太重；
  // 这里只刷隧道那一小块。
  useEffect(() => {
    if (!overview || overview.hosts.length === 0) return;
    const id = window.setInterval(async () => {
      const hosts = overview.hosts.map((h) => h.host.host);
      const tunnels = await Promise.all(hosts.map((h) => sandRemote.tunnelStatus(h)));
      setOverview((prev) =>
        prev
          ? {
              ...prev,
              hosts: prev.hosts.map((h, i) => ({ ...h, tunnel: tunnels[i] ?? h.tunnel })),
            }
          : prev,
      );
    }, 3000);
    return () => window.clearInterval(id);
  }, [overview?.hosts.length]); // eslint-disable-line react-hooks/exhaustive-deps

  async function run(host: string, action: () => Promise<RemoteOutcome>) {
    setBusyHost(host);
    setError(null);
    setOutcome(null);
    setProgress([]);
    try {
      const o = await action();
      setOutcome(o);
      await reload();
    } catch (e) {
      setError(e);
    } finally {
      setBusyHost(null);
    }
  }

  async function toggleTunnel(host: string, on: boolean) {
    setError(null);
    try {
      const t = on ? await sandRemote.tunnelStart(host) : await sandRemote.tunnelStop(host);
      setOverview((prev) =>
        prev
          ? { ...prev, hosts: prev.hosts.map((h) => (h.host.host === host ? { ...h, tunnel: t } : h)) }
          : prev,
      );
    } catch (e) {
      setError(e);
    }
  }

  async function remove(host: string) {
    setError(null);
    try {
      await sandRemote.removeHost(host);
      await reload();
    } catch (e) {
      setError(e);
    }
  }

  /** 从远程实地验一次。失败也是结果，留在卡片上给人读。 */
  async function probe(host: string) {
    setProbing(host);
    setError(null);
    try {
      const r = await sandRemote.probe(host);
      setProbes((prev) => ({ ...prev, [host]: r }));
    } catch (e) {
      setError(e);
    } finally {
      setProbing(null);
    }
  }

  /**
   * 改一台主机的设置。改出网方式会连带动 Cursor 的代理设置（Rust 侧做），可能因为
   * 「代理没开」「有个全局代理」而失败——那时候列表保持原样，错误照实报。
   */
  async function saveHost(host: RemoteHost) {
    setError(null);
    try {
      await sandRemote.updateHost(host);
      await reload();
      return true;
    } catch (e) {
      setError(e);
      return false;
    }
  }

  // 网关是隧道那一头的接收方；这两个动作都是「本地网关」页也能做的事，这里给个就近入口，
  // 免得用户在两个 tab 之间来回找。改 client-type 的语义写在 GatewayGuard 的文案里。
  const [gatewayBusy, setGatewayBusy] = useState(false);
  async function fixGateway(what: "start" | "sand") {
    setGatewayBusy(true);
    setError(null);
    try {
      if (what === "sand") await gateway.updateSettings({ clientType: "sand" });
      else await gateway.start();
      setOverview(await sandRemote.overview());
    } catch (e) {
      setError(e);
    } finally {
      setGatewayBusy(false);
    }
  }

  return (
    <div style={{ marginTop: 28 }}>
      <div className="section-head">
        <h2>远程主机</h2>
        <div className="row">
          <button type="button" className="btn btn-sm btn-icon btn-soft" onClick={() => void reload()} disabled={loading} title="刷新" aria-label="刷新">
            {loading ? <Spinner /> : <Icon name="refresh" size={13} />}
          </button>
          <button type="button" className="btn btn-sm btn-primary" onClick={() => setAdding(true)}>
            <Icon name="plus" size={13} />
            添加主机
          </button>
        </div>
      </div>

      <ErrorNote error={error} onRetry={() => void reload()} />

      {/* 网关那两条提示只对「经本机网关」的主机成立；一台都没有的话不该摆在这儿。 */}
      {overview && overview.hosts.some((h) => h.host.route === "gateway") ? (
        <GatewayGuard
          overview={overview}
          busy={gatewayBusy}
          onStart={() => void fixGateway("start")}
          onUseSand={() => void fixGateway("sand")}
        />
      ) : null}

      {outcome ? <RemoteOutcomeBanner outcome={outcome} /> : null}

      {!overview ? (
        <div className="skeleton" style={{ height: 72 }} />
      ) : overview.hosts.length === 0 ? (
        <Empty title="还没有远程主机">填 ssh 认的名字（config 里的别名或 user@host）。</Empty>
      ) : (
        <div className="list">
          {overview.hosts.map((view) => (
            <HostCard
              key={view.host.host}
              view={view}
              overview={overview}
              busy={busyHost === view.host.host}
              anyBusy={busyHost !== null}
              probe={probes[view.host.host]}
              probing={probing === view.host.host}
              onProbe={() => void probe(view.host.host)}
              onInstall={() =>
                void run(view.host.host, () => sandRemote.install(view.host.host, view.host.route))
              }
              onUninstall={() => void run(view.host.host, () => sandRemote.uninstall(view.host.host))}
              onTunnel={(on) => void toggleTunnel(view.host.host, on)}
              onRemove={() => void remove(view.host.host)}
              onSave={saveHost}
            />
          ))}
        </div>
      )}

      {busyHost || (progress.length > 0 && !outcome && error) ? (
        <Modal
          title={busyHost ? `正在处理 ${busyHost}` : "已中止"}
          subtitle={busyHost ? "会重启远程的 cursor-server；那边未保存的工作请先保存。" : undefined}
          onClose={busyHost ? () => {} : () => setProgress([])}
          compact
        >
          <div className="steps">
            {progress.map((s, i) => (
              <div key={`${s.step}-${i}`} className="step">
                <span className="step-dot">{i === progress.length - 1 && busyHost ? <Spinner /> : "✓"}</span>
                <span>{STEP_LABEL[s.step]}</span>
                <span className="faint tiny">{s.detail}</span>
              </div>
            ))}
          </div>
        </Modal>
      ) : null}

      {adding ? (
        <AddHostModal
          onClose={() => setAdding(false)}
          onAdded={async () => {
            setAdding(false);
            await reload();
          }}
        />
      ) : null}
    </div>
  );
}

function HostCard({
  view,
  overview,
  busy,
  anyBusy,
  probe,
  probing,
  onProbe,
  onInstall,
  onUninstall,
  onTunnel,
  onRemove,
  onSave,
}: {
  view: RemoteHostView;
  overview: RemoteOverview;
  busy: boolean;
  anyBusy: boolean;
  probe?: ProbeReport;
  probing: boolean;
  onProbe: () => void;
  onInstall: () => void;
  onUninstall: () => void;
  onTunnel: (on: boolean) => void;
  onRemove: () => void;
  onSave: (host: RemoteHost) => Promise<boolean>;
}) {
  const [open, setOpen] = useState(false);
  const st = unwrapStatus(view.status);
  const label = view.host.label || view.host.host;
  const s = st.ok;
  const tally = s ? serverTally(s.markers) : null;
  const route = view.host.route;

  // 三盏灯：补丁、隧道、本机那头（网关在跑 / 代理在听）。都亮才算通。
  const patched = !!s?.complete;
  const needsTunnel = route !== "direct";
  const farEndUp = route === "gateway" ? overview.gatewayRunning : route === "proxy" ? view.localListening : true;
  const tunnelOn = view.tunnel.phase === "connected";
  const live = patched && (!needsTunnel || (tunnelOn && farEndUp));
  // 远程那侧已经指着我们、承接的那一头却不在——这一条要摊开说，光把状态点变黄没人看得懂。
  const blocker = needsTunnel ? tunnelBlocker(pinnedTarget(view, s), view.tunnel) : null;
  const drift = endpointDrift(view, s);
  const tunnelChip = tunnelLabel(view.tunnel, farEndUp);

  return (
    <div className="card" style={{ marginBottom: 10 }}>
      <div className="row" style={{ gap: 10, alignItems: "center" }}>
        <span className={`sand-dot ${live ? "is-ok" : patched ? "is-warn" : "is-off"}`} />
        <strong>{label}</strong>
        {view.host.label ? <span className="mono faint tiny">{view.host.host}</span> : null}
        <span className="grow" />
        {st.err ? (
          <Tag tone="bad">连不上</Tag>
        ) : s ? (
          <>
            {s.selected ? (
              <Tag tone={s.commitMatchesLocal ? "ok" : "warn"}>
                {s.commitMatchesLocal ? "与本机同版本" : `server ${s.selected.version}`}
              </Tag>
            ) : (
              <Tag tone="warn">没找到 server</Tag>
            )}
            <Tag tone={patched ? "ok" : s.markers.clientType + s.markers.inferenceStream > 0 ? "warn" : "default"}>
              {patched ? "已打补丁" : tally && tally.have > 0 ? "补丁不完整" : "原版"}
            </Tag>
            {needsTunnel ? <Tag tone={blocker ? "bad" : tunnelChip.tone}>{tunnelChip.text}</Tag> : null}
          </>
        ) : null}
        <button type="button" className="btn btn-sm btn-quiet" onClick={() => setOpen((v) => !v)}>
          {open ? "收起" : "详情"}
        </button>
      </div>

      {st.err ? (
        <div style={{ marginTop: 10 }}>
          <Banner tone="bad" title="ssh 连不上这台主机。" hint={st.err} />
        </div>
      ) : null}

      {blocker ? (
        <div style={{ marginTop: 10 }}>
          <Banner
            tone="bad"
            title={blocker.title}
            hint={view.tunnel.lastError ?? "只有这条隧道能把远程的推理接回本机。"}
            action={
              blocker.canStart ? (
                <button type="button" className="btn btn-sm btn-primary" disabled={anyBusy} onClick={() => onTunnel(true)}>
                  开启隧道
                </button>
              ) : undefined
            }
          />
        </div>
      ) : null}

      {drift ? (
        <div style={{ marginTop: 10 }}>
          <Banner
            tone="warn"
            title={drift}
            action={
              <button type="button" className="btn btn-sm" disabled={anyBusy || !s?.versionSupported} onClick={onInstall}>
                重新安装
              </button>
            }
          />
        </div>
      ) : null}

      {probe ? (
        <div style={{ marginTop: 10 }}>
          <ProbeBanner report={probe} route={route} />
        </div>
      ) : null}

      {s ? (
        <div className="sandcard-meta" style={{ marginTop: 10 }}>
          {tally ? (
            <span>
              <b>{tally.have}</b>
              <span className="faint"> / {tally.need}</span> 处匹配
            </span>
          ) : null}
          <span>
            出网：<span className="faint">{ROUTE_LABEL[route]}</span>
          </span>
          <span>
            {route === "proxy" ? "远程的 HTTP_PROXY：" : "推理出口："}
            {route === "proxy" ? (
              view.proxyConfigured ? (
                <span className="mono">{view.proxyConfigured}</span>
              ) : (
                <span className="faint">未配置</span>
              )
            ) : s.inferenceEndpoint ? (
              <span className="mono">{s.inferenceEndpoint}</span>
            ) : (
              <span className="faint">远程直连</span>
            )}
          </span>
        </div>
      ) : null}

      {open ? (
        <div style={{ marginTop: 12 }}>
          {s && !s.versionSupported && s.selected ? (
            <div style={{ marginBottom: 8 }}>
              <Banner
                tone="warn"
                title={`远程 server 是 ${s.selected.version}，补丁只适配本机那个版本。`}
                hint="在 Cursor 里重连一次这台远程，让它把与本机同版本的 server 装上。"
              />
            </div>
          ) : null}

          <div className="opts is-flush">
            {/* 远程可能留着好几份 server；只动与本机同 commit 的那一份。 */}
            {s?.selected ? (
              <Opt
                icon="box"
                title="server"
                desc={
                  <>
                    <span className="mono">{s.selected.commit.slice(0, 12)}</span>
                    {s.servers.length > 1 ? ` · 共 ${s.servers.length} 份，只改这份` : null}
                    {s.patchedFiles.length > 0 ? ` · 已改 ${s.patchedFiles.length} 个文件` : null}
                  </>
                }
              >
                <span className="faint tiny mono">{s.selected.version}</span>
              </Opt>
            ) : null}
            {needsTunnel ? (
              <Opt
                icon="switcher"
                title="隧道"
                desc={
                  view.localPort ? (
                    <span className="mono">
                      远程 127.0.0.1:{view.host.remotePort} → 本机 127.0.0.1:{view.localPort}
                    </span>
                  ) : (
                    <span className="faint">本机那头还没有可用的端口</span>
                  )
                }
                hint={view.tunnel.lastError && view.tunnel.phase !== "connected" ? view.tunnel.lastError : "断了自动重连，应用退出时一并关闭。"}
                tone={view.tunnel.phase === "stopped" ? undefined : view.tunnel.phase === "connected" ? "on" : "warn"}
              >
                <div className="row" style={{ gap: 6 }}>
                  <button
                    type="button"
                    className="btn btn-sm btn-soft"
                    disabled={anyBusy || probing}
                    title="从远程实地打一次，看断在哪一跳"
                    onClick={onProbe}
                  >
                    {probing ? <Spinner /> : <Icon name="shield" size={13} />}
                    {probing ? "验证中…" : "验一遍"}
                  </button>
                  <button
                    type="button"
                    className="btn btn-sm"
                    disabled={anyBusy || !view.localPort}
                    onClick={() => onTunnel(view.tunnel.phase === "stopped")}
                  >
                    {view.tunnel.phase === "stopped" ? "开启隧道" : "关闭隧道"}
                  </button>
                </div>
              </Opt>
            ) : null}
          </div>

          <RoutePicker host={view.host} overview={overview} disabled={anyBusy} onSave={onSave} />

          <div className="row" style={{ marginTop: 12, justifyContent: "space-between" }}>
            <button type="button" className="btn btn-sm btn-quiet" disabled={anyBusy} onClick={onRemove}>
              移除主机
            </button>
            <div className="row">
              {s && tally && tally.have > 0 ? (
                <button type="button" className="btn btn-sm btn-danger" disabled={anyBusy} onClick={onUninstall}>
                  卸载，恢复原版
                </button>
              ) : null}
              <button
                type="button"
                className="btn btn-sm btn-primary"
                disabled={anyBusy || !s || !s.selected || !s.versionSupported}
                title="会重启远程的 cursor-server，改动前自动备份"
                onClick={onInstall}
              >
                {busy ? <Spinner /> : <Icon name="sand" size={14} />}
                {patched ? "重新安装（应用当前选项）" : "安装到远程"}
              </button>
            </div>
          </div>
        </div>
      ) : null}
    </div>
  );
}

/** 出网方式：三选一 + 端口。改完立刻落盘（Rust 侧连带写 / 摘 Cursor 的代理设置），失败时选项回到原样。 */
function RoutePicker({
  host,
  overview,
  disabled,
  onSave,
}: {
  host: RemoteHost;
  overview: RemoteOverview;
  disabled: boolean;
  onSave: (host: RemoteHost) => Promise<boolean>;
}) {
  const [saving, setSaving] = useState(false);
  const [remotePort, setRemotePort] = useState(String(host.remotePort));
  const [proxyPort, setProxyPort] = useState(host.proxyPort ? String(host.proxyPort) : "");

  async function pick(route: RemoteRoute) {
    if (route === host.route) return;
    setSaving(true);
    await onSave({ ...host, route });
    setSaving(false);
  }

  async function savePorts() {
    const remote = Number(remotePort.trim());
    const local = proxyPort.trim() ? Number(proxyPort.trim()) : null;
    if (!Number.isInteger(remote) || remote < 1024 || remote > 65535) return;
    if (local !== null && (!Number.isInteger(local) || local < 1 || local > 65535)) return;
    if (remote === host.remotePort && local === host.proxyPort) return;
    setSaving(true);
    const ok = await onSave({ ...host, remotePort: remote, proxyPort: local });
    if (!ok) {
      setRemotePort(String(host.remotePort));
      setProxyPort(host.proxyPort ? String(host.proxyPort) : "");
    }
    setSaving(false);
  }

  const effectiveLocal = host.proxyPort ?? overview.detectedProxyPort;

  return (
    <div style={{ marginTop: 12 }}>
      <div className="sect-cap" style={{ marginBottom: 8 }}>
        <span>出网方式</span>
        <span className="sect-aside">远程的推理从哪儿出去</span>
      </div>
      <div className="opts is-flush">
        {ROUTES.map((r) => (
          <Opt key={r.id} icon={r.icon} title={r.label} desc={r.desc} tone={host.route === r.id ? "on" : undefined}>
            <button
              type="button"
              className={host.route === r.id ? "btn btn-sm btn-primary" : "btn btn-sm"}
              disabled={disabled || saving}
              onClick={() => void pick(r.id)}
            >
              {saving && host.route !== r.id ? <Spinner /> : host.route === r.id ? "使用中" : "改用这条"}
            </button>
          </Opt>
        ))}
      </div>

      {host.route !== "direct" ? (
        <div style={{ marginTop: 10 }}>
          <div className="row" style={{ gap: 8, flexWrap: "wrap" }}>
            <label className="field" style={{ flex: "1 1 150px" }}>
              <span>远程端口</span>
              <input
                className="input mono"
                inputMode="numeric"
                value={remotePort}
                disabled={disabled || saving}
                onChange={(e) => setRemotePort(e.target.value.replace(/\D/g, ""))}
                onBlur={() => void savePorts()}
                onKeyDown={(e) => {
                  if (e.key === "Enter") void savePorts();
                }}
              />
            </label>
            {host.route === "proxy" ? (
              <label className="field" style={{ flex: "1 1 150px" }}>
                <span>本机代理端口</span>
                <input
                  className="input mono"
                  inputMode="numeric"
                  placeholder={overview.detectedProxyPort ? `${overview.detectedProxyPort}（自动探测）` : "7890"}
                  value={proxyPort}
                  disabled={disabled || saving}
                  onChange={(e) => setProxyPort(e.target.value.replace(/\D/g, ""))}
                  onBlur={() => void savePorts()}
                  onKeyDown={(e) => {
                    if (e.key === "Enter") void savePorts();
                  }}
                />
              </label>
            ) : null}
          </div>
          <p className="sect-none" style={{ marginTop: 6 }}>
            {host.route === "gateway"
              ? "远程端口改了要重新安装一次；避开远程上已被占用的端口。"
              : effectiveLocal
                ? `远程经 127.0.0.1:${host.remotePort} 出网，本机这头接到 127.0.0.1:${effectiveLocal}${host.proxyPort ? "" : "（自动探测）"}。改端口后重开隧道生效。`
                : "没探测到本机的代理端口：先把代理开起来，或在这里手填。"}
          </p>
        </div>
      ) : null}
    </div>
  );
}

/** 探针结果：一句结论 + 下一步。失败时把断点写在脸上。 */
function ProbeBanner({ report, route }: { report: ProbeReport; route: RemoteRoute }) {
  const v = probeVerdict(report, route);
  return <Banner tone={v.tone} title={v.title} hint={v.hint} />;
}

/**
 * 网关是隧道那一头的接收方，两种状态会让远程装好了也用不了：没在跑；或 client-type 不是 sand
 * （网关转发时会把身份改写成自己的设置，远程的推理就记到普通额度上）。这里给就近入口。
 */
export function GatewayGuard({
  overview,
  busy,
  onStart,
  onUseSand,
}: {
  overview: RemoteOverview;
  busy: boolean;
  onStart: () => void;
  onUseSand: () => void;
}) {
  if (!overview.gatewayRunning) {
    return (
      <div style={{ marginBottom: 12 }}>
        <Banner
          tone="warn"
          title="本机网关没在跑，远程装好了也用不了。"
          hint="远程的推理经隧道回到本机网关；网关不开，隧道通了也没人接。"
          action={
            <button type="button" className="btn btn-sm btn-primary" disabled={busy} onClick={onStart}>
              {busy ? <Spinner /> : null}
              开启网关
            </button>
          }
        />
      </div>
    );
  }
  if (overview.gatewayClientType !== "sand") {
    return (
      <div style={{ marginBottom: 12 }}>
        <Banner
          tone="warn"
          title={`网关的 client-type 是 ${overview.gatewayClientType}，远程走不到 Sand 通道。`}
          hint="改成 sand 后，远程的推理才记到 Sand 额度上。"
          action={
            <button type="button" className="btn btn-sm btn-primary" disabled={busy} onClick={onUseSand}>
              {busy ? <Spinner /> : null}
              改成 sand
            </button>
          }
        />
      </div>
    );
  }
  return null;
}

function RemoteOutcomeBanner({ outcome }: { outcome: RemoteOutcome }) {
  const verb = outcome.operation === "install" ? "安装" : "卸载";
  if (!outcome.wrote) {
    return (
      <div style={{ marginBottom: 12 }}>
        <Banner tone="default" title={`${outcome.host}：无需${verb}，已经是目标状态，远程没有被改动。`} />
      </div>
    );
  }
  return (
    <div style={{ marginBottom: 12 }}>
      <Banner
        tone="ok"
        title={`${outcome.host}：${verb}完成，改动了 ${outcome.filesWritten} 个文件。`}
        hint={
          (outcome.serverRestarted ? "远程 cursor-server 已重启。" : "没能重启远程 server（可能本来就没在跑）。") +
          " 回到 Cursor 对这台远程执行 Reload Window 即生效。" +
          (outcome.operation === "install" ? " 原始文件已在远程备份。" : "")
        }
      />
    </div>
  );
}

function AddHostModal({ onClose, onAdded }: { onClose: () => void; onAdded: () => Promise<void> }) {
  const [host, setHost] = useState("");
  const [label, setLabel] = useState("");
  const [route, setRoute] = useState<RemoteRoute>("gateway");
  const [probing, setProbing] = useState(false);
  const [error, setError] = useState<unknown>(null);
  const [probe, setProbe] = useState<RemoteStatus | null>(null);

  async function submit() {
    setProbing(true);
    setError(null);
    setProbe(null);
    try {
      const st = await sandRemote.addHost({
        host: host.trim(),
        label: label.trim(),
        route,
        remotePort: DEFAULT_REMOTE_PORT,
        proxyPort: null,
      });
      setProbe(st);
      await onAdded();
    } catch (e) {
      setError(e);
    } finally {
      setProbing(false);
    }
  }

  return (
    <Modal title="添加远程主机" onClose={probing ? () => {} : onClose} compact>
      <div className="form">
        <label className="field">
          <span>主机</span>
          <input
            className="input mono"
            placeholder="devbox-01 或 me@10.0.0.8"
            value={host}
            disabled={probing}
            onChange={(e) => setHost(e.target.value)}
            onKeyDown={(e) => {
              if (e.key === "Enter" && host.trim()) void submit();
            }}
            autoFocus
          />
        </label>
        <label className="field">
          <span>显示名（可选）</span>
          <input className="input" placeholder="公司工作站" value={label} disabled={probing} onChange={(e) => setLabel(e.target.value)} />
        </label>
        {/* 出网方式在添加时就问：它决定装补丁时写不写端点改道，装完再改要重装一次。 */}
        <div className="opts is-flush">
          {ROUTES.map((r) => (
            <Opt key={r.id} icon={r.icon} title={r.label} desc={r.desc} tone={route === r.id ? "on" : undefined}>
              <button
                type="button"
                className={route === r.id ? "btn btn-sm btn-primary" : "btn btn-sm"}
                disabled={probing}
                onClick={() => setRoute(r.id)}
              >
                {route === r.id ? "已选" : "选它"}
              </button>
            </Opt>
          ))}
        </div>
      </div>

      <ErrorNote error={error} />
      {probe && probe.selected ? (
        <div style={{ marginTop: 10 }}>
          <Banner tone="ok" title={`已连上，找到 server ${probe.selected.version}（${probe.servers.length} 份）。`} />
        </div>
      ) : null}

      <div className="row" style={{ marginTop: 14, justifyContent: "flex-end" }}>
        <button type="button" className="btn" disabled={probing} onClick={onClose}>
          取消
        </button>
        <button type="button" className="btn btn-primary" disabled={probing || !host.trim()} onClick={() => void submit()}>
          {probing ? <Spinner /> : null}
          {probing ? "正在连接…" : "添加"}
        </button>
      </div>
    </Modal>
  );
}
