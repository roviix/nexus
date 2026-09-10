/**
 * 账号 · ChatGPT —— 账号页的第二个平台页签。
 *
 * 用用户自己的 ChatGPT 订阅（Plus / Pro / Team）跑 Codex 模型：Codex CLI、Claude Code、OpenAI SDK
 * 指到本机网关，凭证只在这台机器上，请求从这台机器直接到 chatgpt.com。和 Cursor 的号是两队人：
 * 一个 GPT 请求先看这里有没有号，有就走 ChatGPT，没有才走 Cursor 的号。
 *
 * 进来有三条路，都不用手抄 token：
 *  1. **授权登录** —— 桌面端就在用户机器上，Codex 的回调地址（localhost:1455）我们自己接得住，
 *     浏览器里点完同意就自动完成；1455 被占（`codex login` 在跑）时退回贴地址。
 *  2. **从本机 Codex CLI 导入** —— 读 `~/.codex/auth.json`，一步。
 *  3. **粘贴** —— auth.json 原文 / `access----refresh` / 单个 refresh token。
 *
 * 加进来的号默认就在网关的队里（它在这个应用里只有这一个用途），想临时摘掉就关开关，比删了重授权轻。
 * 「正在用 / 耗尽 / 冷却」这些接力状态来自网关；网关没开时只显示号本身。
 */
import { useCallback, useEffect, useMemo, useRef, useState, type ReactNode } from "react";
import { laneOf } from "../../gateway/channels";
import { chatgpt, gateway, onChatGptLogin } from "../../ipc/api";
import type {
  ChatGptAccount,
  ChatGptLoginHandle,
  ChatGptLoginState,
  ChatGptManifestModel,
  ChatGptUsage,
  GatewayCandidate,
  GatewayStatus,
} from "../../ipc/types";
import { go, type Route } from "../../shell/nav";
import { timeAgo, timeUntil } from "../../ui/format";
import { Banner, CopyButton, Empty, ErrorNote, Gauge, Icon, Modal, Switch, Tag } from "../../ui/primitives";
import { labelOf, laneBadge, planClass, windowIsFull, windowLabel } from "./chatgpt";

