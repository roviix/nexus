/**
 * 接入 —— 把客户端接到本地网关上，并决定它走哪条通道、哪个模型。
 *
 * 先挑客户端，再给它配路由。客户端配置只写一次：地址是网关上它自己的口（`/client/claude`），
 * 模型按这里配的路由走——之后换通道、换模型点一下就生效，不用重写配置、不用重启客户端。
 * Claude Code 的四档（Sonnet / Opus / Haiku / Fable）可以分开配，比如后台标题、摘要走的 Haiku
 * 那档换成便宜的模型；档与档之间可以跨通道。
 *
 * 顶上一排客户端卡同时是状态总览：谁接好了、谁指着别处、谁的口令 / 端口已经和网关对不上。
 * 「其他客户端」给 Cline、SDK 这类没有配置文件的：地址、口令、模型名手抄。
 *
 * 钥匙不常驻在界面上：配置里先摆占位，点「显示」或「复制」才去取（取过记活动日志，
 * 和账号凭证同一套规矩）；一键接入时钥匙根本不经过前端。
 */
import { useCallback, useEffect, useMemo, useRef, useState, type ReactNode } from "react";
import { channelOfModel, defaultChannelId, isChannelId, localChannels, modelsOf, splitModelId, type LocalChannel, type LocalChannelId } from "../gateway/channels";
import { connect as connectApi, errorText, gateway as gatewayApi, meshGetStatus, crsr as crsrApi, accounts as accountsApi, perms as permsApi, type ConnectTool } from "../ipc/api";
import type { LocalModel } from "../ipc/models";
import type { Account, ClientRoute, ClientState, ConnectResult, ConnectTest, CrsrStatus, GatewayStatus, MeshPeer, MeshStatus, PermItem } from "../ipc/types";
import { planLabel, planTone } from "../ui/usage";
import { ChannelSelect } from "../relay/ChannelPicker";
import { Highlight } from "../relay/Highlight";
import { ModelPicker, type PickerOption } from "../relay/ModelPicker";
import {
  claudeSettings,
  clientEndpoint,
  clientModelId,
  clineFields,
  codexToml,
  endpointOf,
  grokToml,
  KEY_PLACEHOLDER,
  LANG_LABEL,
  opencodeJson,
  PROTOCOL_INFO,
  protocolBase,
  sdkSnippet,
  shellLabel,
  type Endpoint,
  type Lang,
  type Protocol,
} from "../relay/snippets";
import { useRelay } from "../relay/useRelay";
import { VendorLogo } from "../relay/VendorLogo";
import { CLIENT_IDS, go, type ClientId, type Route } from "../shell/nav";
import { ShellIcon } from "../shell/ShellIcon";
import { confirm } from "../ui/confirm";
import { homePath } from "../ui/platform";
import { Banner, Icon, Spinner } from "../ui/primitives";

type Pane = ClientId | "other";

interface ClientMeta {
  id: ClientId;
  label: string;
  /** 它对网关讲哪种方言。 */
  dialect: string;
  file: string;
  vendor?: "anthropic" | "openai" | "xai";
  glyph: string;
}

const CLIENTS: Record<ClientId, ClientMeta> = {
  cursor: { id: "cursor", label: "Cursor IDE", dialect: "Connect-RPC", file: "cursor-agent-host/dist/main.js", glyph: "⌘" },
  claude: { id: "claude", label: "Claude Code", dialect: "Anthropic", file: ".claude/settings.json", vendor: "anthropic", glyph: "C" },
  codex: { id: "codex", label: "Codex CLI", dialect: "Responses", file: ".codex/config.toml", vendor: "openai", glyph: "≥" },
  opencode: { id: "opencode", label: "OpenCode", dialect: "OpenAI", file: ".config/opencode/opencode.json", glyph: "○" },
  grok: { id: "grok", label: "Grok CLI", dialect: "Responses", file: ".grok/config.toml", vendor: "xai", glyph: "G" },
};

/**
 * 每个客户端第一次配时的默认模型：Claude Code 优先 Claude 旗舰，Codex 系优先 GPT。
 * 按名字（去掉通道前缀）在所选通道的目录里找，找不到就取目录第一个。
 */
const PREFERRED: Record<ClientId, string[]> = {
  cursor: ["claude-sonnet-4-6", "claude-3-7-sonnet", "claude-sonnet-5", "claude-opus-5", "gpt-4o", "deepseek-r1"],
  claude: [
    "claude-sonnet-4-6-thinking",
    "claude-sonnet-4-6",
    "claude-sonnet-5",
    "claude-3-7-sonnet",
    "gemini-3.8-pro-high",
    "gemini-4-argon",
    "claude-opus-4-6-thinking",
    "claude-opus-5-5",
    "claude-opus-5",
    "claude-haiku-4-5",
  ],
  codex: ["gpt-6-astra", "gpt-6-sol", "gpt-6-luna", "gpt-5.6-sol", "gpt-5.6-terra", "gpt-5.6"],
  opencode: ["gpt-6-astra", "claude-sonnet-5", "gpt-5.6-sol", "gemini-4-argon", "gemini-3.8-flash-high", "grok-4.7"],
  grok: ["grok-4.7", "grok-4.6", "grok-4.5", "grok-4-fast"],
};

/** 第一次配时先看哪条通道：Codex 天然配 ChatGPT 的号，Grok CLI 配 Grok 的号；有号才轮得到它。 */
const HOME_CHANNEL: Partial<Record<ClientId, LocalChannelId>> = {
  cursor: "cursor",
  codex: "chatgpt",
  grok: "grok",
};

function pickDefault(client: ClientId, ids: string[], channels?: LocalChannel[]): string {
  const hit = PREFERRED[client].map((want) => ids.find((id) => splitModelId(id, channels).name === want)).find(Boolean);
  return hit ?? ids[0] ?? "";
}

/** 按名字给 Claude Code 另外三档挑个像样的：同一条通道里名字带 opus / haiku、flash、mini… 的。 */
function suggestRoles(ids: string[], main: string, channels?: LocalChannel[]): Pick<ClientRoute, "opus" | "haiku" | "fable"> {
  const find = (words: string[]) => ids.find((id) => id !== main && words.some((w) => splitModelId(id, channels).name.toLowerCase().includes(w))) ?? null;
  // 反重力等通道防 429 核心：后台辅助高频任务 (Haiku) 必须错开主力模型，绑定独立配额的轻量模型 (Flash-Low / Flash-Lite / Mini)
  const fastCandidate = ids.find((id) => {
    if (id === main) return false;
    const n = splitModelId(id, channels).name.toLowerCase();
    return (n.includes("flash-low") || n.includes("flash-lite") || n.includes("lite") || n.includes("haiku") || n.includes("mini") || n.includes("2.5-flash")) && !n.includes("thinking") && !n.includes("pro") && !n.includes("flash-high");
  }) ?? ids.find((id) => {
    if (id === main) return false;
    const n = splitModelId(id, channels).name.toLowerCase();
    return (n.includes("flash") || n.includes("lite") || n.includes("haiku") || n.includes("mini") || n.includes("air")) && !n.includes("thinking") && !n.includes("pro");
  });
  return {
    opus: find(["opus", "max", "pro", "thinking"]),
    haiku: fastCandidate ?? find(["haiku", "flash", "mini", "lite", "air", "fast"]),
    fable: find(["fable"]),
  };
}

/** 写进路由的模型一律带通道前缀；用户在框里手输了一个裸名，就归到当前选的通道。 */
function qualify(model: string, channel: LocalChannelId, channels?: LocalChannel[]): string {
  const m = model.trim();
  if (!m) return "";
  return splitModelId(m, channels).channel ? m : `${channel}/${m}`;
}

function cleanRoute(r: ClientRoute): ClientRoute {
  const opt = (v?: string | null) => (v?.trim() ? v.trim() : null);
  return { model: r.model.trim(), opus: opt(r.opus), haiku: opt(r.haiku), fable: opt(r.fable), context1m: Boolean(r.context1m) };
}

function sameRoute(a: ClientRoute | null | undefined, b: ClientRoute | null | undefined): boolean {
  if (!a || !b) return false;
  const x = cleanRoute(a);
  const y = cleanRoute(b);
  return x.model === y.model && x.opus === y.opus && x.haiku === y.haiku && x.fable === y.fable && x.context1m === y.context1m;
}

type Health = "live" | "stale" | "legacy" | "other" | "none";

/** 一个客户端此刻的样子。「接好了」要四件事同时成立：指本机、指自己的口、口令对、端口对。 */
function healthOf(s: ClientState | undefined): Health {
  if (!s || s.pointsTo === "none") return "none";
  if (s.pointsTo === "other") return "other";
  if (!s.scoped) return "legacy";
  if (s.keyOk === false || s.portOk === false) return "stale";
  return "live";
}

const HEALTH_TEXT: Record<Health, { label: string; tone: "ok" | "warn" | "bad" | "flat" }> = {
  live: { label: "已接入", tone: "ok" },
  stale: { label: "需要修复", tone: "bad" },
  legacy: { label: "旧版接入", tone: "warn" },
  other: { label: "指向别处", tone: "flat" },
  none: { label: "未配置", tone: "flat" },
};

