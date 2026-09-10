/**
 * 账号抽屉的「Grok Bot」页：Bot 通道用**这个** Cursor 账号的 Grok 额度。
 *
 * 本机只落一份凭证，Sand 补丁 / 网关每一发现读它：切号 = 换这份文件，不用重装、不用重启 Cursor。
 *  - 主路：用账号库里它的 refresh token 换（不需要 Grok Bot 客户端）。
 *  - 备路：从 Grok Bot 客户端登着的号生成——只给没 refresh token 的账号兜底。
 *
 * 「用这个号」默认顺带把 Cursor 也切到它（记在 localStorage，关掉就只换凭证）。
 * 读 Grok Bot 客户端数据要解钥匙串，首次弹系统授权，所以「识别」是显式动作。
 */
import { useCallback, useEffect, useState } from "react";
import { accounts, app as appApi, grokbot, switcher } from "../../ipc/api";
import type { Account, GrokBotIdentity, GrokBotStatus } from "../../ipc/types";
import { canQueryUsage } from "../../ui/accounts";
import { timeUntil } from "../../ui/format";
import { ErrorNote, Icon, Spinner, Tag } from "../../ui/primitives";

const SWITCH_TOO_KEY = "nexus.grokbot.switchCursorToo";

function readSwitchToo(): boolean {
  try {
    return window.localStorage.getItem(SWITCH_TOO_KEY) !== "0";
  } catch {
    return true;
  }
}

type Busy = null | "use" | "identify" | "launch" | "mint" | "renew";