export function ChatGptAccounts({ tabs, onGo }: { tabs: ReactNode; onGo: (r: Route) => void }) {
  const [accounts, setAccounts] = useState<ChatGptAccount[] | null>(null);
  const [manifest, setManifest] = useState<ChatGptManifestModel[]>([]);
  const [status, setStatus] = useState<GatewayStatus | null>(null);
  const [error, setError] = useState<unknown>(null);
  const [working, setWorking] = useState(false);
  const [adding, setAdding] = useState(false);

  const reload = useCallback(async () => {
    try {
      const [list, models] = await Promise.all([chatgpt.list(), chatgpt.models()]);
      setAccounts(list);
      setManifest(models);
      setError(null);
    } catch (e) {
      setError(e);
    }
    // 网关状态只为拿接力位置；网关模块坏了不该把账号列表一起拖倒。
    try {
      setStatus(await gateway.status());
    } catch {
      setStatus(null);
    }
  }, []);

  useEffect(() => {
    void reload();
  }, [reload]);

  // 网关开着时接力状态会随请求变，每 5 秒跟一次。
  useEffect(() => {
    if (!status?.running) return;
    const t = window.setInterval(() => void reload(), 5000);
    return () => window.clearInterval(t);
  }, [status?.running, reload]);

  const laneByLabel = useMemo(() => {
    const m = new Map<string, GatewayCandidate>();
    for (const c of laneOf(status, "chatgpt").candidates) m.set(c.label.toLowerCase(), c);
    return m;
  }, [status]);

  async function run(action: () => Promise<unknown>) {
    setWorking(true);
    setError(null);
    try {
      await action();
      await reload();
    } catch (e) {
      setError(e);
    } finally {
      setWorking(false);
    }
  }

  async function remove(a: ChatGptAccount) {
    const current = laneByLabel.get(labelOf(a).toLowerCase())?.state.kind === "current";
    const warn = current ? "它正在被网关使用，进行中的对话会换号并丢上游缓存。" : "";
    if (!window.confirm(`删除 ${labelOf(a)}？本机保存的凭证一起删除。${warn}`)) return;
    await run(() => chatgpt.remove(a.id));
  }

  const list = accounts ?? [];
  const active = list.filter((a) => a.enabled && a.status === "active" && a.hasRefresh).length;
  const disabled = working;

  return (
    <>
      <div className="page-head acct-head">
        {tabs}
        <div className="row">
          {/* 和 Cursor 页签一样：次要动作只留图标，标题行上唯一带字的是主动作。 */}
          {active > 0 ? (
            <button
              type="button"
              className="btn btn-icon"
              disabled={disabled}
              onClick={() => void run(() => chatgpt.refreshModels())}
              data-tip={
                manifest.length > 0
                  ? `重拉上游模型目录（现在 ${manifest.length} 个）`
                  : "拉取上游模型目录（还没拉过，网关先用内置清单）"
              }
              aria-label="拉取模型目录"
            >
              <Icon name="layers" size={15} />
            </button>
          ) : null}
          {list.length > 0 ? (
            <button
              type="button"
              className="btn btn-icon"
              disabled={disabled}
              onClick={() => void run(() => chatgpt.resetLane())}
              data-tip="重置接力状态：清掉耗尽 / 冷却记录"
              aria-label="重置接力状态"
            >
              <Icon name="refresh" size={15} className={working ? "is-spinning" : undefined} />
            </button>
          ) : null}
          <button type="button" className="btn btn-primary" disabled={disabled} onClick={() => setAdding(true)}>
            <Icon name="plus" size={14} />
            添加账号
          </button>
        </div>
      </div>

      <ErrorNote error={error} onRetry={() => void reload()} />

      {list.length > 0 && status && !status.running ? (
        <div style={{ marginBottom: 12 }}>
          <Banner
            tone="default"
            title="本地网关没在跑，这些号此刻没人用。"
            hint="ChatGPT 的号只有一个用途：给本机网关跑 GPT / Codex 模型。开了网关，Codex CLI、Claude Code 指到它就能用。"
            action={
              <button type="button" className="btn btn-sm" onClick={() => onGo(go("gateway"))}>
                去本地网关
              </button>
            }
          />
        </div>
      ) : null}

      {accounts === null ? (
        <div className="stack" style={{ gap: 10 }}>
          <div className="skeleton" style={{ height: 96 }} />
          <div className="skeleton" style={{ height: 96 }} />
        </div>
      ) : list.length === 0 ? (
        <Empty
          title="还没有 ChatGPT 账号"
          action={
            <button type="button" className="btn btn-primary" onClick={() => setAdding(true)}>
              <Icon name="plus" size={14} />
              添加 ChatGPT 账号
            </button>
          }
        >
          Plus / Pro / Team 订阅都行。加进来后 Codex CLI、Claude Code 指到本机网关就能用它跑 gpt-5.x：
          凭证只在这台电脑上，请求从这里直接到 chatgpt.com。
        </Empty>
      ) : (
        <div className="card">
          <div className="row" style={{ gap: 8, alignItems: "baseline", marginBottom: 12 }}>
            <strong>{list.length} 个号</strong>
            <span className="faint tiny">{active} 个可接 · GPT / Codex 模型优先走这里</span>
          </div>
          <div className="list">
            {list.map((a) => (
              <AccountRow
                key={a.id}
                account={a}
                lane={laneByLabel.get(labelOf(a).toLowerCase()) ?? null}
                disabled={disabled}
                onToggle={(on) => void run(() => chatgpt.setEnabled(a.id, on))}
                onUse={() => void run(() => chatgpt.setCurrent(labelOf(a)))}
                onRefresh={() => void run(() => chatgpt.refreshUsage(a.id))}
                onRelogin={() => setAdding(true)}
                onRemove={() => void remove(a)}
              />
            ))}
          </div>
        </div>
      )}

      {adding ? (
        <AddModal
          onClose={() => setAdding(false)}
          onAdded={async () => {
            setAdding(false);
            await reload();
          }}
        />
      ) : null}
    </>
  );
}

