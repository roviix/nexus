/**
 * 账号 · ChatGPT —— 账号页的第二个平台页签。
 *
 * 用用户自己的 ChatGPT 订阅（Plus / Pro / Team）跑 Codex 模型：Codex CLI、Claude Code、OpenAI SDK
 * 指到本机网关，凭证只在这台机器上，请求从这台机器直接到 chatgpt.com。写成 `chatgpt/…` 走这里；
 * 把 ChatGPT 设成默认通道后，裸名或不写模型也走这里。空模型优先这个号目录里的
 * gpt-6-astra，没有就 gpt-5.4。官方 ChatGPT 应用不能改接口地址，接不了本机网关。
 *
 * 进来有三条路，都不用手抄 token：
 *  1. **授权登录** —— 桌面端就在用户机器上，Codex 的回调地址（localhost:1455）我们自己接得住，
 *     浏览器里点完同意就自动完成；1455 被占（`codex login` 在跑）时退回贴地址。
 *  2. **从本机 Codex CLI 导入** —— 读 `~/.codex/auth.json`，一步。
 *  3. **粘贴** —— auth.json / sub2api 的 Codex session JSON（数组、多行、带 credentials 包一层）/
 *     `access----refresh` / 单个 refresh token。可以一次贴多个。
 *
 * 加进来的号默认就在网关的队里。列表只负责扫（卡片 + 抽屉，和 Cursor 同一套骨架）；
 * 加入 / 移出本地网关、「用这个」都在抽屉里，不在卡上。
 */
import { useCallback, useEffect, useMemo, useRef, useState, type ReactNode } from "react";
import { AccountCard } from "../../accounts/AccountCard";
import { AccountInspector } from "../../accounts/AccountInspector";
import { createChatGptAccountView } from "../../accounts/model";
import { PoolChips } from "../../accounts/PoolChips";
import { laneOf } from "../../gateway/channels";
import { chatgpt, gateway, onChatGptLogin } from "../../ipc/api";
import type {
  ChatGptAccount,
  ChatGptImportOutcome,
  ChatGptLoginHandle,
  ChatGptLoginState,
  ChatGptManifestModel,
  GatewayCandidate,
  GatewayStatus,
} from "../../ipc/types";
import { Banner, CopyButton, Empty, ErrorNote, Icon, Modal } from "../../ui/primitives";
import { chatgptGatewayMembership, chatgptUsable, importSummary, labelOf } from "./chatgpt";

