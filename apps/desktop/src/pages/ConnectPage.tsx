/**
 * 接入 —— 把一个客户端指到本地网关上：选通道、选工具、选模型，再生成对应配置。
 *
 * 网关里的每一条通道（Cursor / ChatGPT / Grok Build / Kiro）摆成顶部一排卡，切一下，下面的
 * 模型候选和整份配置跟着换，而不是各抄一遍。几条通道共用同一个地址 —— 选哪条只决定
 * 「模型下拉里列谁」；目录主键是 `{通道}/{模型}`，不带前缀的请求走用户设的默认通道。
 *
 * 通道、客户端、配置与连接测试是四个平级内容区。这里不做步骤轨道：它们是当前配置的四个
 * 组成部分，不是必须逐项完成的向导。
 *
 * 钥匙不常驻在界面上：配置里先摆占位，点「显示」或「复制」才去取（取过记活动日志，
 * 和账号凭证同一套规矩）。
 */
import { useCallback, useEffect, useMemo, useRef, useState, type ReactNode } from "react";
import { channelOfModel, defaultChannelId, localChannels, splitModelId, type LocalChannelId } from "../gateway/channels";
import { connect as connectApi, errorText, gateway as gatewayApi, type ConnectTool } from "../ipc/api";
import type { ClientState, ConnectApplied, ConnectReverted } from "../ipc/types";
import { Highlight } from "../relay/Highlight";
import { ModelPicker, type PickerOption } from "../relay/ModelPicker";
import { VendorLogo } from "../relay/VendorLogo";
import {
  claudeSettings,
  clientModelId,
  clineFields,
  codexAuth,
  codexToml,
  grokToml,
  opencodeJson,
  endpointOf,
  KEY_PLACEHOLDER,
  LANG_LABEL,
  PROTOCOL_INFO,
  protocolBase,
  sdkSnippet,
  shellLabel,
  toolMeta,
  TOOLS,
  type Endpoint,
  type Lang,
  type Protocol,
  type Tool,
} from "../relay/snippets";
import { ChannelPicker } from "../relay/ChannelPicker";
import { TryResult } from "../relay/TryResult";
import { useRelay } from "../relay/useRelay";
import { useTryRun } from "../relay/useTryRun";
import { go, type Route } from "../shell/nav";
import { ShellIcon } from "../shell/ShellIcon";
import { homePath } from "../ui/platform";
import { Icon, Spinner } from "../ui/primitives";

const DEFAULT_PROMPT = "用一句话介绍你自己，并说出你是哪个模型。";

/**
 * 每个工具打开时的默认模型：Claude Code 走 Anthropic 协议、默认 Claude 旗舰；Codex /
 * SDK 在 ChatGPT 通道优先 gpt-6-astra。目录里没有时按顺序回落，最后取目录第一个。
 */
const TOOL_DEFAULT_MODEL: Record<Tool, string[]> = {
  claude: ["claude-sonnet-5", "claude-opus-5"],
  codex: ["gpt-6-astra", "gpt-5.4", "gpt-5.6-sol"],
  opencode: ["gpt-6-astra", "gpt-5.4", "grok-4.5", "gpt-5.6-sol"],
  grok: ["grok-4.5", "grok-4.6"],
  cline: ["gpt-6-astra", "gpt-5.4", "claude-sonnet-5", "gpt-5.6-sol"],
  sdk: ["gpt-6-astra", "gpt-5.4", "claude-sonnet-5", "gpt-5.6-sol"],
};

function pickDefault(tool: Tool, ids: string[]): string {
  return TOOL_DEFAULT_MODEL[tool].find((m) => ids.some((id) => id === m || splitModelId(id).name === m)) ?? ids[0] ?? TOOL_DEFAULT_MODEL[tool][0]!;
}

