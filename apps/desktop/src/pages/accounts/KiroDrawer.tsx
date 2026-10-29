/**
 * Kiro 账号详情抽屉。
 *
 * 现代化、克制、精致质感（对标 Cursor 与 Antigravity 账号抽屉）：
 * - 账号身份、Kiro 标识、套餐档位与健康状态；
 * - 本地网关一键加入 / 移出 / 切为主用；
 * - 额度与用量明细（基础额度、试用额度、奖励额度、超额开启状态）；
 * - 凭证信息（授权方式、Token 有效期、账号唯一标识）；
 * - 页脚删除账号（安全二次确认）。
 */
import { useEffect, useState } from "react";
import type { DeviceAccount, GatewayCandidate } from "../../ipc/types";
import { QuotaRows } from "../../ui/AccountLine";
import { CopyButton, Drawer, Health, Icon, Spinner } from "../../ui/primitives";
import { timeAgo } from "../../ui/format";
import { VendorLogo } from "../../relay/VendorLogo";
import { DeleteAccount, NoteLine } from "./AccountDrawer";
import { GatewayLane } from "./GatewayCard";
import { kiro } from "../../ipc/api";

export function KiroDrawer({
  account: a,
  lane,
  isClientActive,
  onSwitchedClient,
  onClose,
  onRefresh,
  onRelogin,
  onRemove,
  onToggle,
  onUse: _onUse,
  onNote,
}: {
  account: DeviceAccount;
  lane: GatewayCandidate | null;
  isClientActive?: boolean;
  onSwitchedClient?: () => void;
  onClose: () => void;
  onRefresh: () => Promise<void>;
  onRelogin: () => void;
  onRemove: () => Promise<void>;
  onToggle: (on: boolean) => Promise<void>;
  onUse: () => Promise<void>;
  onNote: (note: string | null) => Promise<void>;
}) {
  const [refreshing, setRefreshing] = useState(false);
  const [switching, setSwitching] = useState(false);
  const [switchNotice, setSwitchNotice] = useState<string | null>(null);
  const [localClientActive, setLocalClientActive] = useState<boolean | null>(isClientActive ?? null);

  useEffect(() => {
    let alive = true;
    void kiro
      .activeClient()
      .then((active) => {
        if (!alive) return;
        const matched = Boolean(
          active?.accountId === a.id ||
            (active?.email && a.email && active.email.toLowerCase() === a.email.toLowerCase()),
        );
        setLocalClientActive(matched);
      })
      .catch(() => {});
    return () => {
      alive = false;
    };
  }, [a.id, a.email]);

  const activeInClient = localClientActive ?? isClientActive;

  const handleSwitchClient = async () => {
    if (switching) return;
    setSwitching(true);
    setSwitchNotice(null);
    try {
      const outcome = await kiro.switchToClient(a.id);
      setLocalClientActive(true);
      setSwitchNotice(outcome.message);
      onSwitchedClient?.();
    } catch (err: any) {
      setSwitchNotice(`切号失败：${err?.message || err}`);
    } finally {
      setSwitching(false);
    }
  };

  const isCurrent = lane?.state.kind === "current";
  const needsLogin = a.status !== "active";
  const usage = a.kiroUsage ?? null;
  const title = a.label || a.email || a.accountRef;
  const plan = usage?.subscriptionTitle || a.planType || "Free";
  const auth = authLabel(a.authMethod);
  const pct = usage ? Math.max(0, Math.min(100, Math.round(usage.percent))) : null;

  const planLower = plan.toLowerCase();
  const planClass = planLower.includes("power")
    ? "plan plan-ultra"
    : planLower.includes("pro+") || planLower.includes("pro plus") || planLower.includes("proplus")
      ? "plan plan-proplus"
      : planLower.includes("pro")
        ? "plan plan-pro"
        : planLower.includes("team") || planLower.includes("enterprise")
          ? "plan plan-team"
          : planLower.includes("free")
            ? "plan plan-free"
            : "plan";

  const handleRefresh = async () => {
    if (refreshing) return;
    setRefreshing(true);
    try {
      await onRefresh();
    } finally {
      setRefreshing(false);
    }
  };

  const quotaRows = usage
    ? [
        {
          key: "base",
          label: "基础额度",
          hint: `基础套餐已用 ${qty(usage.baseUsed)} / ${qty(usage.baseLimit)}`,
          percent: pct,
          note: `${qty(usage.baseUsed)} / ${qty(usage.baseLimit)}`,
        },
        ...(usage.trialLimit != null
          ? [
              {
                key: "trial",
                label: "试用额度",
                hint: `试用额度已用 ${qty(usage.trialUsed ?? 0)} / ${qty(usage.trialLimit)}`,
                percent: Math.max(0, Math.min(100, Math.round(((usage.trialUsed ?? 0) / usage.trialLimit) * 100))),
                note: `${qty(usage.trialUsed ?? 0)} / ${qty(usage.trialLimit)}`,
              },
            ]
          : []),
        ...(usage.bonusLimit != null
          ? [
              {
                key: "bonus",
                label: "奖励额度",
                hint: `奖励额度已用 ${qty(usage.bonusUsed ?? 0)} / ${qty(usage.bonusLimit)}`,
                percent: Math.max(0, Math.min(100, Math.round(((usage.bonusUsed ?? 0) / usage.bonusLimit) * 100))),
                note: `${qty(usage.bonusUsed ?? 0)} / ${qty(usage.bonusLimit)}`,
              },
            ]
          : []),
      ]
    : [];

  const head = (
    <div className="dr-id">
      <div className="dr-title">
        <VendorLogo vendor="kiro" size={18} />
        <span className="dr-email selectable truncate" title={title}>
          {title}
        </span>
        <CopyButton value={title} icon label="复制账号" />
      </div>

      <div className="dr-badges">
        <span className={planClass}>{plan}</span>
        {auth ? <span className="pill pill-quiet">{auth}</span> : null}
        {activeInClient ? (
          <span
            className="pill tiny"
            title="Kiro 客户端当前正在登录此账号"
            style={{
              background: "rgba(16, 185, 129, 0.12)",
              color: "var(--ok)",
              border: "1px solid rgba(16, 185, 129, 0.25)",
              fontWeight: 600,
            }}
          >
            ● Kiro 当前登录
          </span>
        ) : null}
        <span className="dr-meta">
          {needsLogin ? (
            <Health tone="warn">需重新授权</Health>
          ) : !a.enabled ? (
            <Health tone="warn">已暂停调度</Health>
          ) : (
            <Health tone="ok">健康可用</Health>
          )}
          <span>{usage?.checkedAt ? `用量 ${timeAgo(usage.checkedAt)}更新` : "未同步用量"}</span>
        </span>
      </div>

      <NoteLine key={a.id} note={a.note ?? ""} onSave={onNote} />

      <div className="dr-actions">
        <button
          type="button"
          className="btn btn-sm btn-icon btn-soft"
          onClick={handleRefresh}
          disabled={refreshing || needsLogin}
          title="刷新最新配额与 Token"
          aria-label="刷新"
        >
          <Icon name="refresh" size={13} className={refreshing ? "is-spinning" : undefined} />
        </button>

        {needsLogin ? (
          <button type="button" className="btn btn-sm btn-primary" onClick={onRelogin}>
            重新授权
          </button>
        ) : null}

        {!needsLogin ? (
          activeInClient ? (
            <button
              type="button"
              className="btn btn-sm btn-soft dr-cta"
              disabled
              title="Kiro 客户端当前登录的就是这个号"
            >
              <Icon name="check" size={13} style={{ color: "var(--ok)" }} />
              <span>Kiro 当前登录</span>
            </button>
          ) : (
            <button
              type="button"
              className="btn btn-sm btn-primary dr-cta"
              disabled={switching}
              onClick={handleSwitchClient}
              title="把这个号写入本机 Kiro 客户端"
            >
              {switching ? <Spinner /> : <Icon name="switcher" size={13} />}
              <span>{switching ? "正在切号…" : "切号到 Kiro 客户端"}</span>
            </button>
          )
        ) : null}
      </div>

      {switchNotice ? (
        <div
          className={`dr-notice ${switchNotice.includes("失败") ? "is-bad" : "is-ok"}`}
          style={{
            marginTop: 10,
            padding: "8px 12px",
            borderRadius: 6,
            fontSize: 12,
            background: switchNotice.includes("失败")
              ? "rgba(239, 68, 68, 0.1)"
              : "rgba(16, 185, 129, 0.1)",
            color: switchNotice.includes("失败") ? "var(--bad)" : "var(--ok)",
            border: `1px solid ${
              switchNotice.includes("失败")
                ? "rgba(239, 68, 68, 0.25)"
                : "rgba(16, 185, 129, 0.25)"
            }`,
            display: "flex",
            alignItems: "center",
            gap: 6,
          }}
        >
          <Icon name={switchNotice.includes("失败") ? "alert" : "check"} size={13} />
          <span>{switchNotice}</span>
        </div>
      ) : null}
    </div>
  );

  const foot = (
    <footer className="dr-foot">
      <DeleteAccount
        onConfirm={async () => {
          await onRemove();
          onClose();
        }}
        warning="将从本地彻底清除该 Kiro 账号的所有凭证与额度缓存。"
      />
      <span className="faint tiny">
        {a.createdAt ? `${timeAgo(a.createdAt)}添加` : "本地账号"}
      </span>
    </footer>
  );

  return (
    <Drawer label={`账号详情 ${title}`} onClose={onClose} head={head} footer={foot}>
      <div className="dr-body">
        {/* 本地网关调度 */}
        <GatewayLane enabled={a.enabled} current={isCurrent} onToggle={(on) => void onToggle(on)} />

        {/* 额度与用量明细 */}
        <div className="dr-panel">
          <div className="strong small">使用配额</div>
          {usage ? (
            <>
              <QuotaRows rows={quotaRows} />
              <div className="stack" style={{ gap: 8, marginTop: 4, paddingTop: 8, borderTop: "1px solid var(--color-line)" }}>
                {usage.resetAt ? (
                  <div className="dr-row">
                    <span className="k">重置周期</span>
                    <span className="v strong">
                      {day(usage.resetAt)} · {leftLabel(usage.resetAt)}
                    </span>
                  </div>
                ) : null}
                {usage.overageEnabled != null ? (
                  <div className="dr-row">
                    <span className="k">超额扣费</span>
                    <span className="v">{usage.overageEnabled ? "已开启" : "未开启"}</span>
                  </div>
                ) : null}
              </div>
            </>
          ) : (
            <p className="faint tiny" style={{ margin: 0 }}>
              {needsLogin ? "重新授权后方可获取套餐与额度信息。" : "尚未同步用量，可点击上方刷新按钮手动查询。"}
            </p>
          )}
        </div>

        {/* 凭证与会话状态 */}
        <div className="dr-panel">
          <div className="strong small">凭证与会话</div>
          <div className="dr-row">
            <span className="k">授权方式</span>
            <span className="v">{auth || "AWS 身份认证"}</span>
          </div>

          <div className="dr-row">
            <span className="k">访问令牌</span>
            <span className="v">
              {a.accessExpiresAt && !needsLogin
                ? `有效期剩余 ${tokenLeft(a.accessExpiresAt)}`
                : a.hasRefresh
                  ? "具备自动续期凭证"
                  : "凭证就绪"}
            </span>
          </div>

          <div className="dr-row">
            <span className="k">账号标识</span>
            <span className="v row" style={{ gap: 6, alignItems: "center" }}>
              <code className="tiny mono">{a.accountRef.slice(-12)}</code>
              <CopyButton value={a.accountRef} label="复制标识" icon />
            </span>
          </div>
        </div>
      </div>
    </Drawer>
  );
}