function planTag(plan: string | null) {
  const cls = planClass(plan);
  return cls && plan ? <span className={cls}>{plan}</span> : null;
}

function laneTag(c: GatewayCandidate | null, enabled: boolean) {
  const b = laneBadge(c, enabled);
  if (!b) return null;
  return b.tone === "default" ? <Tag>{b.text}</Tag> : <Tag tone={b.tone}>{b.text}</Tag>;
}

function AccountRow({
  account: a,
  lane,
  disabled,
  onToggle,
  onUse,
  onRefresh,
  onRelogin,
  onRemove,
}: {
  account: ChatGptAccount;
  lane: GatewayCandidate | null;
  disabled: boolean;
  onToggle: (on: boolean) => void;
  onUse: () => void;
  onRefresh: () => void;
  onRelogin: () => void;
  onRemove: () => void;
}) {
  const isCurrent = lane?.state.kind === "current";
  const needsLogin = a.status !== "active" || !a.hasRefresh;
  const u = a.usage;
  return (
    <div className={isCurrent ? "list-row is-current" : "list-row"} style={{ alignItems: "flex-start" }}>
      <div className="grow stack" style={{ gap: 8, minWidth: 0 }}>
        <div className="row" style={{ gap: 8, minWidth: 0 }}>
          <span className="mono selectable truncate" style={{ fontSize: 13 }}>
            {labelOf(a)}
          </span>
          {planTag(a.planType)}
          {laneTag(lane, a.enabled)}
          {a.status === "dead" ? (
            <Tag tone="bad">已停用</Tag>
          ) : needsLogin ? (
            <Tag tone="warn">需要重新授权</Tag>
          ) : null}
        </div>

        {u ? (
          <div className="row" style={{ gap: 14, alignItems: "flex-start" }}>
            <div style={{ flex: 1, minWidth: 0 }}>
              <UsageGauge window={u.primary} fallback="5 小时" />
            </div>
            <div style={{ flex: 1, minWidth: 0 }}>
              <UsageGauge window={u.secondary} fallback="7 天" />
            </div>
          </div>
        ) : (
          <span className="faint tiny">{needsLogin ? "授权后才有额度数据" : "还没有额度数据 · 跑一次请求就有"}</span>
        )}

        <div className="row" style={{ gap: 10, flexWrap: "wrap" }}>
          {a.lastError ? (
            <span className="acct-problem" title={a.lastError}>
              {a.lastError.slice(0, 90)}
            </span>
          ) : null}
          {a.accessExpiresAt && !needsLogin ? (
            <span className="faint tiny">凭证 {timeUntil(new Date(a.accessExpiresAt).getTime())} 后自动续期</span>
          ) : null}
          {u ? <span className="faint tiny">额度 {timeAgo(u.checkedAt)} 更新</span> : null}
          {a.note ? <span className="faint tiny">{a.note}</span> : null}
        </div>
      </div>

      <div className="row" style={{ gap: 6, flexShrink: 0 }}>
        <span className="acct-hover row" style={{ gap: 2 }}>
          <button type="button" className="btn btn-sm btn-icon btn-quiet" disabled={disabled || needsLogin} onClick={onRefresh} title="现在查一次额度" aria-label="查额度">
            <Icon name="refresh" size={13} />
          </button>
          <button type="button" className="btn btn-sm btn-icon btn-soft btn-danger" disabled={disabled} onClick={onRemove} title="删除账号（连凭证）" aria-label="删除">
            <Icon name="trash" size={13} />
          </button>
        </span>
        {needsLogin ? (
          <button type="button" className="btn btn-sm" disabled={disabled} onClick={onRelogin}>
            重新授权
          </button>
        ) : (
          <button type="button" className="btn btn-sm" disabled={disabled || isCurrent || !a.enabled} onClick={onUse}>
            {isCurrent ? "使用中" : "用这个"}
          </button>
        )}
        <Switch checked={a.enabled} disabled={disabled} label={a.enabled ? "暂停这个号" : "加入网关"} onChange={onToggle} />
      </div>
    </div>
  );
}