export function ConnectPage({ route, onGo }: { route: Route; onGo: (r: Route) => void }) {
  const relay = useRelay({ catalogs: true });

  /** 哪一条通道（平台 id）；null 按默认（Cursor）。 */
  const [channel, setChannel] = useState<string | null>(route.channel ?? null);
  const [tool, setTool] = useState<Tool>("claude");
  const [model, setModel] = useState<string>(route.model ?? "");
  const userPickedModel = useRef(Boolean(route.model));
  const [protocol, setProtocol] = useState<Protocol>("openai");
  const [lang, setLang] = useState<Lang>("curl");
  const [prompt, setPrompt] = useState(DEFAULT_PROMPT);
  const [keyError, setKeyError] = useState<string | null>(null);

  /** 已显示出来的口令。 */
  const [shownKey, setShownKey] = useState<string | undefined>(undefined);
  const [keyVisible, setKeyVisible] = useState(false);

  const { run, busy, start, stats } = useTryRun();

  // 地址栏带来的通道 / 模型变了（从模型广场再点一次「用它接入」）就跟着换。
  useEffect(() => {
    if (route.channel) setChannel(route.channel);
    if (route.model) {
      setModel(route.model);
      userPickedModel.current = true;
    }
  }, [route.channel, route.model]);

  const meta = toolMeta(tool);

  const channels = useMemo(() => localChannels(relay.gateway, relay.local), [relay.gateway, relay.local]);
  const localId: LocalChannelId = (channel as LocalChannelId | null) ?? defaultChannelId(relay.gateway);
  /** 目录里归当前那条通道的模型；模型下拉只列它们。 */
  const localInChannel = useMemo(() => (relay.local ?? []).filter((m) => channelOfModel(channels, m.id) === localId), [relay.local, channels, localId]);
  const ids = useMemo(() => localInChannel.map((m) => m.id), [localInChannel]);

  // 用户没手选过模型时，跟着工具 / 通道换默认。
  useEffect(() => {
    if (userPickedModel.current) return;
    if (!ids.length) return;
    setModel(pickDefault(tool, ids));
  }, [tool, ids]);

  const endpoint: Endpoint = useMemo(() => {
    const g = relay.gateway;
    if (g?.running) return endpointOf(g.running.baseUrl);
    const port = g?.settings.port ?? 8787;
    return endpointOf(`http://127.0.0.1:${port}`);
  }, [relay.gateway]);

  const keyForText = keyVisible && shownKey ? shownKey : KEY_PLACEHOLDER;

  /** 取口令（已取过就直接给）。用户显式动作才调；取过记活动日志（Rust 侧）。 */
  const ensureKey = useCallback(async (): Promise<string | null> => {
    setKeyError(null);
    try {
      if (shownKey) return shownKey;
      const k = await gatewayApi.revealKey();
      setShownKey(k);
      return k;
    } catch (e) {
      setKeyError(errorText(e));
      return null;
    }
  }, [shownKey]);

  async function reveal() {
    const k = await ensureKey();
    if (k) setKeyVisible(true);
  }

  const [copied, setCopied] = useState("");
  const copyTimer = useRef<number | undefined>(undefined);
  useEffect(() => () => window.clearTimeout(copyTimer.current), []);

  const markCopied = useCallback((id: string) => {
    setCopied(id);
    window.clearTimeout(copyTimer.current);
    copyTimer.current = window.setTimeout(() => setCopied(""), 1400);
  }, []);

  /** 复制一段带真钥匙的文本：先取钥匙再复制，一步到位。 */
  const copyWithKey = useCallback(
    async (id: string, build: (key: string) => string) => {
      const k = await ensureKey();
      if (!k) return;
      await navigator.clipboard.writeText(build(k)).catch(() => {});
      markCopied(id);
    },
    [ensureKey, markCopied],
  );

  const ap = useApply(tool, model.trim());
  const applyBlocked = !model.trim() ? "先选一个模型" : null;

  const canTest = Boolean(relay.gateway?.running) && model.trim() !== "" && !busy;

  async function test() {
    if (!canTest) return;
    await start(model.trim(), prompt);
  }

  // 模型下拉的候选。
  const options: PickerOption[] = useMemo(
    () =>
      localInChannel.map((m) => ({
        id: m.id,
        meta: m.variant !== "standard" ? m.variant : undefined,
      })),
    [localInChannel],
  );

  const localModel = (relay.local ?? []).find((m) => m.id === model);
  return (
    <div className="connect">
      <div className="page-head">
        <h1>接入</h1>
        <button type="button" className="btn btn-sm btn-icon btn-soft" onClick={() => void relay.reload()} disabled={relay.loading} title="刷新" aria-label="刷新">
          <Icon name="refresh" size={14} className={relay.loading ? "is-spinning" : undefined} />
        </button>
      </div>

      <ConnectSection title="通道">
        <ChannelPicker
          bare
          channel={channel}
          onChange={setChannel}
          gateway={relay.gateway}
          local={relay.local}
          onStartGateway={() => onGo(go("gateway"))}
          onManageLocal={(id) => onGo(id === "cursor" ? go("gateway", { sub: "pool" }) : go("accounts", { platform: id }))}
        />
      </ConnectSection>

      <ConnectSection title="客户端">
        <div className="tools">
          {TOOLS.map((t) => {
            const active = t.id === tool;
            return (
              <button key={t.id} type="button" className={`card toolcard${active ? " card-hot is-active" : ""}`} aria-pressed={active} onClick={() => setTool(t.id)}>
                <span className={`toolcard-glyph${TOOL_VENDOR[t.id] ? ` is-${TOOL_VENDOR[t.id]}` : ""}`}>
                  {TOOL_VENDOR[t.id] ? <VendorLogo vendor={TOOL_VENDOR[t.id]!} size={16} mono={!active} /> : <span className="mono">{t.glyph}</span>}
                </span>
                <span className="toolcard-name">{t.label}</span>
                <span className="toolcard-sub">{t.sub}</span>
                {active ? <ShellIcon name="check" size={12} className="toolcard-tick" /> : null}
              </button>
            );
          })}
        </div>
      </ConnectSection>

      <ConnectSection title="配置">
        <div className="card card-flush cfg">
          <div className="cfg-head">
            <div className="cfg-identity">
              <strong className="cfg-client">{meta.label}</strong>
              <span className="pill cfg-protocol">{PROTOCOL_INFO[tool === "sdk" ? protocol : meta.protocol].label}</span>
            </div>
            {ap.supported ? (
              <div className="cfg-acts">
                {ap.state?.revertible ? (
                  <button type="button" className="btn btn-sm btn-quiet" disabled={ap.busy} onClick={() => void ap.revert()} data-tip="按接入前的备份还原；是我们建的文件就删掉">
                    撤销
                  </button>
                ) : null}
                {ap.onThis && !ap.modelDiffers ? (
                  <span className="cfg-live">
                    <Icon name="check" size={12} />
                    已接入
                  </span>
                ) : (
                  <button
                    type="button"
                    className="btn btn-sm btn-primary"
                    disabled={ap.busy || Boolean(applyBlocked) || !ap.state}
                    data-tip={ap.state?.exists && !ap.state.revertible ? "会先把原文件备份到 ~/.roviix/backups/clients" : undefined}
                    onClick={() => void ap.apply()}
                  >
                    {ap.busy ? <Spinner /> : <Icon name="check" size={13} />}
                    {ap.modelDiffers ? "重新写入" : "一键接入"}
                  </button>
                )}
              </div>
            ) : null}
          </div>

          <ApplyNote ap={ap} blocked={applyBlocked} client={meta.label} model={model.trim()} gatewayRunning={Boolean(relay.gateway?.running)} onGoGateway={() => onGo(go("gateway"))} />

          <div className="cfg-params">
            {/* 钥匙 */}
            <div className="field">
              <label>
                <Icon name="key" size={12} className="field-ico" />
                网关口令
              </label>
              <div className="keyline">
                <code className="keyline-val">{keyVisible && shownKey ? shownKey : relay.gateway?.apiKeySet ? "••••••••••••••••" : "首次开启网关时生成"}</code>
                <KeyActions
                  visible={keyVisible && Boolean(shownKey)}
                  disabled={!relay.gateway?.apiKeySet}
                  onReveal={() => void reveal()}
                  onHide={() => setKeyVisible(false)}
                  onCopy={() => void copyWithKey("key", (k) => k)}
                  copied={copied === "key"}
                />
              </div>
              {keyError ? <span className="tiny" style={{ color: "var(--bad)" }}>{keyError}</span> : null}
            </div>

            {/* 模型 */}
            <div className="field">
              <label>
                <ShellIcon name="layers" size={12} className="field-ico" />
                模型
              </label>
              <ModelPicker
                value={model}
                options={options}
                onChange={(v) => {
                  setModel(v);
                  userPickedModel.current = true;
                }}
              />
              <ModelHint
                id={model}
                local={Boolean(localModel)}
                routedTo={localModel && channelOfModel(channels, model) !== localId ? channels.find((c) => c.id === channelOfModel(channels, model))?.label : undefined}
                known={(relay.local ?? []).length > 0}
              />
            </div>
          </div>

        {/* 配置正文。key 带上工具：换一个就重挂载，淡入的动效跟着重放。 */}
        <div className="cfg-body" key={tool}>
            {tool === "claude" ? (
              <ConfigBlock
                title={homePath(".claude/settings.json")}
                format="JSON"
                code={claudeSettings(endpoint, keyForText, model)}
                copied={copied === "claude"}
                onCopy={() => void copyWithKey("claude", (k) => claudeSettings(endpoint, k, model))}
              />
            ) : null}

            {tool === "codex" ? (
              <>
                <p className="muted" style={{ margin: "0 0 8px", fontSize: 11.5, lineHeight: 1.6 }}>
                  Codex 只认短名，配置里写成 <code className="mono">{clientModelId(model) || "gpt-5.4"}</code>
                  。请把 ChatGPT 设为默认通道，裸名才会走这队号。
                </p>
                <ConfigBlock
                  title={homePath(".codex/config.toml")}
                  format="TOML"
                  code={codexToml(endpoint, keyForText, model)}
                  copied={copied === "codex"}
                  onCopy={() => void copyWithKey("codex", (k) => codexToml(endpoint, k, model))}
                />
                <details className="cfg-fold">
                  <summary>0.46 及更早的 Codex 只从 auth.json 取钥匙</summary>
                  <div className="stack" style={{ paddingTop: 10 }}>
                    <p className="muted" style={{ margin: 0, fontSize: 11.5, lineHeight: 1.6 }}>
                      旧版不认 <code className="mono">experimental_bearer_token</code>，会退回读这份文件。两份都放着就覆盖全部版本。
                    </p>
                    <ConfigBlock
                      title={homePath(".codex/auth.json")}
                      format="JSON"
                      code={codexAuth(keyForText)}
                      copied={copied === "codex-auth"}
                      onCopy={() => void copyWithKey("codex-auth", (k) => codexAuth(k))}
                    />
                  </div>
                </details>
              </>
            ) : null}

            {tool === "opencode" ? (
              <ConfigBlock
                title={homePath(".config/opencode/opencode.json")}
                format="JSON"
                code={opencodeJson(endpoint, keyForText, model)}
                copied={copied === "opencode"}
                onCopy={() => void copyWithKey("opencode", (k) => opencodeJson(endpoint, k, model))}
              />
            ) : null}

            {tool === "grok" ? (
              <ConfigBlock
                title={homePath(".grok/config.toml")}
                format="TOML"
                code={grokToml(endpoint, keyForText, model)}
                copied={copied === "grok"}
                onCopy={() => void copyWithKey("grok", (k) => grokToml(endpoint, k, model))}
              />
            ) : null}

            {tool === "cline" ? (
              <div className="stack-tight stack">
                {clineFields(endpoint, keyForText, model).map((f, i) => (
                  <FieldRow
                    key={f.label}
                    label={f.label}
                    value={f.value}
                    copied={copied === `cline-${i}`}
                    onCopy={() =>
                      void copyWithKey(`cline-${i}`, (k) => clineFields(endpoint, k, model)[i]!.value)
                    }
                  />
                ))}
              </div>
            ) : null}

            {tool === "sdk" ? (
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
                <FieldRow
                  label="Base URL"
                  value={protocolBase(protocol, endpoint)}
                  copied={copied === "sdk-base"}
                  onCopy={() => void copyWithKey("sdk-base", () => protocolBase(protocol, endpoint))}
                />
                <FieldRow label="路径" value={PROTOCOL_INFO[protocol].path} copied={copied === "sdk-path"} onCopy={() => void copyWithKey("sdk-path", () => PROTOCOL_INFO[protocol].path)} />
                <ConfigBlock
                  title={lang === "curl" ? shellLabel() : lang === "python" ? "python" : "javascript"}
                  format={LANG_LABEL[lang]}
                  code={sdkSnippet(lang, protocol, endpoint, keyForText, model)}
                  copied={copied === "sdk"}
                  onCopy={() => void copyWithKey("sdk", (k) => sdkSnippet(lang, protocol, endpoint, k, model))}
                />
              </>
            ) : null}

          </div>
        </div>
      </ConnectSection>

      <ConnectSection title="连接测试">
        <div className="card card-flush cfg connect-test">
          <div className="cfg-test">
            <input
              className="input"
              value={prompt}
              onChange={(e) => setPrompt(e.target.value)}
              disabled={busy}
              placeholder="测试消息"
              onKeyDown={(e) => {
                if (e.key === "Enter") void test();
              }}
            />
            <button type="button" className="btn btn-primary" disabled={!canTest} onClick={() => void test()} title={canTest ? undefined : "网关没开"}>
              <ShellIcon name="play" size={13} />
              {busy ? "测试中…" : "测试连接"}
            </button>
          </div>
          {run ? (
            <div style={{ padding: "0 18px 16px" }}>
              <TryResult run={run} stats={stats} busy={busy} />
            </div>
          ) : null}
        </div>
      </ConnectSection>
    </div>
  );
}