export function ChatGptAccounts({ tabs }: { tabs: ReactNode }) {
  const [accounts, setAccounts] = useState<ChatGptAccount[] | null>(null);
  const [manifest, setManifest] = useState<ChatGptManifestModel[]>([]);
  const [status, setStatus] = useState<GatewayStatus | null>(null);
  const [error, setError] = useState<unknown>(null);
  const [working, setWorking] = useState(false);
  const [adding, setAdding] = useState(false);
  const [openId, setOpenId] = useState<string | null>(null);
  const [refreshing, setRefreshing] = useState<Set<string>>(() => new Set());

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

  const list = accounts ?? [];
  const active = list.filter((a) => a.enabled && chatgptUsable(a)).length;
  const disabled = working;
  const openAccount = list.find((a) => a.id === openId) ?? null;
  const openView = openAccount
    ? createChatGptAccountView({
        account: openAccount,
        lane: laneByLabel.get(labelOf(openAccount).toLowerCase()) ?? null,
      })
    : null;

  function viewOf(a: ChatGptAccount) {
    return createChatGptAccountView({
      account: a,
      lane: laneByLabel.get(labelOf(a).toLowerCase()) ?? null,
    });
  }

  async function refreshOne(id: string) {
    setRefreshing((p) => new Set(p).add(id));
    try {
      await chatgpt.refreshUsage(id);
      setError(null);
      await reload();
    } catch (e) {
      setError(e);
    } finally {
      setRefreshing((p) => {
        const next = new Set(p);
        next.delete(id);
        return next;
      });
    }
  }

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

      {accounts === null ? (
        <div className="accts">
          <div className="skeleton" style={{ height: 164 }} />
          <div className="skeleton" style={{ height: 164 }} />
          <div className="skeleton" style={{ height: 164 }} />
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
          Plus / Pro / Team 订阅都行。加进来后，在网关页把 ChatGPT 设为默认，Codex CLI、Claude Code、
          OpenAI SDK 指到本机网关就能用：凭证只在这台电脑上，请求从这里直接到 chatgpt.com。
        </Empty>
      ) : (
        <div className="accts">
          {list.map((a) => {
            const view = viewOf(a);
            const refreshingAccount = refreshing.has(a.id);
            return (
              <AccountCard
                key={a.id}
                view={view}
                highlighted={a.id === openId}
                onOpen={() => setOpenId(a.id)}
                badges={<PoolChips membership={{ switcher: null, gateway: chatgptGatewayMembership(a, view.lane) }} />}
                actions={
                  chatgptUsable(a) ? (
                    <button
                      type="button"
                      className="ibtn"
                      disabled={refreshingAccount}
                      onClick={() => void refreshOne(a.id)}
                      aria-label="刷新用量"
                    >
                      <Icon name="refresh" size={13} className={refreshingAccount ? "is-spinning" : undefined} />
                    </button>
                  ) : (
                    <button type="button" className="btn btn-sm btn-soft" onClick={() => setAdding(true)}>
                      授权
                    </button>
                  )
                }
              />
            );
          })}
        </div>
      )}

      {openView ? (
        <AccountInspector
          view={openView}
          onClose={() => setOpenId(null)}
          onChanged={reload}
          onReauth={() => setAdding(true)}
        />
      ) : null}

      {adding ? (
        <AddModal
          onClose={() => setAdding(false)}
          onAdded={async () => {
            setAdding(false);
            await reload();
          }}
          onReload={reload}
        />
      ) : null}
    </>
  );
}

// ── 添加 ─────────────────────────────────────────────────────────────────────

type Mode = "oauth" | "cli" | "paste";

/**
 * 三条进来的路放一个弹窗里。默认「授权登录」：一键、最像官方 `codex login`。
 * 授权链接打开后这里只显示等待；回调自动到达就关弹窗。1455 被占时才露出「贴地址」的输入框。
 */
function AddModal({
  onClose,
  onAdded,
  onReload,
}: {
  onClose: () => void;
  onAdded: () => Promise<void>;
  onReload: () => Promise<void>;
}) {
  const [mode, setMode] = useState<Mode>("oauth");
  const [error, setError] = useState<unknown>(null);
  const [working, setWorking] = useState(false);
  const [handle, setHandle] = useState<ChatGptLoginHandle | null>(null);
  const [waitSecs, setWaitSecs] = useState(0);
  const [callback, setCallback] = useState("");
  const [paste, setPaste] = useState("");
  const [note, setNote] = useState("");
  const [outcome, setOutcome] = useState<ChatGptImportOutcome | null>(null);
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
              const next = await chatgpt.importText(paste, note.trim() || undefined);
              setOutcome(next);
              if (next.failed === 0) await onAdded();
              else await onReload();
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
              rows={6}
              placeholder={"可一次贴多个，这些写法都认：\n· ~/.codex/auth.json 原文\n· sub2api 的 Codex session JSON（数组 / 多行 / 带 credentials）\n· access_token----refresh_token\n· 单独一个 refresh token（会先刷一次拿身份）"}
              value={paste}
              onChange={(e) => {
                setPaste(e.target.value);
                setOutcome(null);
              }}
              spellCheck={false}
            />
            <input className="input" placeholder="备注（可选）" value={note} onChange={(e) => setNote(e.target.value)} />
            {outcome ? (
              <Banner
                tone={outcome.failed > 0 ? "warn" : "ok"}
                title={importSummary(outcome)}
                hint={outcome.errors.length ? outcome.errors.slice(0, 4).join(" · ") : undefined}
              />
            ) : null}
          </div>
        ) : null}
      </div>
    </Modal>
  );
}