function authLabel(method: string | null | undefined): string | null {
  if (!method) return null;
  const m = method.toLowerCase();
  if (m.includes("google")) return "Google 登录";
  if (m.includes("github")) return "GitHub 登录";
  if (m.includes("builder")) return "AWS Builder ID";
  if (m.includes("social")) return "社交账号登录";
  if (m.includes("external") || m.includes("entra") || m.includes("azure") || m.includes("microsoft")) {
    return "企业 SSO 登录";
  }
  if (m.includes("idc") || m.includes("sso")) return "IAM Identity Center";
  return method;
}

function qty(n: number): string {
  if (!Number.isFinite(n)) return "—";
  const rounded = Math.abs(n - Math.round(n)) < 0.05 ? Math.round(n) : Math.round(n * 10) / 10;
  return rounded.toLocaleString("zh-CN");
}

function day(iso: string): string {
  const t = Date.parse(iso);
  if (!Number.isFinite(t)) return "—";
  const d = new Date(t);
  const m = String(d.getMonth() + 1).padStart(2, "0");
  const dd = String(d.getDate()).padStart(2, "0");
  return `${d.getFullYear()}-${m}-${dd}`;
}

function leftLabel(iso: string, now = Date.now()): string {
  const t = Date.parse(iso);
  if (!Number.isFinite(t)) return "—";
  const days = Math.ceil((t - now) / 86_400_000);
  if (days <= 0) return "今天重置";
  if (days === 1) return "剩 1 天";
  return `剩 ${days} 天`;
}

function tokenLeft(iso: string, now = Date.now()): string {
  const t = Date.parse(iso);
  if (!Number.isFinite(t)) return "—";
  const sec = Math.round((t - now) / 1000);
  if (sec <= 0) return "即将续期";
  if (sec < 3600) return `${Math.max(1, Math.floor(sec / 60))} 分钟`;
  if (sec < 86400) return `${Math.floor(sec / 3600)} 小时`;
  return `${Math.floor(sec / 86400)} 天`;
}