/* ── 一键接入 ─────────────────────────────────────────────────────────────── */

type Done = { kind: "applied"; r: ConnectApplied } | { kind: "reverted"; r: ConnectReverted };

/**
 * 一键接入的现状和两个动作。
 *
 * 桌面应用就在这台机器上，让人把 JSON 抄回 `~/.claude/settings.json` 是把最容易错的一步留给了人。
 * Rust 侧读现有文件、只改我们那几个键、先备份再写；钥匙不经过前端。
 *
 * 界面上只剩配置卡右上角一个键：没接 → 一键接入；接在别处 → 还是一键接入（原文件会备份）；
 * 已经接在网关上 → 一枚「已接入」加一个撤销。旧版在参数下面横着一整条状态带，
 * 写的是文件路径、模型、几小时前写入 —— 前两样下面的代码块本来就写着，那条带子只是
 * 把一张卡切成了三段灰。真正要说的话（写完了、出错了、模型改了、文件现在指向别处）
 * 交给 [`ApplyNote`]，有话才出一行。
 *
 * 没有配置文件的工具（Cline / SDK / cursor-agent）不查也不写：`supported` 为假时整套不出现。
 */
function useApply(tool: Tool, model: string) {
  const supported = tool === "claude" || tool === "codex" || tool === "opencode" || tool === "grok";
  const [state, setState] = useState<ClientState | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [done, setDone] = useState<Done | null>(null);

  const load = useCallback(async () => {
    if (!supported) {
      setState(null);
      return;
    }
    try {
      setState(await connectApi.inspect(tool as ConnectTool));
    } catch (e) {
      setError(errorText(e));
    }
  }, [tool, supported]);

  useEffect(() => {
    setDone(null);
    setError(null);
    void load();
  }, [load]);

  const act = useCallback(
    async (run: () => Promise<Done>) => {
      setBusy(true);
      setError(null);
      setDone(null);
      try {
        setDone(await run());
        await load();
      } catch (e) {
        setError(errorText(e));
      } finally {
        setBusy(false);
      }
    },
    [load],
  );

  const apply = useCallback(() => act(async () => ({ kind: "applied", r: await connectApi.apply(tool as ConnectTool, clientModelId(model)) })), [act, tool, model]);

  const revert = useCallback(() => act(async () => ({ kind: "reverted", r: await connectApi.revert(tool as ConnectTool) })), [act, tool]);

  const onThis = state?.pointsTo === "local";
  return {
    supported,
    state,
    busy,
    error,
    done,
    apply,
    revert,
    onThis,
    modelDiffers: Boolean(onThis && state?.model != null && state.model !== clientModelId(model)),
  };
}

