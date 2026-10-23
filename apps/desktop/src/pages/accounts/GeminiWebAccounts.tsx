/**
 * Gemini 网页通道。Cookie 登录 gemini.google.com，和反重力的 OAuth 分开。
 */
import { useCallback, useEffect, useMemo, useState, type ReactNode } from "react";
import { geminiWeb } from "../../ipc/api";
import type { DeviceAccount } from "../../ipc/types";
import type { Route } from "../../shell/nav";
import { Empty, ErrorNote, Icon } from "../../ui/primitives";
import { GatewayCard } from "./GatewayCard";
import { VendorLogo } from "../../relay/VendorLogo";

export function GeminiWebAccounts({ tabs }: { tabs: ReactNode; onGo: (r: Route) => void }) {
  const [list, setList] = useState<DeviceAccount[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<unknown>(null);
  const [cookie, setCookie] = useState("");
  const [email, setEmail] = useState("");
  const [busy, setBusy] = useState(false);
  const [filter, setFilter] = useState<"all" | "active" | "disabled">("all");
  const activeCount = list.filter((a) => a.enabled && a.status === "active").length;
  const disabledCount = list.length - activeCount;

  const filteredList = useMemo(() => {
    if (filter === "active") return list.filter((a) => a.enabled && a.status === "active");
    if (filter === "disabled") return list.filter((a) => !(a.enabled && a.status === "active"));
    return list;
  }, [list, filter]);

  const reload = useCallback(async () => {
    setError(null);
    try {
      setList(await geminiWeb.list());
    } catch (err) {
      setError(err);
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    void reload();
  }, [reload]);

  const add = async () => {
    if (!cookie.trim() || busy) return;
    setBusy(true);
    setError(null);
    try {
      await geminiWeb.importCookie(cookie, email.trim() || undefined);
      setCookie("");
      setEmail("");
      await reload();
    } catch (err) {
      setError(err);
    } finally {
      setBusy(false);
    }
  };

  return (
    <div className="page">
      {tabs}
      <div className="acct-toolbar">
        <p className="faint tiny" style={{ margin: 0 }}>
          这条通道走 Gemini 网页协议，账号是浏览器 Cookie。反重力仍用自己的 OAuth，两边不共用号。
        </p>
      </div>
      <div className="dr-panel" style={{ marginBottom: 16 }}>
        <div className="strong small">粘贴 Cookie</div>
        <input
          className="input"
          placeholder="邮箱（可选，用来认这个号）"
          value={email}
          onChange={(e) => setEmail(e.target.value)}
        />
        <textarea
          className="input"
          rows={4}
          placeholder="从 gemini.google.com 已登录请求里复制整段 Cookie，需要包含 __Secure-1PSID"
          value={cookie}
          onChange={(e) => setCookie(e.target.value)}
        />
        <button type="button" className="btn btn-sm btn-primary" disabled={busy || !cookie.trim()} onClick={() => void add()}>
          {busy ? "正在添加" : "添加网页账号"}
        </button>
      </div>
      {error ? <ErrorNote error={error} /> : null}
      {loading ? (
        <p className="faint tiny">正在读取账号</p>
      ) : list.length === 0 ? (
        <Empty title="还没有 Gemini 网页账号">粘贴 Cookie 之后，客户端用 gemini-web/ 前缀走这条通道。</Empty>
      ) : (
        <>
          <div className="status-pills" role="tablist" aria-label="账号状态视图" style={{ marginBottom: 12 }}>
            <button
              type="button"
              className={`status-pill${filter === "all" ? " is-active" : ""}`}
              onClick={() => setFilter("all")}
            >
              <span>全部</span>
              <span className="pill-n">{list.length}</span>
            </button>
            <button
              type="button"
              className={`status-pill${filter === "active" ? " is-active" : ""}`}
              onClick={() => setFilter("active")}
            >
              <i className="status-dot is-long_lived" />
              <span>可用</span>
              <span className="pill-n">{activeCount}</span>
            </button>
            {disabledCount > 0 ? (
              <button
                type="button"
                className={`status-pill${filter === "disabled" ? " is-active" : ""}`}
                onClick={() => setFilter("disabled")}
              >
                <i className="status-dot is-dead" />
                <span>已停用</span>
                <span className="pill-n">{disabledCount}</span>
              </button>
            ) : null}
          </div>
          <div className="accts">
            {filteredList.map((account) => (
              <GatewayCard
                key={account.id}
                logo={<VendorLogo vendor="google" size={14} />}
                title={account.label || account.email || account.accountRef}
                plan="网页"
                planClass="plan plan-free"
                dimmed={!account.enabled || account.status !== "active"}
                tone={account.status === "active" && account.enabled ? "ok" : "warn"}
                blank={account.status === "active" ? "Cookie 已保存，请求时向网页换令牌" : "需要重新粘贴 Cookie"}
                problem={account.lastError}
                onOpen={() => undefined}
                spend={
                  <span className="qf-meta">
                    <button
                      type="button"
                      className="btn btn-sm btn-soft"
                      onClick={() => void geminiWeb.setEnabled(account.id, !account.enabled).then(reload)}
                    >
                      <Icon name="gateway" size={12} />
                      {account.enabled ? "移出通道" : "加入通道"}
                    </button>
                    <button
                      type="button"
                      className="btn btn-sm btn-soft"
                      onClick={() => void geminiWeb.remove(account.id).then(reload)}
                    >
                      删除
                    </button>
                  </span>
                }
              />
            ))}
          </div>
        </>
      )}
    </div>
  );
}