function UsageGauge({ window: w, fallback }: { window: ChatGptUsage["primary"]; fallback: string }) {
  const label = windowLabel(w?.windowMinutes, fallback);
  const reset = w?.resetAtMs != null && windowIsFull(w) ? `${timeUntil(w.resetAtMs)}后重置` : null;
  return (
    <Gauge
      label={label}
      percent={w?.usedPercent ?? null}
      compact
      note={reset ? <span className="acct-problem">{reset}</span> : undefined}
      title={w?.resetAtMs != null ? `窗口 ${timeUntil(w.resetAtMs)} 后重置` : undefined}
    />
  );
}

// ── 添加 ─────────────────────────────────────────────────────────────────────

type Mode = "oauth" | "cli" | "paste";

/**
 * 三条进来的路放一个弹窗里。默认「授权登录」：一键、最像官方 `codex login`。
 * 授权链接打开后这里只显示等待；回调自动到达就关弹窗。1455 被占时才露出「贴地址」的输入框。
 */
function AddModal({ onClose, onAdded }: { onClose: () => void; onAdded: () => Promise<void> }) {
  const [mode, setMode] = useState<Mode>("oauth");
  const [error, setError] = useState<unknown>(null);
  const [working, setWorking] = useState(false);
  const [handle, setHandle] = useState<ChatGptLoginHandle | null>(null);
  const [waitSecs, setWaitSecs] = useState(0);
  const [callback, setCallback] = useState("");
  const [paste, setPaste] = useState("");
  const [note, setNote] = useState("");
  const handleRef = useRef<ChatGptLoginHandle | null>(null);
  handleRef.current = handle;

  useEffect(() => {
    let unlisten: (() => void) | null = null;
    void onChatGptLogin((st: ChatGptLoginState) => {
      const h = handleRef.current;
      if (!h || st.sessionId !== h.sessionId) return;
      if (st.state === "waiting") setWaitSecs(st.elapsedSecs);
      if (st.state === "succeeded") void onAdded();
      if (st.state === "failed") {
        setError({ code: "upstream", message: st.message, hint: st.hint ?? undefined });
        setHandle(null);
      }
      if (st.state === "cancelled") setHandle(null);
    }).then((u) => {
      unlisten = u;
    });
    return () => {
      unlisten?.();
      const h = handleRef.current;
      if (h?.callbackListening) void chatgpt.loginCancel(h.sessionId);
    };
  }, [onAdded]);

  async function go(action: () => Promise<unknown>) {
    setWorking(true);
    setError(null);
    try {
      await action();
    } catch (e) {
      setError(e);
    } finally {
      setWorking(false);
    }
  }

  return (
    <Modal
      title="添加 ChatGPT 账号"
      subtitle="Plus / Pro / Team 订阅。凭证只存在这台电脑上。"
      onClose={onClose}
      footer={
        <>
          <button type="button" className="btn" onClick={onClose}>
            {handle ? "关闭" : "取消"}
          </button>
          {mode === "oauth" && !handle ? (
            <button type="button" className="btn btn-primary" disabled={working} onClick={() => void go(async () => setHandle(await chatgpt.loginStart(note.trim() || undefined)))}>
              <Icon name="external" size={13} />
              打开浏览器授权
            </button>
          ) : null}
          {mode === "oauth" && handle && !handle.callbackListening ? (
            <button type="button" className="btn btn-primary" disabled={working || !callback.trim()} onClick={() => void go(async () => {
              await chatgpt.loginComplete(handle.sessionId, callback);
              await onAdded();
            })}>
              完成授权
            </button>
          ) : null}
          {mode === "cli" ? (
            <button type="button" className="btn btn-primary" disabled={working} onClick={() => void go(async () => {
              await chatgpt.importCodexCli();
              await onAdded();
            })}>
              <Icon name="download" size={13} />
              从 Codex CLI 导入
            </button>
          ) : null}
          {mode === "paste" ? (
            <button type="button" className="btn btn-primary" disabled={working || !paste.trim()} onClick={() => void go(async () => {
              await chatgpt.importText(paste, note.trim() || undefined);
              await onAdded();
            })}>
              导入
            </button>
          ) : null}
        </>
      }
    >
      <div className="stack" style={{ gap: 12 }}>
        <div className="gwset-options" role="radiogroup" aria-label="添加方式">
          {(
            [
              ["oauth", "授权登录"],
              ["cli", "从本机 Codex CLI"],
              ["paste", "粘贴 token"],
            ] as Array<[Mode, string]>
          ).map(([id, label]) => (
            <button
              key={id}
              type="button"
              role="radio"
              aria-checked={mode === id}
              className={`gwset-option${mode === id ? " is-active" : ""}`}
              disabled={working || !!handle}
              onClick={() => setMode(id)}
            >
              {label}
            </button>
          ))}
        </div>

        <ErrorNote error={error} />

        {mode === "oauth" ? (
          handle ? (
            <div className="stack" style={{ gap: 10 }}>
              {handle.callbackListening ? (
                <>
                  <p className="muted" style={{ margin: 0 }}>
                    浏览器已打开。用 ChatGPT 账号登录并同意后会自动回到这里
                    {waitSecs > 0 ? <span className="faint tiny">（已等待 {waitSecs} 秒）</span> : null}。
                  </p>
                  <div className="row" style={{ gap: 6 }}>
                    <span className="faint tiny">没自动打开？</span>
                    <CopyButton value={handle.authorizeUrl} label="复制授权链接" />
                  </div>
                </>
              ) : (
                <>
                  <p className="muted" style={{ margin: 0 }}>
                    本机 1455 端口被占着（多半是终端里的 <code className="mono">codex login</code> 在跑），没法自动收回调。
                    在浏览器里完成登录后，页面会跳到一个打不开的 <code className="mono">localhost:1455</code> 地址——把地址栏整条复制过来：
                  </p>
                  <input
                    className="input mono"
                    placeholder="http://localhost:1455/auth/callback?code=…&state=…"
                    value={callback}
                    onChange={(e) => setCallback(e.target.value)}
                    spellCheck={false}
                  />
                  <div className="row" style={{ gap: 6 }}>
                    <CopyButton value={handle.authorizeUrl} label="复制授权链接" />
                  </div>
                </>
              )}
            </div>
          ) : (
            <div className="stack" style={{ gap: 10 }}>
              <p className="muted" style={{ margin: 0 }}>
                和 <code className="mono">codex login</code> 走同一条路：在系统浏览器里用 ChatGPT 账号登录并同意，
                授权码会自动回到这里。不需要在别处复制任何东西。
              </p>
              <input className="input" placeholder="备注（可选）" value={note} onChange={(e) => setNote(e.target.value)} />
            </div>
          )
        ) : null}

        {mode === "cli" ? (
          <p className="muted" style={{ margin: 0 }}>
            读取本机 <code className="mono">~/.codex/auth.json</code>（终端里 <code className="mono">codex login</code> 留下的登录态）。
            文件不动，只把凭证收进 Nexus。
          </p>
        ) : null}

        {mode === "paste" ? (
          <div className="stack" style={{ gap: 10 }}>
            <textarea
              className="textarea mono"
              rows={5}
              placeholder={"三种写法都认：\n· ~/.codex/auth.json 的原文\n· access_token----refresh_token\n· 单独一个 refresh token（会先刷一次拿身份）"}
              value={paste}
              onChange={(e) => setPaste(e.target.value)}
              spellCheck={false}
            />
            <input className="input" placeholder="备注（可选）" value={note} onChange={(e) => setNote(e.target.value)} />
          </div>
        ) : null}
      </div>
    </Modal>
  );
}
