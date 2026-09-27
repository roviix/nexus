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
import { channelOfModel, defaultChannelId, isChannelId, localChannels, splitModelId, type LocalChannel, type LocalChannelId } from "../gateway/channels";
import { connect as connectApi, errorText, gateway as gatewayApi, type ConnectTool } from "../ipc/api";
import type { LocalModel } from "../ipc/models";
import type { ClientRoute, ClientState, ConnectResult, ConnectTest, GatewayStatus } from "../ipc/types";
import { ChannelPicker } from "../relay/ChannelPicker";
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
  claude: ["claude-sonnet-5", "claude-opus-5", "claude-sonnet-4.5"],
  codex: ["gpt-6-astra", "gpt-5.4", "gpt-5.6-sol", "gpt-5.5"],
  opencode: ["gpt-6-astra", "gpt-5.4", "claude-sonnet-5", "grok-4.5"],
  grok: ["grok-4.5", "grok-4.6", "grok-4"],
};

/** 第一次配时先看哪条通道：Codex 天然配 ChatGPT 的号，Grok CLI 配 Grok 的号；有号才轮得到它。 */
const HOME_CHANNEL: Partial<Record<ClientId, LocalChannelId>> = {
  codex: "chatgpt",
  grok: "grok",
};

function pickDefault(client: ClientId, ids: string[]): string {
  const hit = PREFERRED[client].map((want) => ids.find((id) => splitModelId(id).name === want)).find(Boolean);
  return hit ?? ids[0] ?? "";
}

/** 按名字给 Claude Code 另外三档挑个像样的：同一条通道里名字带 opus / haiku、flash、mini… 的。 */
function suggestRoles(ids: string[], main: string): Pick<ClientRoute, "opus" | "haiku" | "fable"> {
  const find = (words: string[]) => ids.find((id) => id !== main && words.some((w) => splitModelId(id).name.toLowerCase().includes(w))) ?? null;
  return {
    opus: find(["opus", "max", "pro"]),
    haiku: find(["haiku", "flash", "mini", "lite", "air", "fast"]),
    fable: find(["fable"]),
  };
}