export function GrokBotTab({ account }: { account: Account }) {
  const [st, setSt] = useState<GrokBotStatus | null>(null);
  const [identity, setIdentity] = useState<GrokBotIdentity | null>(null);
  const [cursorEmail, setCursorEmail] = useState<string | null>(null);
  const [cursorRunning, setCursorRunning] = useState(false);
  const [coldSwitch, setColdSwitch] = useState(false);
  const [switchToo, setSwitchToo] = useState(readSwitchToo);
  const [busy, setBusy] = useState<Busy>(null);
  const [error, setError] = useState<unknown>(null);
  const [showClient, setShowClient] = useState(false);

  const reload = useCallback(async () => {
    try {
      const [status, overview, appStatus] = await Promise.all([
        grokbot.status(),
        switcher.overview(),
        appApi.status(),
      ]);
      setSt(status);
      setCursorEmail(overview.current?.email?.toLowerCase() ?? null);
      setCursorRunning(overview.cursorRunning);
      setColdSwitch(appStatus.switchMachineIds);
    } catch (e) {
      setError(e);
    }
  }, []);

  useEffect(() => {
    void reload();
  }, [reload]);

  // 直连 token 约 10 分钟一换；页停留时每 30 秒刷一次倒计时。
  useEffect(() => {
    if (!st?.direct) return;
    const t = window.setInterval(() => void reload(), 30_000);
    return () => window.clearInterval(t);
  }, [st?.direct, reload]);

  useEffect(() => {
    setIdentity(null);
  }, [st?.activeSlot]);

  function toggleSwitchToo(next: boolean) {
    setSwitchToo(next);
    try {
      window.localStorage.setItem(SWITCH_TOO_KEY, next ? "1" : "0");
    } catch {
      /* 记不住就当次有效 */
    }
  }

  async function act(kind: NonNullable<Busy>, fn: () => Promise<unknown>) {
    setBusy(kind);
    setError(null);
    try {
      await fn();
      await reload();
    } catch (e) {
      setError(e);
    } finally {
      setBusy(null);
    }
  }

  if (!st) {
    return (
      <div className="row gap-sm items-center muted text-sm">
        <Spinner />
      </div>
    );
  }

  const mine = account.email.toLowerCase();
  const direct = st.direct;
  const directOk = !!direct && (!direct.expired || direct.canRenew);
  const directOwner = direct?.accountEmail?.toLowerCase() ?? null;
  const directIsMine = directOk && directOwner === mine;
  // 换 Grok 额度只要一把活的 access：仅会话的号有效期内也行。切 Cursor 则必须有 refresh。
  const canUse = canQueryUsage(account) && account.status !== "dead";
  const cursorIsMine = cursorEmail === mine;
  const willSwitch = switchToo && canUse && account.hasRefresh && !cursorIsMine;

  const app = st.app;
  const activeEmail = (identity?.email ?? st.activeEmail)?.toLowerCase() ?? null;
  const clientIsThis = activeEmail === mine;

  async function use() {
    await grokbot.mintForAccount(account.id);
    if (!willSwitch) return;
    // 冷切会退出并重启 Cursor，这一步才值得打断问一句。
    if (
      cursorRunning &&
      coldSwitch &&
      !window.confirm(`切到 ${account.email} 会退出并重启 Cursor，继续？`)
    ) {
      return;
    }
    const profile = await accounts.addToSwitchBook(account.id);
    await switcher.switchTo(profile.id);
  }

  return (
    <section className="stack gap-sm">
      <div className="usedin">
        <div className={`usedin-row${directIsMine ? " is-in is-live" : ""}`}>
          <span className="usedin-ico">
            <Icon name="sand" size={13} />
          </span>
          <span className="usedin-k">Bot 通道</span>
          <span className="usedin-v">
            {directIsMine
              ? direct!.expired
                ? "这个号 · 下一发自动续"
                : `这个号 · ${timeUntil(direct!.expiresAtMs)}续`
              : directOk
                ? directOwner ?? "另一个号"
                : "未设置"}
          </span>
          {directIsMine ? (
            <span className="row" style={{ gap: 6 }}>
              {direct?.canRenew ? (
                <button
                  type="button"
                  className="btn btn-sm btn-quiet"
                  disabled={busy !== null}
                  onClick={() => void act("renew", () => grokbot.renewDirect())}
                >
                  {busy === "renew" ? <Spinner /> : "续一次"}
                </button>
              ) : null}
              <Tag tone="ok">当前</Tag>
            </span>
          ) : (
            <button
              type="button"
              className="btn btn-sm btn-primary"
              disabled={busy !== null || !canUse}
              title={
                canUse
                  ? "换成这个号的 Grok 额度，下一发生效；不用重启 Cursor"
                  : account.status === "dead"
                    ? "这个号已失效"
                    : account.hasAccess
                      ? "session token 已过期，到凭证页粘一份新的"
                      : "需要 refresh token"
              }
              onClick={() => void act("use", use)}
            >
              {busy === "use" ? <Spinner /> : null}
              {willSwitch ? "用这个号并切 Cursor" : "用这个号"}
            </button>
          )}
        </div>
        <div className={`usedin-row${cursorIsMine ? " is-in" : ""}`}>
          <span className="usedin-ico">
            <Icon name="switcher" size={13} />
          </span>
          <span className="usedin-k">Cursor 登录</span>
          <span className="usedin-v">{cursorEmail ?? "未登录"}</span>
          {cursorIsMine ? (
            <Tag tone="ok">当前</Tag>
          ) : !account.hasRefresh ? (
            <span className="faint tiny" title="切 Cursor 需要 refresh token">不可切</span>
          ) : (
            <label className="row items-center tiny muted" style={{ gap: 6, cursor: "pointer" }}>
              <input
                type="checkbox"
                checked={switchToo}
                disabled={busy !== null}
                onChange={(e) => toggleSwitchToo(e.target.checked)}
              />
              同时切 Cursor
            </label>
          )}
        </div>
      </div>

      <ErrorNote error={error} />

      {!canUse || showClient ? (
        <div className="usedin">
          <div className={`usedin-row${clientIsThis ? " is-in" : ""}`}>
            <span className="usedin-ico">
              <Icon name="external" size={13} />
            </span>
            <span className="usedin-k">Grok Bot 客户端</span>
            <span className="usedin-v">
              {!app.installed
                ? "未安装"
                : app.signedIn === false
                  ? "未登录"
                  : activeEmail === null
                    ? "未识别"
                    : clientIsThis
                      ? `这个号${app.running ? "" : " · 未运行"}`
                      : activeEmail}
            </span>
            <span className="row" style={{ gap: 6 }}>
              {app.installed && app.signedIn !== false ? (
                <button
                  type="button"
                  className="btn btn-sm btn-soft"
                  disabled={busy !== null}
                  title="解钥匙串看客户端登着谁；首次会弹系统授权"
                  onClick={() => void act("identify", async () => setIdentity(await grokbot.identify()))}
                >
                  {busy === "identify" ? <Spinner /> : "识别"}
                </button>
              ) : null}
              {clientIsThis ? (
                <button
                  type="button"
                  className="btn btn-sm btn-soft"
                  disabled={busy !== null}
                  title="用客户端登着的这个号生成凭证"
                  onClick={() => void act("mint", () => grokbot.mintDirect())}
                >
                  {busy === "mint" ? <Spinner /> : "生成凭证"}
                </button>
              ) : (
                <button
                  type="button"
                  className="btn btn-sm btn-soft"
                  disabled={busy !== null}
                  onClick={() => void act("launch", () => grokbot.launch())}
                  title={app.installed ? "打开 Grok Bot" : "到 x.ai/bot 下载"}
                >
                  {busy === "launch" ? <Spinner /> : <Icon name="external" size={13} />}
                  打开
                </button>
              )}
            </span>
          </div>
        </div>
      ) : (
        <button
          type="button"
          className="btn btn-sm btn-quiet"
          style={{ alignSelf: "flex-start" }}
          onClick={() => setShowClient(true)}
        >
          从 Grok Bot 客户端取
        </button>
      )}
    </section>
  );
}
