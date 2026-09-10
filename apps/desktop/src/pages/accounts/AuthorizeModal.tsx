/**
 * 授权取 token（ARCHITECTURE §5.1）。
 *
 * 弹窗里先把登录时要抄的东西摆齐 —— 邮箱、Cursor 密码、验证码 —— 每一样都带复制，
 * 然后一个按钮开隐私窗口。用户在浏览器里登录，应用后台轮询收 token。
 *
 * 授权是后台任务：弹窗关掉之后 token 才到是常事，列表那边靠 `oauth://state` 事件自己刷新。
 */
import { useEffect, useRef, useState } from "react";
import { accounts } from "../../ipc/api";
import { onOauthState } from "../../ipc/api";
import type { Account, OauthStarted, OauthState } from "../../ipc/types";
import { Banner, CopyButton, ErrorNote, Icon, Modal, Spinner } from "../../ui/primitives";

export function AuthorizeModal({ account, onClose }: { account: Account; onClose: () => void }) {
  const [state, setState] = useState<OauthState | null>(null);
  const [started, setStarted] = useState<OauthStarted | null>(null);
  const [starting, setStarting] = useState(false);
  const [error, setError] = useState<unknown>(null);
  const [password, setPassword] = useState<string | null>(null);
  const myUuid = useRef<string | null>(null);

  // 弹窗关了授权还在跑，所以同时有几场在发事件是常态。
  // **发起之前一条都不认** —— 否则上一场的成功事件会落到这一场的界面上。
  useEffect(() => {
    const off = onOauthState((s) => {
      if (myUuid.current === null || s.uuid !== myUuid.current) return;
      setState(s);
    });
    return () => void off.then((fn) => fn());
  }, []);

  async function start() {
    setStarting(true);
    setError(null);
    setState(null);
    try {
      const session = await accounts.startOauth(account.email);
      myUuid.current = session.uuid;
      setStarted(session);
    } catch (err) {
      setError(err);
    } finally {
      setStarting(false);
    }
  }

  async function revealPassword() {
    try {
      setPassword(await accounts.revealSecret(account.id, "cursorPassword"));
      setError(null);
    } catch (err) {
      setError(err);
    }
  }

  const done = state?.state === "succeeded";
  const failed = state?.state === "failed";

  return (
    <Modal
      title={account.hasRefresh ? "重新授权" : "授权登录"}
      subtitle={`${account.email} · 在隐私窗口里登录，收到 token 前这个窗口可以关掉。`}
      onClose={onClose}
      footer={
        <>
          {started && !done && !failed ? (
            <button
              type="button"
              className="btn"
              onClick={() => {
                void accounts.cancelOauth(started.uuid);
                onClose();
              }}
            >
              取消授权
            </button>
          ) : null}
          <button type="button" className={done ? "btn btn-primary" : "btn"} onClick={onClose}>
            {done ? "完成" : "关闭"}
          </button>
        </>
      }
    >
      <div className="stack" style={{ gap: 14 }}>
        <ErrorNote error={error} />

        <section className="sheet">
          <div className="sheet-cap">登录时会用到</div>
          <div className="kv">
            <div className="kv-row">
              <span className="kv-k">邮箱</span>
              <span className="kv-v">
                <span className="selectable truncate">{account.email}</span>
                <CopyButton value={account.email} icon />
              </span>
            </div>
            <div className="kv-row">
              <span className="kv-k">Cursor 密码</span>
              {!account.hasPassword ? (
                <span className="kv-v faint">未保存 · 用邮箱验证码登录</span>
              ) : password != null ? (
                <span className="kv-v">
                  <code className="secret selectable truncate" title={password}>
                    {password}
                  </code>
                  <CopyButton value={password} icon />
                  <button
                    type="button"
                    className="btn btn-sm btn-icon btn-quiet"
                    onClick={() => setPassword(null)}
                    title="隐藏"
                    aria-label="隐藏"
                  >
                    <Icon name="eyeOff" size={13} />
                  </button>
                </span>
              ) : (
                <span className="kv-v">
                  <span className="secret is-masked">••••••••••••</span>
                  <button
                    type="button"
                    className="btn btn-sm btn-icon btn-quiet"
                    onClick={() => void revealPassword()}
                    title="显示"
                    aria-label="显示"
                  >
                    <Icon name="eye" size={13} />
                  </button>
                </span>
              )}
            </div>
            <div className="kv-row">
              <span className="kv-k">验证码</span>
              <span className="kv-v faint tiny">浏览器要验证码时到邮箱里查看</span>
            </div>
          </div>
        </section>

        {!started ? (
          <button
            type="button"
            className="btn btn-primary btn-lg"
            disabled={starting}
            onClick={() => void start()}
          >
            {starting ? <Spinner /> : <Icon name="external" size={15} />}
            打开隐私窗口登录
          </button>
        ) : done ? (
          <Banner tone="ok" title="已收到 token，这个号可用了。" />
        ) : failed ? (
          <Banner
            tone="bad"
            title={state.message}
            action={
              <button type="button" className="btn btn-sm" onClick={() => void start()}>
                重试
              </button>
            }
          />
        ) : (
          <Banner
            title={
              state?.state === "waiting"
                ? `等待隐私窗口里完成登录…（${state.elapsedSecs} 秒）`
                : "已打开隐私窗口，等待登录…"
            }
            hint="没弹出的话，复制链接自己用无痕窗口打开。"
            action={<CopyButton value={started.loginUrl} label="复制链接" />}
          />
        )}
      </div>
    </Modal>
  );
}