function hostOf(url: string | null | undefined): string {
  if (!url) return "";
  return url.replace(/^https?:\/\//, "").split("/")[0] ?? url;
}

export function ConnectPage({ route, onGo }: { route: Route; onGo: (r: Route) => void }) {
  const relay = useRelay({ catalogs: true });
  const [meshStatus, setMeshStatus] = useState<MeshStatus | null>(null);
  const [states, setStates] = useState<ClientState[] | null>(null);
  const [stateError, setStateError] = useState<string | null>(null);
  const [pane, setPane] = useState<Pane>(route.client ?? "claude");
  const [starting, setStarting] = useState(false);

  useEffect(() => {
    if (route.client) setPane(route.client);
  }, [route.client]);

  useEffect(() => {
    void meshGetStatus().then(setMeshStatus).catch(() => {});
    const timer = setInterval(() => {
      void meshGetStatus().then(setMeshStatus).catch(() => {});
    }, 5000);
    return () => clearInterval(timer);
  }, []);

  const peers: MeshPeer[] = useMemo(() => meshStatus?.peers ?? [], [meshStatus?.peers]);

  const [cursorStatus, setCursorStatus] = useState<CrsrStatus | null>(null);

  const loadStates = useCallback(async () => {
    try {
      const [cStates, cStatus] = await Promise.all([
        connectApi.inspectAll(),
        crsrApi.status().catch(() => null),
      ]);
      setStates(cStates);
      setCursorStatus(cStatus);
      setStateError(null);
    } catch (e) {
      setStateError(errorText(e));
    }
  }, []);

  useEffect(() => {
    void loadStates();
  }, [loadStates]);

  const reloadAll = useCallback(async () => {
    await Promise.all([relay.reload(), loadStates()]);
  }, [relay, loadStates]);

  const byTool = useMemo(() => new Map((states ?? []).map((s) => [s.tool as ClientId, s])), [states]);
  const channels = useMemo(
    () => localChannels(relay.gateway, relay.local, peers),
    [relay.gateway, relay.local, peers],
  );
  const running = Boolean(relay.gateway?.running);

  async function startGateway() {
    setStarting(true);
    try {
      await gatewayApi.start();
      await reloadAll();
    } catch (e) {
      setStateError(errorText(e));
    } finally {
      setStarting(false);
    }
  }

  const choose = (p: Pane) => {
    setPane(p);
    onGo(go("connect", p === "other" ? {} : { client: p }));
  };

  return (
    <div className="connect">
      {/* 极简一级标题栏 */}
      <div className="page-head">
        <div className="page-title-line">
          <h1>接入</h1>
        </div>
        <div className="row" style={{ gap: 8 }}>
          <button
            type="button"
            className="btn btn-sm btn-icon btn-soft"
            onClick={() => void reloadAll()}
            disabled={relay.loading}
            title="刷新状态"
            aria-label="刷新"
          >
            <Icon name="refresh" size={14} className={relay.loading ? "is-spinning" : undefined} />
          </button>
        </div>
      </div>

      {relay.gateway && !running ? (
        <Banner
          title="本地网关未开启"
          hint="客户端的请求需经过本地网关 (127.0.0.1)。一键接入时会自动开启网关，并设为随应用启动。"
          action={
            <button type="button" className="btn btn-sm btn-primary" disabled={starting} onClick={() => void startGateway()}>
              {starting ? <Spinner /> : null}
              立即开启本地网关
            </button>
          }
        />
      ) : null}
      {stateError ? <Banner tone="bad" title="读不出客户端配置" hint={stateError} /> : null}

      {/* 客户端卡片区（包含各客户端与通用 SDK，稳定排列、点击不乱跳） */}
      <section className="connect-section" style={{ marginBottom: 16 }}>
        <div className="ctiles">
          {CLIENT_IDS.map((id) => (
            <ClientTile
              key={id}
              meta={CLIENTS[id]}
              state={byTool.get(id)}
              saved={relay.gateway?.routes?.[id] ?? null}
              loading={states == null}
              active={pane === id}
              cursorStatus={cursorStatus}
              onPick={() => choose(id)}
            />
          ))}
          <button
            type="button"
            className={`card ctile${pane === "other" ? " is-active" : ""}`}
            aria-pressed={pane === "other"}
            onClick={() => choose("other")}
          >
            <span className="ctile-top">
              <span className="ctile-brand">
                <span className="toolcard-glyph">
                  <span className="mono">{"{}"}</span>
                </span>
                <span className="ctile-name">其他客户端 / SDK</span>
              </span>
              <span className="ctile-status-badge is-flat">
                <span className="ctile-dot" aria-hidden />
                <span className="ctile-status-text">通用协议</span>
              </span>
            </span>
            <span className="ctile-line truncate" title="Cline、cURL、OpenAI / Anthropic SDK">
              Cline、cURL、OpenAI / Anthropic SDK
            </span>
          </button>
        </div>
      </section>

      {/* 紧密咬合的详情配置面板 (Master-Detail 联动整体) */}
      {pane === "other" ? (
        <OtherClients gateway={relay.gateway} local={relay.local} channels={channels} peers={peers} onGo={onGo} />
      ) : pane === "cursor" ? (
        <CursorClientPanel
          status={cursorStatus}
          gateway={relay.gateway}
          local={relay.local}
          channels={channels}
          peers={peers}
          savedRoute={relay.gateway?.routes?.cursor ?? null}
          onChanged={reloadAll}
          onGo={onGo}
        />
      ) : !relay.gateway || states == null ? (
        <div className="skeleton" style={{ height: 320 }} />
      ) : (
        <ClientPanel
          key={pane}
          meta={CLIENTS[pane]}
          state={byTool.get(pane)}
          gateway={relay.gateway}
          local={relay.local}
          channels={channels}
          peers={peers}
          preset={route.client === pane || !route.client ? { channel: route.channel, model: route.model } : {}}
          onChanged={reloadAll}
          onGo={onGo}
        />
      )}
    </div>
  );
}

/* ── 客户端卡 ─────────────────────────────────────────────────────────────── */

function ClientTile({
  meta,
  state,
  saved,
  loading,
  active,
  cursorStatus,
  onPick,
}: {
  meta: ClientMeta;
  state: ClientState | undefined;
  saved: ClientRoute | null;
  loading: boolean;
  active: boolean;
  cursorStatus?: CrsrStatus | null;
  onPick: () => void;
}) {
  if (meta.id === "cursor") {
    const isLive = Boolean(cursorStatus?.installed && cursorStatus?.complete);
    const isMock = cursorStatus?.credential?.gatewayUrl?.includes("/mock");
    const h = isLive
      ? { tone: "ok" as const, label: "已接入" }
      : cursorStatus?.sandConflict
        ? { tone: "warn" as const, label: "有冲突" }
        : { tone: "flat" as const, label: "未接管" };

    const line = loading
      ? "读取中…"
      : isLive
        ? isMock
          ? (saved?.model ? splitModelId(saved.model).name : "自选异构模型")
          : "Cursor 原生号池"
        : "官方直连";

    return (
      <button type="button" className={`card ctile${active ? " card-hot is-active" : ""}`} aria-pressed={active} onClick={onPick}>
        <span className="ctile-top">
          <span className="ctile-brand">
            <span className="toolcard-glyph">
              <span className="mono">{meta.glyph}</span>
            </span>
            <span className="ctile-name">{meta.label}</span>
          </span>
          <span className={`ctile-status-badge is-${h.tone}`}>
            <span className={`ctile-dot is-${h.tone}`} aria-hidden />
            <span className="ctile-status-text">{loading ? "…" : h.label}</span>
          </span>
        </span>
        <span className="ctile-line mono truncate" title={line}>
          {line}
        </span>
      </button>
    );
  }

  const health = healthOf(state);
  const h = HEALTH_TEXT[health];
  const line = loading
    ? "读取中…"
    : health === "live" || health === "stale"
      ? (saved?.model ?? state?.model ?? "")
      : health === "legacy"
        ? `全局默认通道 · ${state?.model ?? ""}`
        : health === "other"
          ? hostOf(state?.baseUrl)
          : "还没接";
  return (
    <button type="button" className={`card ctile${active ? " card-hot is-active" : ""}`} aria-pressed={active} onClick={onPick}>
      <span className="ctile-top">
        <span className="ctile-brand">
          <span className={`toolcard-glyph${meta.vendor ? ` is-${meta.vendor}` : ""}`}>
            {meta.vendor ? <VendorLogo vendor={meta.vendor} size={16} mono={!active} /> : <span className="mono">{meta.glyph}</span>}
          </span>
          <span className="ctile-name">{meta.label}</span>
        </span>
        <span className={`ctile-status-badge is-${h.tone}`}>
          <span className={`ctile-dot is-${h.tone}`} aria-hidden />
          <span className="ctile-status-text">{loading ? "…" : h.label}</span>
          {state?.env.length ? <span className="ctile-env" title="有环境变量可能架空这份配置">!</span> : null}
        </span>
      </span>
      <span className="ctile-line mono truncate" title={line}>
        {line}
      </span>
    </button>
  );
}

/* ── 一个客户端的路由与接入 ───────────────────────────────────────────────── */

function channelModels(local: LocalModel[] | null, channels: LocalChannel[], id: LocalChannelId): string[] {
  const ch = channels.find((c) => c.id.toLowerCase() === id.toLowerCase());
  if (ch && ch.vendor === "nexus-mesh") {
    return modelsOf(ch);
  }
  return (local ?? []).filter((m) => (m.modality ?? "chat") === "chat" && channelOfModel(channels, m.id) === id).map((m) => m.id);
}

function initialRoute(
  client: ClientId,
  saved: ClientRoute | null,
  preset: { channel?: string; model?: string },
  gateway: GatewayStatus,
  local: LocalModel[] | null,
  channels: LocalChannel[],
): { route: ClientRoute; channel: LocalChannelId } {
  // 排除不可做网关标准推理的通道（如 Cursor）
  const validChannels = channels.filter((c) => !c.unavailableReason && c.id !== "cursor");
  const fallback = defaultChannelId(gateway, validChannels) || validChannels[0]?.id || "claude";

  if (preset.model) {
    const parsed = splitModelId(preset.model, channels).channel;
    const ch = (parsed && validChannels.some((c) => c.id === parsed)) ? parsed : (isChannelId(preset.channel, validChannels) ? preset.channel : fallback);
    return { route: { ...(saved ?? { model: "" }), model: qualify(preset.model, ch, channels) }, channel: ch };
  }
  if (saved?.model) {
    const parsed = splitModelId(saved.model, channels).channel;
    const ch = (parsed && validChannels.some((c) => c.id === parsed)) ? parsed : fallback;
    if (!isChannelId(preset.channel, validChannels) || preset.channel === ch) return { route: saved, channel: ch };
  }
  // 没配过：优先地址里带来的通道，其次这个客户端的「本家」通道（有号时），再其次默认通道、第一条有模型的可用通道
  const home = HOME_CHANNEL[client];
  const order: LocalChannelId[] = [
    ...(isChannelId(preset.channel, validChannels) ? [preset.channel] : []),
    ...(home && validChannels.find((c) => c.id === home)?.ready ? [home] : []),
    ...(home && validChannels.some((c) => c.id === home) ? [home] : []),
    fallback,
    ...validChannels.filter((c) => c.ready).map((c) => c.id),
    ...validChannels.map((c) => c.id),
    "claude",
  ];
  const ch = order.find((id) => channelModels(local, channels, id).length > 0) ?? order[0] ?? fallback;
  const inCh = channelModels(local, channels, ch);
  const defModel = pickDefault(client, inCh, channels);
  const roles = client === "claude" ? suggestRoles(inCh, defModel, channels) : {};
  return {
    route: {
      model: defModel,
      ...roles,
    },
    channel: ch,
  };
}

function ClientPanel({
  meta,
  state,
  gateway,
  local,
  channels,
  peers,
  preset,
  onChanged,
  onGo: _onGo,
}: {
  meta: ClientMeta;
  state: ClientState | undefined;
  gateway: GatewayStatus;
  local: LocalModel[] | null;
  channels: LocalChannel[];
  peers?: MeshPeer[];
  preset: { channel?: string; model?: string };
  onChanged: () => Promise<void>;
  onGo: (r: Route) => void;
}) {
  const client = meta.id;
  const saved = gateway.routes?.[client] ?? null;
  // 只在打开这个客户端时算一次：之后是用户手里的草稿，别被后台刷新的状态冲掉。
  const [init] = useState(() => initialRoute(client, saved, preset, gateway, local, channels));
  const [draft, setDraft] = useState<ClientRoute>(init.route);
  const [channel, setChannel] = useState<LocalChannelId>(init.channel);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [done, setDone] = useState<{ kind: "applied"; r: ConnectResult; update: boolean } | { kind: "reverted" } | null>(null);
  const [test, setTest] = useState<ConnectTest | null>(null);
  const [testing, setTesting] = useState(false);

  // 目录晚到（第一次渲染时还没拉回来）：主模型还空着就补一个默认的。
  useEffect(() => {
    if (draft.model) return;
    const ids = channelModels(local, channels, channel);
    if (ids.length) setDraft((d) => ({ ...d, model: pickDefault(client, ids) }));
  }, [local, channels, channel, client, draft.model]);

  const health = healthOf(state);
  const live = health === "live";
  const dirty = !sameRoute(draft, saved);
  const inChannel = useMemo(() => channelModels(local, channels, channel), [local, channels, channel]);
  const channelLabel = (id: string | null) => channels.find((c) => c.id === id)?.label ?? id ?? "";
  const blockedRouteFields = [
    [client === "claude" ? "主模型" : "模型", draft.model],
    ["Opus", draft.opus],
    ["Haiku", draft.haiku],
    ["Fable", draft.fable],
  ].flatMap(([label, value]) => {
    if (!value) return [];
    const target = splitModelId(value).channel ?? channel;
    return channels.find((c) => c.id === target)?.unavailableReason ? [label] : [];
  });
  const blockedRouteReason = blockedRouteFields.length
    ? channels.find((c) => c.unavailableReason && (c.id === channel || [draft.model, draft.opus, draft.haiku, draft.fable].some((v) => v && splitModelId(v).channel === c.id)))?.unavailableReason ?? "这条路由包含不可用通道。"
    : null;

  const options: PickerOption[] = useMemo(() => inChannel.map((id) => ({ id })), [inChannel]);
  /** Claude 的分档可以跨通道：全部对话模型，右边标上通道。 */
  const allOptions: PickerOption[] = useMemo(
    () =>
      (local ?? [])
        .filter((m) => (m.modality ?? "chat") === "chat")
        .map((m) => {
          const id = channelOfModel(channels, m.id);
          return { id: m.id, meta: channels.find((c) => c.id === id)?.label ?? id };
        }),
    [local, channels],
  );

  function pickChannel(id: string | null) {
    const next = isChannelId(id, channels) ? id : defaultChannelId(gateway, channels);
    setChannel(next);
    setDone(null);
    const ids = channelModels(local, channels, next);
    const defModel = ids.includes(draft.model) ? draft.model : pickDefault(client, ids, channels);
    if (client === "claude") {
      const suggested = suggestRoles(ids, defModel, channels);
      setDraft((d) => ({
        ...d,
        model: defModel,
        opus: suggested.opus,
        haiku: suggested.haiku,
        fable: suggested.fable,
      }));
    } else {
      setDraft((d) => (ids.includes(d.model) ? d : { ...d, model: defModel }));
    }
  }

  const routeToSave = (): ClientRoute => {
    const r = cleanRoute({ ...draft, model: qualify(draft.model, channel, channels) });
    const q = (v: string | null | undefined) => (v ? qualify(v, channel, channels) : null);
    return { ...r, opus: q(r.opus), haiku: q(r.haiku), fable: q(r.fable) };
  };

  async function apply() {
    const next = routeToSave();
    if (blockedRouteReason) {
      setError(`不能保存：${blockedRouteFields.join("、")} 仍指向 Cursor。${blockedRouteReason}`);
      return;
    }
    if (!next.model) {
      setError("先选一个模型。");
      return;
    }
    setBusy(true);
    setError(null);
    setDone(null);
    setTest(null);
    try {
      if (!gateway.running) {
        try {
          await gatewayApi.start();
        } catch {}
      }
      const r = await connectApi.apply(client as ConnectTool, next);
      setDone({ kind: "applied", r, update: live });
      setDraft(next);
      await onChanged();
    } catch (e) {
      setError(errorText(e));
    } finally {
      setBusy(false);
    }
  }

  async function revert() {
    const ok = await confirm(`${meta.label} 的配置会还原成接入之前的样子；是我们建的文件就删掉。之后它不再经过本地网关。`, {
      title: `撤销 ${meta.label} 的接入？`,
      okLabel: "撤销",
      danger: true,
    });
    if (!ok) return;
    setBusy(true);
    setError(null);
    try {
      await connectApi.revert(client as ConnectTool);
      setDone({ kind: "reverted" });
      setTest(null);
      await onChanged();
    } catch (e) {
      setError(errorText(e));
    } finally {
      setBusy(false);
    }
  }

  async function runTest() {
    setTesting(true);
    setTest(null);
    try {
      setTest(await connectApi.test(client as ConnectTool));
    } catch (e) {
      setTest({ ok: false, text: "", requested: "", target: null, channel: null, account: null, durationMs: 0, error: errorText(e) });
    } finally {
      setTesting(false);
    }
  }

  const primary =
    health === "stale"
      ? "修复接入"
      : health === "legacy"
        ? "升级接入"
        : live
          ? (dirty ? "保存并即时生效" : null)
          : health === "other"
            ? "改接到 Nexus"
            : "一键接入";

  const gatewayPort = gateway.running ? Number(gateway.running.addr.split(":").pop()) : gateway.settings.port;
  const statusHint = useMemo(() => {
    if (error) return { tone: "bad", text: error, full: error };
    if (done?.kind === "applied") {
      return {
        tone: "ok",
        text: done.update ? "路由已更新，即时生效" : "配置已写入，重开生效",
        full: done.update ? "路由已更新，下一条请求就按新的走，不用重启。" : "已写入配置，重开客户端即可生效。",
      };
    }
    if (done?.kind === "reverted") {
      return { tone: "flat", text: "已还原配置", full: "已撤销接入，客户端不再走本地网关" };
    }
    if (health === "stale") {
      if (state?.portOk === false) {
        return {
          tone: "bad",
          text: `配置端口与网关 (${gatewayPort}) 不符，请点击「修复接入」`,
          full: `配置里的端口和网关现在的（${gatewayPort}）对不上，客户端连不上。点「修复接入」重写一遍。`,
        };
      }
      if (state?.keyOk === false) {
        return {
          tone: "bad",
          text: "网关口令已失效，请点击「修复接入」",
          full: "配置里的口令和网关现在的不一样，请求会 401。点「修复接入」重写一遍。",
        };
      }
      return { tone: "bad", text: "配置已过时，请点击「修复接入」", full: "配置已过时，点「修复接入」重新同步。" };
    }
    if (health === "legacy") {
      return {
        tone: "warn",
        text: `旧版全局路由 (${channelLabel(defaultChannelId(gateway, channels))})，建议「升级接入」`,
        full: "这是旧版接入：请求走全局默认通道，不看这里的路由。点「升级接入」改成按客户端路由。",
      };
    }
    if (health === "other") {
      return {
        tone: "flat",
        text: `当前指向 ${hostOf(state?.baseUrl)}`,
        full: `现在指向 ${hostOf(state?.baseUrl)}。接入会先把原文件备份，撤销能还原。`,
      };
    }
    return null;
  }, [error, done, health, state?.portOk, state?.keyOk, state?.baseUrl, gatewayPort, gateway, channels]);

  return (
    <section className="connect-section">
      <div className="card card-flush cfg">
        <div className="cfg-head">
          <div className="cfg-identity">
            <strong className="cfg-client">{meta.label}</strong>
            <span className="pill cfg-protocol">{meta.dialect}</span>
            <span className={`ctile-status is-inline`}>
              <span className={`ctile-dot is-${HEALTH_TEXT[health].tone}`} aria-hidden />
              {HEALTH_TEXT[health].label}
            </span>
            {statusHint ? (
              <span
                className={`cfg-inline-status-hint is-${statusHint.tone} truncate`}
                title={statusHint.full}
              >
                {statusHint.text}
              </span>
            ) : null}
          </div>
          <div className="cfg-acts">
            {dirty && live ? (
              <span className="cfg-dirty-badge">
                <span className="cfg-dirty-dot" />
                未保存变更
              </span>
            ) : null}
            {state?.revertible || live ? (
              <button type="button" className="btn btn-sm btn-quiet" disabled={busy} onClick={() => void revert()} data-tip="按接入前的备份还原；是我们建的文件就删掉">
                还原
              </button>
            ) : null}
            {live ? (
              <button type="button" className="btn btn-sm" disabled={testing || busy} onClick={() => void runTest()}>
                {testing ? <Spinner /> : <ShellIcon name="play" size={12} />}
                测一下
              </button>
            ) : null}
            {primary ? (
              <button
                type="button"
                className={`btn btn-sm ${dirty || !live ? "btn-primary cfg-save-btn" : "btn-soft"}`}
                disabled={busy || !draft.model.trim() || Boolean(blockedRouteReason)}
                onClick={() => void apply()}
              >
                {busy ? <Spinner /> : <Icon name="check" size={13} />}
                {primary}
              </button>
            ) : (
              <span className="cfg-synced-pill">
                <Icon name="check" size={12} />
                已同步
              </span>
            )}
          </div>
        </div>

        {test ? (
          <div className={`cfg-probe-banner is-${test.ok ? "ok" : "err"}`}>
            <span className={`pod-dot${test.ok ? " is-live" : ""}`} />
            {test.ok ? (
              <span>
                ✓ 链路畅通 · 落点通道: [<b>{test.channel || channel}</b>] → 命中账号: <b>{test.account || "默认凭证"}</b> ({test.durationMs}ms)
              </span>
            ) : (
              <span>
                ▲ 链路探测失败: {test.error || "请求未能成功通过本地网关"}
              </span>
            )}
          </div>
        ) : null}

        {blockedRouteReason ? (
          <p className="cfg-note is-bad">
            <i aria-hidden />
            <span>
              {blockedRouteFields.join("、")} 仍指向 Cursor。{blockedRouteReason}
            </span>
          </p>
        ) : null}

        {/* 宽屏双栏工作台布局 */}
        <div className="cpanel-workbench">
          {/* 左栏：路由与模型编排 */}
          <div className="cpanel-editor">
            <div className="cpanel-section-title">
              <span>路由与模型编排</span>
            </div>

            <div className="croute-v-flow">
              <div className="croute-v-item">
                <div className="croute-v-head">
                  <span className="croute-v-title">路由通道</span>
                  <span className="croute-v-desc">分配给该工具的账号池或 Mesh 节点</span>
                </div>
                <div className="croute-v-control">
                  <ChannelSelect
                    channel={channel}
                    onChange={pickChannel}
                    gateway={gateway}
                    local={local}
                    peers={peers}
                    filterUnavailable
                  />
                </div>
              </div>

              <div className="croute-v-item">
                <div className="croute-v-head">
                  <span className="croute-v-title">{client === "claude" ? "主模型" : "目标模型"}</span>
                  <span className="croute-v-desc">
                    {client === "claude"
                      ? "Sonnet 档与未单独配置的档位默认使用此模型"
                      : `请求路由至此模型 (写入 ${clientModelId(qualify(draft.model, channel)) || "…"})`}
                  </span>
                </div>
                <div className="croute-v-control">
                  <ModelPicker
                    value={draft.model}
                    options={options}
                    placeholder={
                      inChannel.length
                        ? "选一个模型，或直接输入 id"
                        : "这条通道还没有可用的模型——先去添加号"
                    }
                    onChange={(v) => {
                      setDraft((d) => ({ ...d, model: v }));
                      setDone(null);
                    }}
                  />
                </div>
              </div>

              {client === "claude" ? (
                <ClaudeRoles
                  draft={draft}
                  options={allOptions}
                  onChange={(patch) => {
                    setDraft((d) => ({ ...d, ...patch }));
                    setDone(null);
                  }}
                  onSuggest={() => setDraft((d) => ({ ...d, ...suggestRoles(inChannel, qualify(d.model, channel)) }))}
                />
              ) : null}
            </div>
          </div>

          {/* 右栏：实时写入预览与链路状态 */}
          <div className="cpanel-inspector">
            <div className="cpanel-section-title">
              <span>写入配置预览</span>
              <span className="mono faint tiny truncate" title={meta.file}>{homePath(meta.file)}</span>
            </div>

            {test ? <TestResult test={test} channelLabel={channelLabel} /> : null}

            {state?.env.length ? <EnvWarnings env={state.env} client={meta.label} /> : null}

            <ConfigLiveBlock meta={meta} gateway={gateway} route={routeToSave()} />
          </div>
        </div>
      </div>
    </section>
  );
}

/** Claude Code 另外三档：标准钛白分段控制器 (Segmented Control)。 */
function ClaudeRoles({
  draft,
  options,
  onChange,
  onSuggest,
}: {
  draft: ClientRoute;
  options: PickerOption[];
  onChange: (patch: Partial<ClientRoute>) => void;
  onSuggest: () => void;
}) {
  const custom = Boolean(draft.opus || draft.haiku || draft.fable || draft.context1m);
  const [open, setOpen] = useState(custom);

  return (
    <div className="croute-tier-section">
      <div className="croute-tier-header">
        <div className="croute-tier-label">
          <span className="croute-v-title">分档配置</span>
          <span className="croute-v-desc">为 Claude Code 的 Opus / Haiku / Fable 角色分别指定模型</span>
        </div>
        <div className="croute-seg-ctrl" role="tablist">
          <button
            type="button"
            role="tab"
            aria-selected={!open}
            className={`croute-seg-btn${!open ? " is-active" : ""}`}
            onClick={() => {
              onChange({ opus: null, haiku: null, fable: null, context1m: false });
              setOpen(false);
            }}
          >
            统一使用主模型
          </button>
          <button
            type="button"
            role="tab"
            aria-selected={open}
            className={`croute-seg-btn${open ? " is-active" : ""}`}
            onClick={() => setOpen(true)}
          >
            按四档分别配置
          </button>
        </div>
      </div>

      <div className="faint tiny" style={{ marginTop: 6, lineHeight: 1.5 }}>
        提示：上游通道对 Opus 频控较为严格。日常编码建议以 Sonnet 为主，后台高频任务可搭配 Flash / Haiku 档位。
      </div>

      {open ? (
        <div className="croute-tiers-grid">
          <div className="crole-grid-item">
            <label className="crole-label">
              <span className="crole-title">Opus 档位 (深度推理)</span>
              <span className="crole-desc">复杂架构规划与深度分析</span>
            </label>
            <ModelPicker
              value={draft.opus ?? ""}
              options={options}
              placeholder="留空则跟主模型"
              onChange={(v) => onChange({ opus: v || null })}
            />
          </div>

          <div className="crole-grid-item">
            <label className="crole-label">
              <span className="crole-title">Haiku 档位 (轻量高频)</span>
              <span className="crole-desc">后台索引与生成摘要</span>
            </label>
            <ModelPicker
              value={draft.haiku ?? ""}
              options={options}
              placeholder="留空则跟主模型"
              onChange={(v) => onChange({ haiku: v || null })}
            />
          </div>

          <div className="crole-grid-item">
            <label className="crole-label">
              <span className="crole-title">Fable 档位 (内部代理)</span>
              <span className="crole-desc">内部多 Agent 派生任务</span>
            </label>
            <ModelPicker
              value={draft.fable ?? ""}
              options={options}
              placeholder="留空则跟随 Opus"
              onChange={(v) => onChange({ fable: v || null })}
            />
          </div>

          <div className="crole-grid-item crole-extra-item">
            <label className="crole-check-label">
              <input
                type="checkbox"
                className="tick"
                checked={Boolean(draft.context1m)}
                onChange={(e) => onChange({ context1m: e.target.checked })}
              />
              <span>按 1M 上下文算 (Sonnet / Opus / Fable)</span>
            </label>
            <button
              type="button"
              className="btn btn-xs btn-quiet"
              onClick={onSuggest}
              title="按模型名字智能分派"
            >
              按名字自动分
            </button>
          </div>
        </div>
      ) : null}
    </div>
  );
}



/** 越小越该先说。换配置目录会让整份接入作废，比盖住一个模型名严重。 */
function envRank(name: string): number {
  if (
    name === "CLAUDE_CONFIG_DIR" ||
    name === "CODEX_HOME" ||
    name === "OPENCODE_CONFIG" ||
    name === "OPENCODE_CONFIG_DIR" ||
    name === "GROK_HOME"
  ) {
    return 0;
  }
  if (name === "CLAUDE_CODE_USE_BEDROCK" || name === "CLAUDE_CODE_USE_VERTEX") return 1;
  if (name === "ANTHROPIC_BASE_URL") return 2;
  if (name === "ANTHROPIC_API_KEY" || name === "ANTHROPIC_AUTH_TOKEN") return 3;
  return 4;
}

function envHeadline(name: string): string {
  switch (name) {
    case "CLAUDE_CONFIG_DIR":
    case "CODEX_HOME":
    case "OPENCODE_CONFIG":
    case "OPENCODE_CONFIG_DIR":
    case "GROK_HOME":
      return `${name} 会让写入的配置不生效`;
    case "CLAUDE_CODE_USE_BEDROCK":
      return "CLAUDE_CODE_USE_BEDROCK 会改走 Bedrock";
    case "CLAUDE_CODE_USE_VERTEX":
      return "CLAUDE_CODE_USE_VERTEX 会改走 Vertex";
    case "ANTHROPIC_BASE_URL":
      return "ANTHROPIC_BASE_URL 会改走另一个地址";
    case "ANTHROPIC_API_KEY":
    case "ANTHROPIC_AUTH_TOKEN":
      return `${name} 可能盖住口令`;
    default:
      return `${name} 可能盖住模型名`;
  }
}

function EnvWarnings({ env, client }: { env: ClientState["env"]; client: string }) {
  const [open, setOpen] = useState(false);
  const lead = [...env].sort((a, b) => envRank(a.name) - envRank(b.name) || a.name.localeCompare(b.name))[0];
  if (!lead) return null;
  const rest = env.length - 1;
  const summary = `${envHeadline(lead.name)}${rest > 0 ? `，还有 ${rest} 个` : ""}`;
  return (
    <div className="cenv">
      <button
        type="button"
        className="cenv-head"
        aria-expanded={open}
        onClick={() => setOpen((v) => !v)}
      >
        <Icon name="alert" size={12} />
        <span className="cenv-summary">
          {summary}，可能让 {client} 不照配置走
        </span>
        <Icon name="chevron" size={12} className={`opt-chev${open ? " is-open" : ""}`} />
      </button>
      {open
        ? env.map((e) => (
            <div key={`${e.name}@${e.source}`} className="cenv-row">
              <code className="mono">
                {e.name}={e.value}
              </code>
              <span className="faint tiny">{e.source}</span>
              <span className="cenv-effect">{e.effect}</span>
            </div>
          ))
        : null}
    </div>
  );
}

function TestResult({ test, channelLabel }: { test: ConnectTest; channelLabel: (id: string | null) => string }) {
  return (
    <div className={`ctest ${test.ok ? "is-ok" : "is-bad"}`}>
      <div className="ctest-head">
        <span className={`ctile-dot is-${test.ok ? "ok" : "bad"}`} aria-hidden />
        {test.ok ? "通了" : "没通"}
        {test.target ?? test.requested ? <code className="mono">{test.target ?? test.requested}</code> : null}
        {test.channel ? <span className="faint tiny">{channelLabel(test.channel)}</span> : null}
        {test.account ? <span className="faint tiny truncate">{test.account}</span> : null}
        {test.durationMs ? <span className="faint tiny">{(test.durationMs / 1000).toFixed(1)}s</span> : null}
      </div>
      {test.ok ? <p className="ctest-text selectable">{test.text || "（没有文本）"}</p> : <p className="ctest-text is-bad selectable">{test.error}</p>}
    </div>
  );
}

/* ── 配置正文预览（复制用）───────────────────────────────────────────────── */

function useRevealedKey() {
  const [key, setKey] = useState<string | undefined>(undefined);
  const [visible, setVisible] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const ensure = useCallback(async (): Promise<string | null> => {
    setError(null);
    try {
      if (key) return key;
      const k = await gatewayApi.revealKey();
      setKey(k);
      return k;
    } catch (e) {
      setError(errorText(e));
      return null;
    }
  }, [key]);
  return { key, visible, setVisible, error, ensure };
}

function useCopied() {
  const [copied, setCopied] = useState("");
  const timer = useRef<number | undefined>(undefined);
  useEffect(() => () => window.clearTimeout(timer.current), []);
  const mark = useCallback((id: string) => {
    setCopied(id);
    window.clearTimeout(timer.current);
    timer.current = window.setTimeout(() => setCopied(""), 1400);
  }, []);
  return { copied, mark };
}

function gatewayEndpoint(gateway: GatewayStatus | null): Endpoint {
  if (gateway?.running) return endpointOf(gateway.running.baseUrl);
  return endpointOf(`http://127.0.0.1:${gateway?.settings.port ?? 8787}`);
}

function ConfigLiveBlock({ meta, gateway, route }: { meta: ClientMeta; gateway: GatewayStatus; route: ClientRoute }) {
  const k = useRevealedKey();
  const { copied, mark } = useCopied();
  const ep = gatewayEndpoint(gateway);
  const build = (key: string) => {
    const written = clientModelId(route.model);
    switch (meta.id) {
      case "claude":
        return claudeSettings(ep, key, route);
      case "codex":
        return codexToml(clientEndpoint(ep, "codex"), key, written);
      case "opencode":
        return opencodeJson(clientEndpoint(ep, "opencode"), key, written);
      case "grok":
        return grokToml(clientEndpoint(ep, "grok"), key, written);
      case "cursor":
      default:
        return "";
    }
  };
  const text = build(k.visible && k.key ? k.key : KEY_PLACEHOLDER);
  return (
    <div className="cpreview-live">
      {k.error ? <span className="tiny" style={{ color: "var(--bad)" }}>{k.error}</span> : null}
      <ConfigBlock
        title={homePath(meta.file)}
        format={meta.file.endsWith(".toml") ? "TOML" : "JSON"}
        code={text}
        copied={copied === "cfg"}
        onCopy={() =>
          void k.ensure().then(async (key) => {
            if (!key) return;
            await navigator.clipboard.writeText(build(key)).catch(() => {});
            mark("cfg");
          })
        }
        extraAction={
          <button
            type="button"
            className="btn btn-sm btn-quiet"
            onClick={() => void (k.visible ? k.setVisible(false) : k.ensure().then((v) => v && k.setVisible(true)))}
            title={k.visible ? "隐藏真实口令" : "显示真实口令"}
            style={{ fontSize: 11.5, gap: 4, height: 24, padding: "0 6px" }}
          >
            <Icon name={k.visible ? "eyeOff" : "eye"} size={12} />
            {k.visible ? "隐藏口令" : "显示口令"}
          </button>
        }
      />
    </div>
  );
}

/* ── Cursor 极客双模控制面板 ────────────────────────────────────────────── */

function BaseAccountPicker({
  accounts,
  currentEmail,
  disabled,
  onSelect,
  onGoAccounts,
}: {
  accounts: Account[];
  currentEmail?: string | null;
  disabled?: boolean;
  onSelect: (accId: string) => void;
  onGoAccounts: () => void;
}) {
  const [open, setOpen] = useState(false);
  const [query, setQuery] = useState("");
  const popoverRef = useRef<HTMLDivElement>(null);

  // 1. 过滤掉已归档账号
  const activeAccounts = useMemo(() => {
    return accounts.filter((a) => !a.archivedAt);
  }, [accounts]);

  // 2. 搜索过滤（支持邮箱、档位、标签）
  const filteredAccounts = useMemo(() => {
    const q = query.trim().toLowerCase();
    if (!q) return activeAccounts;
    return activeAccounts.filter((a) => {
      const emailHit = a.email.toLowerCase().includes(q);
      const planHit = (a.usage?.plan || a.membership || "").toLowerCase().includes(q);
      const tagHit = a.tags?.some((t) => t.toLowerCase().includes(q));
      return emailHit || planHit || tagHit;
    });
  }, [activeAccounts, query]);

  // 当前绑定的账号对象
  const currentAccount = useMemo(() => {
    if (!currentEmail) return null;
    return activeAccounts.find((a) => a.email === currentEmail) ?? null;
  }, [activeAccounts, currentEmail]);

  // 点击外部和按 Esc 关闭
  useEffect(() => {
    if (!open) return;
    function handleClickOutside(e: MouseEvent) {
      if (popoverRef.current && !popoverRef.current.contains(e.target as Node)) {
        setOpen(false);
      }
    }
    function handleKeyDown(e: KeyboardEvent) {
      if (e.key === "Escape") setOpen(false);
    }
    document.addEventListener("mousedown", handleClickOutside);
    document.addEventListener("keydown", handleKeyDown);
    return () => {
      document.removeEventListener("mousedown", handleClickOutside);
      document.removeEventListener("keydown", handleKeyDown);
    };
  }, [open]);

  const currentPlan = currentAccount?.usage?.plan ?? currentAccount?.membership ?? null;

  return (
    <div className="base-acc-picker-wrap" ref={popoverRef}>
      <button
        type="button"
        className={`base-acc-trigger${currentEmail ? " is-bound" : " is-empty"}`}
        disabled={disabled}
        onClick={() => setOpen((v) => !v)}
      >
        <div className="base-acc-trigger-main">
          {currentEmail ? (
            <>
              <span className="base-acc-trigger-email mono truncate">{currentEmail}</span>
              {currentPlan ? (
                <span className={`plan ${planTone(currentPlan)}`}>
                  {planLabel(currentPlan)}
                </span>
              ) : null}
            </>
          ) : (
            <span className="base-acc-placeholder">未绑定底座账号 (点击选择)</span>
          )}
        </div>
        <span className="base-acc-trigger-arrow">▾</span>
      </button>

      {open && (
        <div className="base-acc-popover">
          <div className="base-acc-search-box">
            <Icon name="search" size={13} />
            <input
              type="text"
              className="base-acc-search-input"
              value={query}
              onChange={(e) => setQuery(e.target.value)}
              placeholder="搜索邮箱、档位 (Pro/Free) 或标签…"
              autoFocus
            />
            {query ? (
              <button
                type="button"
                className="base-acc-search-clear"
                onClick={() => setQuery("")}
                aria-label="清空搜索"
              >
                ×
              </button>
            ) : null}
          </div>

          <div className="base-acc-list">
            {activeAccounts.length === 0 ? (
              <div className="base-acc-empty">
                <span>号池暂无可用 Cursor 账号</span>
                <button
                  type="button"
                  className="linkish"
                  onClick={() => {
                    setOpen(false);
                    onGoAccounts();
                  }}
                >
                  前往添加
                </button>
              </div>
            ) : filteredAccounts.length === 0 ? (
              <div className="base-acc-empty">没有匹配的账号</div>
            ) : (
              filteredAccounts.map((acc) => {
                const isSelected = acc.email === currentEmail;
                const plan = acc.usage?.plan ?? acc.membership ?? null;
                return (
                  <button
                    key={acc.id}
                    type="button"
                    className={`base-acc-item${isSelected ? " is-selected" : ""}`}
                    onClick={() => {
                      setOpen(false);
                      onSelect(acc.id);
                    }}
                  >
                    <div className="base-acc-item-left">
                      <span className="base-acc-item-email mono truncate">{acc.email}</span>
                      <div className="base-acc-item-meta">
                        {plan ? (
                          <span className={`plan ${planTone(plan)}`}>
                            {planLabel(plan)}
                          </span>
                        ) : null}
                        {acc.tags?.slice(0, 2).map((t) => (
                          <span key={t} className="tag tag-sm">{t}</span>
                        ))}
                      </div>
                    </div>
                    <div className="base-acc-item-right">
                      {isSelected ? (
                        <span className="base-acc-current-badge">✓ 当前绑定</span>
                      ) : null}
                    </div>
                  </button>
                );
              })
            )}
          </div>
        </div>
      )}
    </div>
  );
}

function CursorClientPanel({
  status,
  gateway,
  local,
  channels,
  peers,
  savedRoute,
  onChanged,
  onGo,
}: {
  status: CrsrStatus | null;
  gateway: GatewayStatus | null;
  local: LocalModel[] | null;
  channels: LocalChannel[];
  peers: MeshPeer[];
  savedRoute: ClientRoute | null;
  onChanged: () => Promise<void>;
  onGo: (r: Route) => void;
}) {
  const [cursorAccounts, setCursorAccounts] = useState<Account[]>([]);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);
  const [testing, setTesting] = useState(false);
  const [cursorAppPerm, setCursorAppPerm] = useState<PermItem | null>(null);
  const [checkingPerm, setCheckingPerm] = useState(false);
  const [copiedModel, setCopiedModel] = useState(false);
  const [customModels, setCustomModels] = useState<string[]>([]);
  const [syncingModel, setSyncingModel] = useState(false);
  const [testResult, setTestResult] = useState<{
    ok: boolean;
    durationMs: number;
    target: string;
    error?: string;
  } | null>(null);

  const checkPerm = useCallback(async () => {
    try {
      const rep = await permsApi.check();
      const p = rep.items.find((i) => i.id === "cursor_app") ?? null;
      setCursorAppPerm(p);
    } catch {}
  }, []);

  const refreshCustomModels = useCallback(async () => {
    try {
      const list = await crsrApi.getCustomModels();
      setCustomModels(list);
    } catch {}
  }, []);

  useEffect(() => {
    void accountsApi.list().then(setCursorAccounts).catch(() => {});
    void checkPerm();
    void refreshCustomModels();
  }, [checkPerm, refreshCustomModels]);

  const isLive = Boolean(status?.installed && status?.complete);
  const currentGatewayUrl = status?.credential?.gatewayUrl;
  const gatewayPort = gateway?.running
    ? Number(gateway.running.addr.split(":").pop())
    : (gateway?.settings?.port ?? 15721);

  // 过滤可用异构通道
  const validChannels = useMemo(
    () => channels.filter((c) => !c.unavailableReason && c.id !== "cursor"),
    [channels],
  );

  // 初始算力来源推导
  const initialSource = useMemo(() => {
    if (savedRoute?.model) {
      return `mock:${savedRoute.model}`;
    }
    if (currentGatewayUrl?.includes("/mock")) {
      return "mock:claude/claude-sonnet-4-6";
    }
    return "cursor:local";
  }, [currentGatewayUrl, savedRoute?.model]);

  const [selectedSource, setSelectedSource] = useState<string>(initialSource);

  useEffect(() => {
    setSelectedSource(initialSource);
  }, [initialSource]);

  const isSelectedMock = selectedSource.startsWith("mock:");
  const targetModel = isSelectedMock ? selectedSource.slice(5) : null;
  const bareModel = useMemo(() => {
    if (!targetModel) return null;
    return targetModel.includes("/") ? targetModel.split("/").pop()! : targetModel;
  }, [targetModel]);

  const cursorDisplayName = useMemo(() => {
    if (!bareModel) return null;
    return `${bareModel} (Nexus)`;
  }, [bareModel]);

  const isModelSynced = useMemo(() => {
    if (!cursorDisplayName) return false;
    return customModels.some((m) => {
      const ml = m.toLowerCase();
      return (
        ml === cursorDisplayName.toLowerCase() ||
        (bareModel && ml === bareModel.toLowerCase())
      );
    });
  }, [bareModel, cursorDisplayName, customModels]);

  const targetGatewayUrl = `http://127.0.0.1:${gatewayPort}/cursor/route`;

  const dirty = isLive && (
    currentGatewayUrl !== targetGatewayUrl ||
    (isSelectedMock ? savedRoute?.model !== targetModel : Boolean(savedRoute?.model))
  );

  const meshCursorPeers = useMemo(() => {
    return peers.filter(
      (p) =>
        p.shared_channels?.includes("cursor") ||
        p.remote_models?.some((m) => m.toLowerCase().includes("cursor")),
    );
  }, [peers]);

  async function handleSyncCustomModel() {
    if (!cursorDisplayName) return;
    setSyncingModel(true);
    setError(null);
    try {
      await crsrApi.syncCustomModel(cursorDisplayName);
      if (bareModel && bareModel !== cursorDisplayName) {
        try {
          await crsrApi.syncCustomModel(bareModel);
        } catch {}
      }
      await refreshCustomModels();
      setNotice(`模型「${cursorDisplayName}」已成功注入 Cursor！若 Cursor 正在运行，按 Cmd+Shift+P 运行「Reload Window」或重启即可在下拉中看到。`);
    } catch (e) {
      setError(errorText(e));
    } finally {
      setSyncingModel(false);
    }
  }

  async function handleApply() {
    let perm = cursorAppPerm;
    if (perm?.status === "unknown") {
      try {
        const rep = await permsApi.request("cursor_app");
        perm = rep.items.find((i) => i.id === "cursor_app") ?? null;
        setCursorAppPerm(perm);
      } catch {}
    }
    if (perm?.status === "denied") {
      setError("接管需要「修改 Cursor 安装」系统权限：请先点击上方「打开系统设置」放行 Nexus 后重试。");
      return;
    }
    setBusy(true);
    setError(null);
    setNotice(null);
    setTestResult(null);
    try {
      if (gateway && !gateway.running) {
        try {
          await gatewayApi.start();
        } catch {}
      }
      if (!status?.installed || !status?.complete) {
        await crsrApi.install(false);
      }
      await crsrApi.setGatewayUrl(targetGatewayUrl);
      if (isSelectedMock && targetModel) {
        await gatewayApi.setRoute("cursor", { model: targetModel });
        if (cursorDisplayName) {
          try {
            await crsrApi.syncCustomModel(cursorDisplayName);
            if (bareModel && bareModel !== cursorDisplayName) {
              try {
                await crsrApi.syncCustomModel(bareModel);
              } catch {}
            }
            await refreshCustomModels();
          } catch (syncErr) {
            console.warn("syncCustomModel warning:", syncErr);
          }
        }
      } else {
        await gatewayApi.setRoute("cursor", null);
      }
      setNotice(isLive ? "双轨路由与算力已即时生效！无需重开 Cursor。" : "Cursor 路由已成功接管！双轨分流即刻生效。");
      await onChanged();
      void checkPerm();
    } catch (e) {
      setError(errorText(e));
    } finally {
      setBusy(false);
    }
  }

  async function handleRevert() {
    const ok = await confirm("确定要还原 Cursor 为官方直连吗？之后请求将不再经过本地网关。", {
      title: "还原 Cursor 官方默认配置？",
      okLabel: "还原官方",
      danger: true,
    });
    if (!ok) return;
    setBusy(true);
    setError(null);
    setNotice(null);
    setTestResult(null);
    try {
      await crsrApi.setGatewayUrl(null);
      await crsrApi.uninstall(false);
      await gatewayApi.setRoute("cursor", null);
      setNotice("已还原官方直连，Cursor 不再经过本地网关。");
      await onChanged();
      void checkPerm();
    } catch (e) {
      setError(errorText(e));
    } finally {
      setBusy(false);
    }
  }

  async function handleSwitchBaseAccount(accId: string) {
    setBusy(true);
    setError(null);
    try {
      await crsrApi.mintForAccount(accId);
      if (isLive) {
        await crsrApi.setGatewayUrl(targetGatewayUrl);
      }
      setNotice("底座账号切换成功，凭证已即时同步！");
      await onChanged();
    } catch (e) {
      setError(errorText(e));
    } finally {
      setBusy(false);
    }
  }

  async function runTest() {
    setTesting(true);
    setTestResult(null);
    const start = performance.now();
    try {
      const probeEndpoint = `http://127.0.0.1:${gatewayPort}/cursor/route/aiserver.v1.InferenceService/Stream`;
      const res = await fetch(probeEndpoint, {
        method: "POST",
        headers: {
          "content-type": "application/connect+proto",
          "connect-protocol-version": "1",
        },
        body: new Uint8Array(0),
      });
      const durationMs = Math.round(performance.now() - start);
      setTestResult({
        ok: res.status < 500,
        target: isSelectedMock ? `双轨路由 (${bareModel} + 官方直通)` : "Cursor 原生号池",
        durationMs,
      });
    } catch (err: any) {
      const durationMs = Math.round(performance.now() - start);
      setTestResult({
        ok: false,
        target: isSelectedMock ? `双轨路由 (${bareModel} + 官方直通)` : "Cursor 原生号池",
        durationMs,
        error: err?.message || String(err),
      });
    } finally {
      setTesting(false);
    }
  }

  const primaryBtn =
    !isLive
      ? "一键接管 Cursor 路由"
      : dirty
        ? "保存并即时生效"
        : null;

  const statusHint = useMemo(() => {
    if (error) return { tone: "bad" as const, text: error, full: error };
    if (notice) return { tone: "ok" as const, text: notice, full: notice };
    if (status?.sandConflict) {
      return { tone: "warn" as const, text: "检测到旧版独立补丁，建议一键升级接管", full: `盘上存在 ${status.sandConflict} 独立补丁，一键接管将统一纳入网关调度` };
    }
    if (!isLive) {
      return { tone: "flat" as const, text: "未接管 · 当前为 Cursor 官方直连", full: "尚未注入本地网关挂点，Cursor 仍走官方直连" };
    }
    if (isSelectedMock) {
      return { tone: "ok" as const, text: `双轨路由就绪 · 扩展模型 ${cursorDisplayName}`, full: `Cursor 官方模型直通底座；在 Cursor 中选用 ${cursorDisplayName} 时走 Nexus 跨协议转译` };
    }
    return { tone: "ok" as const, text: "原生直通已生效 · 零协议损耗", full: "100% 原生 Connect-RPC 二进制透传，由本地号池/Mesh提供算力" };
  }, [error, notice, status?.sandConflict, isLive, isSelectedMock, cursorDisplayName]);

  return (
    <section className="connect-section">
      <div className="card card-flush cfg">
        {/* 与 Claude Code / Codex 完全一致的精炼头部 */}
        <div className="cfg-head">
          <div className="cfg-identity">
            <strong className="cfg-client">Cursor IDE</strong>
            <span className="pill cfg-protocol">Connect-RPC</span>
            <span className="ctile-status is-inline">
              <span className={`ctile-dot is-${isLive ? "ok" : status?.sandConflict ? "warn" : "flat"}`} aria-hidden />
              {isLive ? "已接入" : status?.sandConflict ? "有旧补丁冲突" : "未接管"}
            </span>
            {statusHint ? (
              <span className={`cfg-inline-status-hint is-${statusHint.tone} truncate`} title={statusHint.full}>
                {statusHint.text}
              </span>
            ) : null}
          </div>

          <div className="cfg-acts">
            {dirty && isLive ? (
              <span className="cfg-dirty-badge">
                <span className="cfg-dirty-dot" />
                未保存变更
              </span>
            ) : null}
            {isLive ? (
              <button
                type="button"
                className="btn btn-sm btn-quiet"
                disabled={busy}
                onClick={() => void handleRevert()}
                data-tip="撤销接管并还原 Cursor 官方默认直连"
              >
                还原默认
              </button>
            ) : null}
            {isLive ? (
              <button
                type="button"
                className="btn btn-sm"
                disabled={testing || busy}
                onClick={() => void runTest()}
              >
                {testing ? <Spinner /> : <ShellIcon name="play" size={12} />}
                测一下
              </button>
            ) : null}
            {primaryBtn ? (
              <button
                type="button"
                className={`btn btn-sm ${dirty || !isLive ? "btn-primary cfg-save-btn" : "btn-soft"}`}
                disabled={busy}
                onClick={() => void handleApply()}
              >
                {busy ? <Spinner /> : <Icon name="check" size={13} />}
                {primaryBtn}
              </button>
            ) : (
              <span className="cfg-synced-pill">
                <Icon name="check" size={12} />
                已同步
              </span>
            )}
          </div>
        </div>

        {/* 权限缺失警示横幅 */}
        {cursorAppPerm?.status === "denied" ? (
          <div className="cfg-notice-banner is-warn">
            <div className="row" style={{ gap: 8, alignItems: "center", flex: 1 }}>
              <Icon name="shield" size={15} />
              <span>
                <b>未取得修改 Cursor 安装权限</b>：macOS 限制对应用程序包的写操作。请在「系统设置 → 隐私与安全性 → App 管理」允许 Nexus。
              </span>
            </div>
            <div className="row" style={{ gap: 8, flex: "none" }}>
              <button
                type="button"
                className="btn btn-xs btn-primary"
                onClick={() => void permsApi.openSettings("cursor_app")}
              >
                <Icon name="external" size={12} />
                打开系统设置
              </button>
              <button
                type="button"
                className="btn btn-xs btn-soft"
                disabled={checkingPerm}
                onClick={async () => {
                  setCheckingPerm(true);
                  try {
                    const rep = await permsApi.request("cursor_app");
                    const p = rep.items.find((i) => i.id === "cursor_app") ?? null;
                    setCursorAppPerm(p);
                  } finally {
                    setCheckingPerm(false);
                  }
                }}
              >
                {checkingPerm ? <Spinner /> : "重新检测"}
              </button>
            </div>
          </div>
        ) : null}

        {/* 轻量测一下测试横幅 (与 Claude Code 一致) */}
        {testResult ? (
          <div className={`cfg-probe-banner is-${testResult.ok ? "ok" : "err"}`}>
            <span className={`pod-dot${testResult.ok ? " is-live" : ""}`} />
            {testResult.ok ? (
              <span>
                ✓ 链路畅通 · 目标: [<b>{testResult.target}</b>] ({testResult.durationMs}ms)
              </span>
            ) : (
              <span>
                ▲ 链路探测失败: {testResult.error || "请求未能成功通过本地网关"}
              </span>
            )}
          </div>
        ) : null}

        {/* 旧版独立补丁冲突提示 */}
        {status?.sandConflict ? (
          <div className="cfg-notice-banner is-warn">
            <span>检测到旧版独立文件补丁 ({status.sandConflict})，点击一键清理并升级为统一网关挂点</span>
            <button type="button" className="btn btn-xs btn-primary" onClick={() => void handleApply()}>
              一键升级并接管
            </button>
          </div>
        ) : null}

        {/* 单栏清爽工作台布局 */}
        <div style={{ padding: "18px 20px 24px", maxWidth: 780 }}>
          <div className="croute-v-flow">
            {/* 算力模型选择器 */}
            <div className="croute-v-item">
              <div className="croute-v-head">
                <span className="croute-v-title">算力模型</span>
              </div>
              <div className="croute-v-control">
                <select
                  className="input select select-lg select-fluid"
                  value={selectedSource}
                  onChange={(e) => setSelectedSource(e.target.value)}
                >
                  <optgroup label="Cursor 原生直通号源">
                    <option value="cursor:local">
                      本机 Cursor 号池
                    </option>
                    {meshCursorPeers.map((p) => (
                      <option key={p.node_id} value={`cursor:mesh:${p.node_id}`}>
                        Mesh · {p.device_name} ({p.latency_ms}ms)
                      </option>
                    ))}
                  </optgroup>
                  {validChannels.map((ch) => {
                    const mList = channelModels(local, channels, ch.id);
                    if (mList.length === 0) return null;
                    return (
                      <optgroup key={ch.id} label={`Nexus 跨协议模型 · ${ch.label}`}>
                        {mList.map((m) => (
                          <option key={m} value={`mock:${m}`}>
                            {splitModelId(m, channels).name} ({ch.label})
                          </option>
                        ))}
                      </optgroup>
                    );
                  })}
                  {/* Mesh 远端与共享通道模型 */}
                  {peers.filter((p) => (p.remote_models?.length ?? 0) > 0).map((p) => (
                    <optgroup key={p.node_id} label={`Mesh 共享模型 · ${p.device_name} (${p.latency_ms}ms)`}>
                      {p.remote_models.map((rm) => {
                        const scoped = rm.toLowerCase().startsWith(`${p.device_name.toLowerCase()}/`)
                          ? rm
                          : `${p.device_name.toLowerCase()}/${rm}`;
                        return (
                          <option key={rm} value={`mock:${scoped}`}>
                            {rm} (Mesh · {p.device_name})
                          </option>
                        );
                      })}
                    </optgroup>
                  ))}
                </select>
                {isSelectedMock && cursorDisplayName && (
                  <div style={{ marginTop: 8, display: "flex", alignItems: "center", gap: 10, fontSize: 12, flexWrap: "wrap" }}>
                    <span style={{ color: "var(--color-fg-muted)" }}>
                      Cursor 接入模型名：<code className="mono" style={{ color: "var(--color-fg)", fontWeight: 600 }}>{cursorDisplayName}</code>
                    </span>
                    <button
                      type="button"
                      className="btn btn-xs btn-soft"
                      onClick={async () => {
                        await navigator.clipboard.writeText(cursorDisplayName);
                        setCopiedModel(true);
                        setTimeout(() => setCopiedModel(false), 2000);
                      }}
                    >
                      {copiedModel ? "✓ 已复制" : "复制模型名"}
                    </button>
                    {isModelSynced ? (
                      <span className="badge badge-ok" style={{ display: "inline-flex", alignItems: "center", gap: 4 }}>
                        ✓ 已同步至 Cursor 模型列表
                      </span>
                    ) : (
                      <button
                        type="button"
                        className="btn btn-xs btn-outline"
                        disabled={syncingModel || busy}
                        onClick={() => void handleSyncCustomModel()}
                      >
                        {syncingModel ? "写入中…" : "+ 一键同步至 Cursor"}
                      </button>
                    )}
                  </div>
                )}
              </div>
            </div>

            {/* IDE 底座账号 */}
            <div className="croute-v-item">
              <div className="croute-v-head">
                <span className="croute-v-title">IDE 底座账号</span>
              </div>
              <div className="croute-v-control">
                <BaseAccountPicker
                  accounts={cursorAccounts}
                  currentEmail={status?.credential?.accountEmail}
                  disabled={busy}
                  onSelect={(accId) => void handleSwitchBaseAccount(accId)}
                  onGoAccounts={() => onGo(go("accounts"))}
                />
              </div>
            </div>
          </div>
        </div>
      </div>
    </section>
  );
}