/** 写进路由的模型一律带通道前缀；用户在框里手输了一个裸名，就归到当前选的通道。 */
function qualify(model: string, channel: LocalChannelId): string {
  const m = model.trim();
  if (!m) return "";
  return splitModelId(m).channel ? m : `${channel}/${m}`;
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
  const [states, setStates] = useState<ClientState[] | null>(null);
  const [stateError, setStateError] = useState<string | null>(null);
  const [pane, setPane] = useState<Pane>(route.client ?? "claude");
  const [starting, setStarting] = useState(false);

  useEffect(() => {
    if (route.client) setPane(route.client);
  }, [route.client]);

  const loadStates = useCallback(async () => {
    try {
      setStates(await connectApi.inspectAll());
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
  const channels = useMemo(() => localChannels(relay.gateway, relay.local), [relay.gateway, relay.local]);
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
      <div className="page-head">
        <h1>接入</h1>
        <button type="button" className="btn btn-sm btn-icon btn-soft" onClick={() => void reloadAll()} disabled={relay.loading} title="刷新" aria-label="刷新">
          <Icon name="refresh" size={14} className={relay.loading ? "is-spinning" : undefined} />
        </button>
      </div>

      {relay.gateway && !running ? (
        <Banner
          title="本地网关没开"
          hint="客户端的请求要经过它。一键接入时会自动开启，并设成随 Nexus 启动。"
          action={
            <button type="button" className="btn btn-sm" disabled={starting} onClick={() => void startGateway()}>
              {starting ? <Spinner /> : null}
              现在开启
            </button>
          }
        />
      ) : null}
      {stateError ? <Banner tone="bad" title="读不出客户端配置" hint={stateError} /> : null}

      <section className="connect-section">
        <h2 className="connect-section-title">客户端</h2>
        <div className="ctiles">
          {CLIENT_IDS.map((id) => (
            <ClientTile key={id} meta={CLIENTS[id]} state={byTool.get(id)} saved={relay.gateway?.routes?.[id] ?? null} loading={states == null} active={pane === id} onPick={() => choose(id)} />
          ))}
          <button type="button" className={`card ctile${pane === "other" ? " card-hot is-active" : ""}`} aria-pressed={pane === "other"} onClick={() => choose("other")}>
            <span className="ctile-top">
              <span className="toolcard-glyph">
                <span className="mono">{"{}"}</span>
              </span>
              <span className="ctile-name">其他客户端</span>
            </span>
            <span className="ctile-line">Cline、SDK、cURL · 手动填</span>
          </button>
        </div>
      </section>

      {pane === "other" ? (
        <OtherClients gateway={relay.gateway} local={relay.local} channels={channels} onGo={onGo} />
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
  onPick,
}: {
  meta: ClientMeta;
  state: ClientState | undefined;
  saved: ClientRoute | null;
  loading: boolean;
  active: boolean;
  onPick: () => void;
}) {
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
        <span className={`toolcard-glyph${meta.vendor ? ` is-${meta.vendor}` : ""}`}>
          {meta.vendor ? <VendorLogo vendor={meta.vendor} size={16} mono={!active} /> : <span className="mono">{meta.glyph}</span>}
        </span>
        <span className="ctile-name">{meta.label}</span>
      </span>
      <span className="ctile-status">
        <span className={`ctile-dot is-${h.tone}`} aria-hidden />
        {loading ? "…" : h.label}
        {state?.env.length ? <span className="ctile-env" title="有环境变量可能架空这份配置">!</span> : null}
      </span>
      <span className="ctile-line mono truncate" title={line}>
        {line}
      </span>
    </button>
  );
}

/* ── 一个客户端的路由与接入 ───────────────────────────────────────────────── */

function channelModels(local: LocalModel[] | null, channels: LocalChannel[], id: LocalChannelId): string[] {
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
  if (preset.model) {
    const ch = splitModelId(preset.model).channel ?? (isChannelId(preset.channel) ? preset.channel : defaultChannelId(gateway));
    return { route: { ...(saved ?? { model: "" }), model: qualify(preset.model, ch) }, channel: ch };
  }
  if (saved?.model) {
    const ch = splitModelId(saved.model).channel ?? defaultChannelId(gateway);
    if (!isChannelId(preset.channel) || preset.channel === ch) return { route: saved, channel: ch };
  }
  // 没配过：优先地址里带来的通道，其次这个客户端的「本家」通道（有号时），再其次默认通道、
  // 第一条有模型的通道。
  const home = HOME_CHANNEL[client];
  const order: LocalChannelId[] = [
    ...(isChannelId(preset.channel) ? [preset.channel] : []),
    ...(home && channels.find((c) => c.id === home)?.ready ? [home] : []),
    defaultChannelId(gateway),
    ...channels.filter((c) => c.ready).map((c) => c.id),
  ];
  const ch = order.find((id) => channelModels(local, channels, id).length > 0) ?? order[0] ?? "cursor";
  return { route: { model: pickDefault(client, channelModels(local, channels, ch)) }, channel: ch };
}

function ClientPanel({
  meta,
  state,
  gateway,
  local,
  channels,
  preset,
  onChanged,
  onGo,
}: {
  meta: ClientMeta;
  state: ClientState | undefined;
  gateway: GatewayStatus;
  local: LocalModel[] | null;
  channels: LocalChannel[];
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
    const next = isChannelId(id) ? id : defaultChannelId(gateway);
    setChannel(next);
    setDone(null);
    const ids = channelModels(local, channels, next);
    setDraft((d) => (ids.includes(d.model) ? d : { ...d, model: pickDefault(client, ids) }));
  }

  const routeToSave = (): ClientRoute => {
    const r = cleanRoute({ ...draft, model: qualify(draft.model, channel) });
    const q = (v: string | null | undefined) => (v ? qualify(v, channel) : null);
    return { ...r, opus: q(r.opus), haiku: q(r.haiku), fable: q(r.fable) };
  };

  async function apply() {
    const next = routeToSave();
    if (!next.model) {
      setError("先选一个模型。");
      return;
    }
    setBusy(true);
    setError(null);
    setDone(null);
    setTest(null);
    try {
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
    health === "stale" ? "修复接入" : health === "legacy" ? "升级接入" : live ? (dirty ? "保存，即时生效" : null) : health === "other" ? "改接到 Nexus" : "一键接入";

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
          </div>
          <div className="cfg-acts">
            {state?.revertible || live ? (
              <button type="button" className="btn btn-sm btn-quiet" disabled={busy} onClick={() => void revert()} data-tip="按接入前的备份还原；是我们建的文件就删掉">
                撤销
              </button>
            ) : null}
            {live ? (
              <button type="button" className="btn btn-sm" disabled={testing || busy} onClick={() => void runTest()}>
                {testing ? <Spinner /> : <ShellIcon name="play" size={12} />}
                测一下
              </button>
            ) : null}
            {primary ? (
              <button type="button" className="btn btn-sm btn-primary" disabled={busy || !draft.model.trim()} onClick={() => void apply()}>
                {busy ? <Spinner /> : <Icon name="check" size={13} />}
                {primary}
              </button>
            ) : (
              <span className="cfg-live">
                <Icon name="check" size={12} />
                已接入
              </span>
            )}
          </div>
        </div>

        <PanelNotes
          meta={meta}
          state={state}
          health={health}
          dirty={dirty}
          error={error}
          done={done}
          defaultLabel={channelLabel(defaultChannelId(gateway))}
          gatewayPort={gateway.running ? Number(gateway.running.addr.split(":").pop()) : gateway.settings.port}
        />

        <div className="croute">
          <div className="croute-row">
            <span className="croute-k">通道</span>
            <div className="croute-v">
              <ChannelPicker
                plain
                channel={channel}
                onChange={pickChannel}
                gateway={gateway}
                local={local}
                onManageLocal={(id) => onGo(id === "cursor" ? go("gateway", { sub: "pool" }) : go("accounts", { platform: id }))}
              />
            </div>
          </div>
          <div className="croute-row">
            <span className="croute-k">{client === "claude" ? "主模型" : "模型"}</span>
            <div className="croute-v">
              <ModelPicker
                value={draft.model}
                options={options}
                placeholder={inChannel.length ? "选一个模型，或直接输入 id" : "这条通道还没有可用的模型——先去添加号"}
                onChange={(v) => {
                  setDraft((d) => ({ ...d, model: v }));
                  setDone(null);
                }}
              />
              <span className="croute-hint">
                {client === "claude"
                  ? "Sonnet 档与没单独配的档走它。Claude Code 里默认用的也是这一档。"
                  : `写进配置的是 ${clientModelId(qualify(draft.model, channel)) || "…"}；在 ${meta.label} 里换成这条通道认识的别的模型，也照样走这条通道。`}
              </span>
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

        {state?.env.length ? <EnvWarnings env={state.env} client={meta.label} /> : null}

        {test ? <TestResult test={test} channelLabel={channelLabel} /> : null}

        <ConfigPreview meta={meta} gateway={gateway} route={routeToSave()} />
      </div>
    </section>
  );
}

/** Claude Code 另外三档。默认收着：大多数人四档走一个模型就够了。 */
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
  const rows: Array<{ key: "opus" | "haiku" | "fable"; label: string; hint: string }> = [
    { key: "opus", label: "Opus", hint: "跟主模型" },
    { key: "haiku", label: "Haiku", hint: "跟主模型 · 后台起标题、做摘要走这一档" },
    { key: "fable", label: "Fable", hint: "跟 Opus" },
  ];
  return (
    <div className="croute-row">
      <span className="croute-k">分档</span>
      <div className="croute-v">
        {!open ? (
          <button type="button" className="linkish croute-more" onClick={() => setOpen(true)}>
            四档都走主模型 · 分开配
          </button>
        ) : (
          <div className="croles">
            {rows.map((r) => (
              <div key={r.key} className="crole">
                <span className="crole-k">{r.label}</span>
                <ModelPicker value={draft[r.key] ?? ""} options={options} placeholder={r.hint} onChange={(v) => onChange({ [r.key]: v || null })} />
              </div>
            ))}
            <div className="crole-foot">
              <label className="crole-check">
                <input type="checkbox" className="tick" checked={Boolean(draft.context1m)} onChange={(e) => onChange({ context1m: e.target.checked })} />
                按 1M 上下文算（Sonnet / Opus / Fable）
              </label>
              <span className="row" style={{ gap: 6 }}>
                <button type="button" className="btn btn-sm btn-quiet" onClick={onSuggest}>
                  按名字自动分
                </button>
                <button
                  type="button"
                  className="btn btn-sm btn-quiet"
                  onClick={() => {
                    onChange({ opus: null, haiku: null, fable: null, context1m: false });
                    setOpen(false);
                  }}
                >
                  都跟主模型
                </button>
              </span>
            </div>
          </div>
        )}
      </div>
    </div>
  );
}

/** 卡头下面那几行：有话才出。 */
function PanelNotes({
  meta,
  state,
  health,
  dirty,
  error,
  done,
  defaultLabel,
  gatewayPort,
}: {
  meta: ClientMeta;
  state: ClientState | undefined;
  health: Health;
  dirty: boolean;
  error: string | null;
  done: { kind: "applied"; r: ConnectResult; update: boolean } | { kind: "reverted" } | null;
  defaultLabel: string;
  gatewayPort: number;
}) {
  const notes: Array<{ tone: "ok" | "warn" | "bad" | "flat"; body: ReactNode }> = [];
  if (error) notes.push({ tone: "bad", body: error });
  if (done?.kind === "applied") {
    const r = done.r;
    const extra = [r.gatewayStarted ? "本地网关已开启" : "", r.autostartEnabled ? "已设为随 Nexus 启动" : "", r.onboarded ? "已跳过 Claude Code 的首次引导" : ""].filter(Boolean);
    notes.push({
      tone: "ok",
      body: done.update ? (
        <>路由已更新，下一条请求就按新的走，不用重启 {meta.label}。{extra.length ? `${extra.join("，")}。` : ""}</>
      ) : (
        <>
          已写入 {r.files.map((f) => f.path.split(/[\\/]/).pop()).join("、")}
          {r.files.some((f) => f.backup) ? "（原文件已备份）" : ""}。{extra.length ? `${extra.join("，")}。` : ""}重开 {meta.label} 生效，之后在这里改通道、改模型都即时生效。
        </>
      ),
    });
  } else if (done?.kind === "reverted") {
    notes.push({ tone: "flat", body: <>已撤销。{meta.label} 不再经过本地网关。</> });
  }
  if (!done) {
    if (health === "stale") {
      if (state?.keyOk === false) notes.push({ tone: "bad", body: <>配置里的口令和网关现在的不一样（换过口令？），请求会 401。点「修复接入」重写一遍。</> });
      if (state?.portOk === false) notes.push({ tone: "bad", body: <>配置里的端口和网关现在的（{gatewayPort}）对不上，客户端连不上。点「修复接入」重写一遍。</> });
    } else if (health === "legacy") {
      notes.push({ tone: "warn", body: <>这是旧版接入：请求走全局默认通道（{defaultLabel}），不看这里的路由。点「升级接入」改成按客户端路由，之后改模型不用再重启。</> });
    } else if (health === "other") {
      notes.push({ tone: "flat", body: <>现在指向 <code className="mono">{hostOf(state?.baseUrl)}</code>。接入会先把原文件备份，撤销能还原。</> });
    } else if (health === "live" && dirty) {
      notes.push({ tone: "warn", body: <>路由改了还没保存。保存后下一条请求就生效，不用重启 {meta.label}。</> });
    } else if (health === "live") {
      notes.push({ tone: "flat", body: <>在这里改通道和模型，下一条请求就生效——不用重写配置、不用重启 {meta.label}。</> });
    }
  }
  if (!notes.length) return null;
  return (
    <>
      {notes.map((n, i) => (
        <p key={i} className={`cfg-note is-${n.tone}`}>
          <i aria-hidden />
          <span>{n.body}</span>
        </p>
      ))}
    </>
  );
}

function EnvWarnings({ env, client }: { env: ClientState["env"]; client: string }) {
  return (
    <div className="cenv">
      <div className="cenv-head">
        <Icon name="info" size={12} />
        这些环境变量可能让 {client} 不照配置走
      </div>
      {env.map((e) => (
        <div key={`${e.name}@${e.source}`} className="cenv-row">
          <code className="mono">
            {e.name}={e.value}
          </code>
          <span className="faint tiny">{e.source}</span>
          <span className="cenv-effect">{e.effect}</span>
        </div>
      ))}
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

function ConfigPreview({ meta, gateway, route }: { meta: ClientMeta; gateway: GatewayStatus; route: ClientRoute }) {
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
    }
  };
  const text = build(k.visible && k.key ? k.key : KEY_PLACEHOLDER);
  return (
    <details className="cpreview">
      <summary>
        写进配置的内容
        <span className="faint tiny">{homePath(meta.file)}</span>
      </summary>
      <div className="cpreview-body">
        <div className="row-between" style={{ gap: 8 }}>
          <span className="faint tiny">一键接入写的就是这些键；别的内容一字不动。复制的是带真口令的版本。</span>
          <button type="button" className="btn btn-sm btn-quiet" onClick={() => void (k.visible ? k.setVisible(false) : k.ensure().then((v) => v && k.setVisible(true)))}>
            <Icon name={k.visible ? "eyeOff" : "eye"} size={13} />
            {k.visible ? "隐藏口令" : "显示口令"}
          </button>
        </div>
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
        />
      </div>
    </details>
  );
}

/* ── 其他客户端（没有配置文件，手抄）─────────────────────────────────────── */

function OtherClients({ gateway, local, channels, onGo }: { gateway: GatewayStatus | null; local: LocalModel[] | null; channels: LocalChannel[]; onGo: (r: Route) => void }) {
  const [channel, setChannel] = useState<LocalChannelId | null>(null);
  const [model, setModel] = useState("");
  const [kind, setKind] = useState<"cline" | "sdk">("cline");
  const [protocol, setProtocol] = useState<Protocol>("openai");
  const [lang, setLang] = useState<Lang>("curl");
  const k = useRevealedKey();
  const { copied, mark } = useCopied();
  const ep = gatewayEndpoint(gateway);
  const ch = channel ?? defaultChannelId(gateway);
  const ids = useMemo(() => channelModels(local, channels, ch), [local, channels, ch]);

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
            直接打 <code className="mono">/v1</code>：模型名带通道前缀（<code className="mono">chatgpt/gpt-5.4</code>）就走那条通道，裸名走默认通道（{channels.find((c) => c.isDefault)?.label ?? "Cursor"}）。默认通道在
            <button type="button" className="linkish" onClick={() => onGo(go("gateway"))}>
              本地网关
            </button>
            页改。
          </span>
        </p>
        <div className="croute">
          <div className="croute-row">
            <span className="croute-k">通道</span>
            <div className="croute-v">
              <ChannelPicker plain channel={ch} onChange={(id) => setChannel(isChannelId(id) ? id : null)} gateway={gateway} local={local} />
            </div>
          </div>
          <div className="croute-row">
            <span className="croute-k">模型</span>
            <div className="croute-v">
              <ModelPicker value={model} options={ids.map((id) => ({ id }))} onChange={setModel} />
            </div>
          </div>
        </div>
        <div className="cfg-body" key={kind}>
          <div className="row-between wrap" style={{ gap: 10 }}>
            <div className="tabs" role="tablist" aria-label="客户端">
              <button type="button" role="tab" aria-selected={kind === "cline"} className={kind === "cline" ? "tab is-active" : "tab"} onClick={() => setKind("cline")}>
                Cline / 兼容客户端
              </button>
              <button type="button" role="tab" aria-selected={kind === "sdk"} className={kind === "sdk" ? "tab is-active" : "tab"} onClick={() => setKind("sdk")}>
                SDK / cURL
              </button>
            </div>
            <button type="button" className="btn btn-sm btn-quiet" onClick={() => void (k.visible ? k.setVisible(false) : k.ensure().then((v) => v && k.setVisible(true)))}>
              <Icon name={k.visible ? "eyeOff" : "eye"} size={13} />
              {k.visible ? "隐藏口令" : "显示口令"}
            </button>
          </div>
          {kind === "cline" ? (
            <div className="stack-tight stack">
              {clineFields(ep, keyText, model).map((f, i) => (
                <FieldRow key={f.label} label={f.label} value={f.value} copied={copied === `cline-${i}`} onCopy={() => copyWith(`cline-${i}`, (key) => clineFields(ep, key, model)[i]!.value)} />
              ))}
            </div>
          ) : (
            <>
              <div className="row-between wrap" style={{ gap: 10 }}>
                <div className="tabs" role="tablist" aria-label="协议">
                  {(Object.keys(PROTOCOL_INFO) as Protocol[]).map((p) => (
                    <button key={p} type="button" role="tab" aria-selected={protocol === p} className={protocol === p ? "tab is-active" : "tab"} onClick={() => setProtocol(p)}>
                      {PROTOCOL_INFO[p].label}
                    </button>
                  ))}
                </div>
                <div className="row" style={{ gap: 4 }}>
                  {(Object.keys(LANG_LABEL) as Lang[]).map((l) => (
                    <button key={l} type="button" className="chip" aria-pressed={lang === l} onClick={() => setLang(l)}>
                      {LANG_LABEL[l]}
                    </button>
                  ))}
                </div>
              </div>
              <FieldRow label="Base URL" value={protocolBase(protocol, ep)} copied={copied === "sdk-base"} onCopy={() => copyWith("sdk-base", () => protocolBase(protocol, ep))} />
              <ConfigBlock
                title={lang === "curl" ? shellLabel() : lang === "python" ? "python" : "javascript"}
                format={LANG_LABEL[lang]}
                code={sdkSnippet(lang, protocol, ep, keyText, model)}
                copied={copied === "sdk"}
                onCopy={() => copyWith("sdk", (key) => sdkSnippet(lang, protocol, ep, key, model))}
              />
            </>
          )}
          {k.error ? <span className="tiny" style={{ color: "var(--bad)" }}>{k.error}</span> : null}
        </div>
      </div>
    </section>
  );
}

/* ── 小件 ─────────────────────────────────────────────────────────────────── */

function ConfigBlock({ title, format, code, copied, onCopy }: { title: string; format: string; code: string; copied: boolean; onCopy: () => void }) {
  const separatorIndex = Math.max(title.lastIndexOf("/"), title.lastIndexOf("\\"));
  const directory = separatorIndex >= 0 ? title.slice(0, separatorIndex + 1) : "";
  const name = separatorIndex >= 0 ? title.slice(separatorIndex + 1) : title;
  return (
    <div className="codeblock">
      <div className="codeblock-head">
        <span className="codeblock-file" aria-hidden />
        <span className="codeblock-path truncate" title={title}>
          {directory ? <span className="codeblock-directory">{directory}</span> : null}
          <span className="codeblock-name">{name}</span>
        </span>
        <span className="codeblock-format">{format}</span>
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