/** 配置卡上那行注解：有话才出，没话不占位。 */
function ApplyNote({
  ap,
  blocked,
  client,
  model,
  gatewayRunning,
  onGoGateway,
}: {
  ap: ReturnType<typeof useApply>;
  blocked: string | null;
  client: string;
  model: string;
  gatewayRunning: boolean;
  onGoGateway: () => void;
}) {
  if (!ap.supported) return null;

  let tone: "ok" | "warn" | "bad" | "flat" = "flat";
  let body: ReactNode = null;

  if (ap.error) {
    tone = "bad";
    body = ap.error;
  } else if (ap.done?.kind === "applied") {
    const r = ap.done.r;
    tone = "ok";
    body = (
      <>
        已写入 {r.files.map((f) => f.path.split(/[\\/]/).pop()).join("、")}
        {r.files.some((f) => f.backup) ? "（原文件已备份）" : ""}。重开 {client} 生效
        {!gatewayRunning ? (
          <>
            ；本地网关还没开，
            <button type="button" className="linkish" onClick={onGoGateway}>
              去开启 →
            </button>
          </>
        ) : (
          "。"
        )}
      </>
    );
  } else if (ap.done?.kind === "reverted") {
    const r = ap.done.r;
    body = (
      <>
        已撤销：
        {r.restored.length ? `还原 ${r.restored.length} 个文件` : null}
        {r.removed.length ? `${r.restored.length ? "、" : ""}删除 ${r.removed.length} 个我们建的文件` : null}
        {r.stripped.length ? `${r.restored.length || r.removed.length ? "、" : ""}从 ${r.stripped.length} 个文件里去掉了我们的配置` : null}。
      </>
    );
  } else if (blocked) {
    tone = "warn";
    body = blocked;
  } else if (ap.modelDiffers) {
    tone = "warn";
    body = (
      <>
        配置文件里还是 <code className="mono">{ap.state?.model}</code>，重新写入才换成 <code className="mono">{clientModelId(model)}</code>。
      </>
    );
  } else if (ap.state && !ap.onThis && ap.state.pointsTo === "other") {
    tone = "warn";
    body = <>这份配置现在指向{ap.state.baseUrl ? <code className="mono">{ap.state.baseUrl}</code> : "别处"}，接入会改写它（原文件先备份）。</>;
  }

  if (!body) return null;
  return (
    <p className={`cfg-note is-${tone}`}>
      <i aria-hidden />
      <span>{body}</span>
    </p>
  );
}