/* ── 其他客户端（没有配置文件，手抄）─────────────────────────────────────── */

function OtherClients({ gateway, local, channels, peers, onGo }: { gateway: GatewayStatus | null; local: LocalModel[] | null; channels: LocalChannel[]; peers?: MeshPeer[]; onGo: (r: Route) => void }) {
  const [channel, setChannel] = useState<LocalChannelId | null>(null);
  const [model, setModel] = useState("");
  const [kind, setKind] = useState<"cline" | "sdk">("cline");
  const [protocol, setProtocol] = useState<Protocol>("openai");
  const [lang, setLang] = useState<Lang>("curl");
  const k = useRevealedKey();
  const { copied, mark } = useCopied();
  const ep = gatewayEndpoint(gateway);
  const ch = channel ?? defaultChannelId(gateway, channels);
  const ids = useMemo(() => channelModels(local, channels, ch), [local, channels, ch]);
  const defaultLocal = channels.find((c) => c.isDefault);

  useEffect(() => {
    setModel((m) => (ids.includes(m) ? m : (ids[0] ?? "")));
  }, [ids]);

  const keyText = k.visible && k.key ? k.key : KEY_PLACEHOLDER;
  const copyWith = (id: string, build: (key: string) => string) =>
    void k.ensure().then(async (key) => {
      if (!key) return;
      await navigator.clipboard.writeText(build(key)).catch(() => {});
      mark(id);
    });

  return (
    <section className="connect-section">
      <div className="card card-flush cfg">
        <div className="cfg-head">
          <div className="cfg-identity">
            <strong className="cfg-client">其他客户端</strong>
            <span className="pill cfg-protocol">OpenAI / Anthropic 兼容</span>
          </div>
        </div>
        <p className="cfg-note is-flat">
          <i aria-hidden />
          <span>
            直接打 <code className="mono">/v1</code>：模型名带通道前缀（<code className="mono">chatgpt/gpt-5.4</code>）就走那条通道，裸名走默认通道（{defaultLocal?.label ?? "未设置"}{defaultLocal?.unavailableReason ? "，当前不可用" : ""}）。默认通道在
            <button type="button" className="linkish" onClick={() => onGo(go("channels"))}>
              通道管理
            </button>
            页改。
          </span>
        </p>
        <div className="cpanel-workbench">
          {/* 左栏：通道、模型与协议选择 */}
          <div className="cpanel-editor">
            <div className="cpanel-section-title">
              <span>通道与模型选择</span>
            </div>
            <div className="croute-v-flow">
              <div className="croute-v-item">
                <div className="croute-v-head">
                  <span className="croute-v-title">路由通道</span>
                  <span className="croute-v-desc">分配给该调用的通道或节点</span>
                </div>
                <div className="croute-v-control">
                  <ChannelSelect channel={ch} onChange={(id) => setChannel(isChannelId(id, channels) ? id : null)} gateway={gateway} local={local} peers={peers} />
                </div>
              </div>
              <div className="croute-v-item">
                <div className="croute-v-head">
                  <span className="croute-v-title">目标模型</span>
                  <span className="croute-v-desc">通道内可用模型</span>
                </div>
                <div className="croute-v-control">
                  <ModelPicker value={model} options={ids.map((id) => ({ id }))} onChange={setModel} />
                </div>
              </div>
            </div>

            <div className="cpanel-section-title" style={{ marginTop: 20 }}>
              <span>客户端形式与协议标准</span>
            </div>
            <div className="tabs" role="tablist" aria-label="客户端" style={{ marginBottom: 12 }}>
              <button type="button" role="tab" aria-selected={kind === "cline"} className={kind === "cline" ? "tab is-active" : "tab"} onClick={() => setKind("cline")}>
                Cline / 兼容客户端
              </button>
              <button type="button" role="tab" aria-selected={kind === "sdk"} className={kind === "sdk" ? "tab is-active" : "tab"} onClick={() => setKind("sdk")}>
                SDK / cURL
              </button>
            </div>

            {kind === "sdk" ? (
              <div className="croute-v-flow" style={{ gap: 14 }}>
                <div className="croute-v-item">
                  <div className="croute-v-head">
                    <span className="croute-v-title">协议标准</span>
                  </div>
                  <div className="tabs" role="tablist" aria-label="协议">
                    {(Object.keys(PROTOCOL_INFO) as Protocol[]).map((p) => (
                      <button key={p} type="button" role="tab" aria-selected={protocol === p} className={protocol === p ? "tab is-active" : "tab"} onClick={() => setProtocol(p)}>
                        {PROTOCOL_INFO[p].label}
                      </button>
                    ))}
                  </div>
                </div>

                <div className="croute-v-item">
                  <div className="croute-v-head">
                    <span className="croute-v-title">语言 / 工具</span>
                  </div>
                  <div className="row" style={{ gap: 6, flexWrap: "wrap" }}>
                    {(Object.keys(LANG_LABEL) as Lang[]).map((l) => (
                      <button key={l} type="button" className="chip" aria-pressed={lang === l} onClick={() => setLang(l)}>
                        {LANG_LABEL[l]}
                      </button>
                    ))}
                  </div>
                </div>
              </div>
            ) : null}
          </div>

          {/* 右栏：生成的配置与代码 */}
          <div className="cpanel-inspector">
            <div className="cpanel-section-title">
              <span>调用参数与配置示例</span>
              <button type="button" className="btn btn-xs btn-quiet" onClick={() => void (k.visible ? k.setVisible(false) : k.ensure().then((v) => v && k.setVisible(true)))}>
                <Icon name={k.visible ? "eyeOff" : "eye"} size={12} />
                {k.visible ? "隐藏口令" : "显示口令"}
              </button>
            </div>

            {kind === "cline" ? (
              <div className="stack-tight stack" style={{ gap: 10 }}>
                {clineFields(ep, keyText, model).map((f, i) => (
                  <FieldRow key={f.label} label={f.label} value={f.value} copied={copied === `cline-${i}`} onCopy={() => copyWith(`cline-${i}`, (key) => clineFields(ep, key, model)[i]!.value)} />
                ))}
              </div>
            ) : (
              <div className="stack-tight stack" style={{ gap: 12 }}>
                <FieldRow label="Base URL" value={protocolBase(protocol, ep)} copied={copied === "sdk-base"} onCopy={() => copyWith("sdk-base", () => protocolBase(protocol, ep))} />
                <ConfigBlock
                  title={lang === "curl" ? shellLabel() : lang === "python" ? "python" : "javascript"}
                  format={LANG_LABEL[lang]}
                  code={sdkSnippet(lang, protocol, ep, keyText, model)}
                  copied={copied === "sdk"}
                  onCopy={() => copyWith("sdk", (key) => sdkSnippet(lang, protocol, ep, key, model))}
                />
              </div>
            )}
            {k.error ? <span className="tiny" style={{ color: "var(--bad)" }}>{k.error}</span> : null}
          </div>
        </div>
      </div>
    </section>
  );
}

/* ── 小件 ─────────────────────────────────────────────────────────────────── */

function ConfigBlock({
  title,
  format,
  code,
  copied,
  onCopy,
  extraAction,
}: {
  title: string;
  format: string;
  code: string;
  copied: boolean;
  onCopy: () => void;
  extraAction?: ReactNode;
}) {
  const separatorIndex = Math.max(title.lastIndexOf("/"), title.lastIndexOf("\\"));
  const directory = separatorIndex >= 0 ? title.slice(0, separatorIndex + 1) : "";
  const name = separatorIndex >= 0 ? title.slice(separatorIndex + 1) : title;
  return (
    <div className="codeblock">
      <div className="codeblock-head">
        <span className="codeblock-file" aria-hidden />
        <span className="codeblock-path truncate" title={`${title}（写入仅修改网关路由键，原配置其余项不动）`}>
          {directory ? <span className="codeblock-directory">{directory}</span> : null}
          <span className="codeblock-name">{name}</span>
        </span>
        <span className="codeblock-format">{format}</span>
        {extraAction}
        <button type="button" className={`btn btn-sm btn-quiet codeblock-copy${copied ? " is-done" : ""}`} onClick={onCopy} title="复制的是带真口令的版本">
          <Icon name={copied ? "check" : "copy"} size={13} />
          {copied ? "已复制" : "复制"}
        </button>
      </div>
      <pre className="codeblock-pre selectable">
        <Highlight code={code} />
      </pre>
    </div>
  );
}

function FieldRow({ label, value, copied, onCopy }: { label: string; value: string; copied: boolean; onCopy: () => void }) {
  return (
    <div className="frow">
      <span className="frow-k">{label}</span>
      <code className="frow-v selectable truncate" title={value}>
        {value}
      </code>
      <button type="button" className={`btn btn-sm btn-icon btn-quiet${copied ? " is-done" : ""}`} onClick={onCopy} aria-label="复制">
        <Icon name={copied ? "check" : "copy"} size={13} />
      </button>
    </div>
  );
}