/* ── 小件 ─────────────────────────────────────────────────────────────────── */

/** 工具卡上的标：有可信官方标的用厂商标，其余用一个字符。 */
const TOOL_VENDOR: Partial<Record<Tool, "anthropic" | "openai" | "cursor" | "xai">> = {
  claude: "anthropic",
  codex: "openai",
  grok: "xai",
};

function ConnectSection({ title, children }: { title: string; children: ReactNode }) {
  return (
    <section className="connect-section">
      <h2 className="connect-section-title">{title}</h2>
      <div className="connect-section-body">{children}</div>
    </section>
  );
}

function KeyActions({
  visible,
  disabled,
  onReveal,
  onHide,
  onCopy,
  copied,
}: {
  visible: boolean;
  disabled?: boolean;
  onReveal: () => void;
  onHide: () => void;
  onCopy: () => void;
  copied: boolean;
}) {
  return (
    <span className="row" style={{ gap: 2 }}>
      <button type="button" className="btn btn-sm btn-icon btn-quiet" disabled={disabled} onClick={visible ? onHide : onReveal} aria-label={visible ? "隐藏" : "显示"}>
        <Icon name={visible ? "eyeOff" : "eye"} size={13} />
      </button>
      <button type="button" className={`btn btn-sm btn-icon btn-quiet${copied ? " is-done" : ""}`} disabled={disabled} onClick={onCopy} aria-label="复制">
        <Icon name={copied ? "check" : "copy"} size={13} />
      </button>
    </span>
  );
}

/** 模型框下面那一行：网关认不认这个名字、会走哪条通道。 */
function ModelHint({
  id,
  local,
  routedTo,
  known,
}: {
  id: string;
  local: boolean;
  /** 这个名字实际会走的那条通道（和当前选的不是同一条时才给）。 */
  routedTo?: string;
  known: boolean;
}) {
  if (!id.trim() || !known) return null;
  if (routedTo) {
    return (
      <span className="mhint muted">
        <span className="mono">{id}</span> 会走 {routedTo} 通道——地址和口令一样，只是扣的是那边的号。
      </span>
    );
  }
  if (!local) {
    return (
      <span className="mhint muted">
        目录里没有 <span className="mono">{id}</span>。不带通道前缀时走默认通道；写成 通道/名称 则强制那条。
      </span>
    );
  }
  return null;
}

function ConfigBlock({
  title,
  format,
  code,
  copied,
  onCopy,
}: {
  title: string;
  format: string;
  code: string;
  copied: boolean;
  onCopy: () => void;
}) {
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
        <button type="button" className={`btn btn-sm btn-quiet codeblock-copy${copied ? " is-done" : ""}`} onClick={onCopy} title="复制的是带真钥匙的版本">
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
