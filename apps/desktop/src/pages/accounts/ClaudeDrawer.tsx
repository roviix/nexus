/**
 * Claude 账号抽屉。套餐用量和凭证，网关启停和别的平台同一行。
 */
import { useState } from "react";
import type { DeviceAccount, GatewayCandidate } from "../../ipc/types";
import { QuotaRows } from "../../ui/AccountLine";
import { CopyButton, Drawer, Health, Icon } from "../../ui/primitives";
import { timeAgo } from "../../ui/format";
import { VendorLogo } from "../../relay/VendorLogo";
import { DeleteAccount, NoteLine } from "./AccountDrawer";
import { GatewayLane } from "./GatewayCard";

export function ClaudeDrawer({
  account: a,
  lane,
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
  onClose: () => void;
  onRefresh: () => Promise<void>;
  onRelogin: () => void;
  onRemove: () => Promise<void>;
  onToggle: (on: boolean) => Promise<void>;
  onUse: () => Promise<void>;
  onNote?: (note: string | null) => Promise<void>;
}) {
  const [refreshing, setRefreshing] = useState(false);
  const isCurrent = lane?.state.kind === "current";
  const isApiKey = a.authKind === "api_key" || (a.hasApiKey === true && !a.hasRefresh && !a.hasToken);
  const needsLogin = a.status !== "active";
  const title = a.label || a.email || a.accountRef;
  const quota = a.usage ?? null;
  const plan = isApiKey ? "API Key" : (quota?.subscriptionTier || a.planType || a.tier || "Pro");
  const pct = !isApiKey && quota?.creditUsagePercent != null ? Math.max(0, Math.min(100, Math.round(quota.creditUsagePercent))) : null;
  const period = quota?.periodType === "monthly" ? "本月" : quota?.periodType === "five_hour" ? "5 小时" : "本周";

  const planClass = isApiKey
    ? "plan pill-key"
    : plan.toLowerCase().includes("team")
      ? "plan plan-team"
      : plan.toLowerCase().includes("pro")
        ? "plan plan-pro"
        : plan.toLowerCase().includes("free")
          ? "plan plan-free"
          : "plan";

  const head = (
    <div className="dr-id">
      <div className="dr-title">
        <VendorLogo vendor="anthropic" size={18} />
        <span className="dr-email selectable truncate" title={title}>
          {title}
        </span>
        <CopyButton value={title} icon label="复制账号" />
      </div>
      <div className="dr-badges">
        <span className={planClass}>{plan}</span>
        <span className="dr-meta">
          {needsLogin ? (
            <Health tone="warn">需重新授权</Health>
          ) : !a.enabled ? (
            <Health tone="warn">已暂停调度</Health>
          ) : (
            <Health tone="ok">健康可用</Health>
          )}
          <span>{quota ? `用量 ${timeAgo(quota.checkedAt)}更新` : "未同步用量"}</span>
        </span>
      </div>
      {onNote ? <NoteLine key={a.id} note={a.note ?? ""} onSave={onNote} /> : null}
      <div className="dr-actions">
        {!isApiKey ? (
          <button
            type="button"
            className="btn btn-sm btn-icon btn-soft"
            onClick={() => {
              if (refreshing) return;
              setRefreshing(true);
              void onRefresh().finally(() => setRefreshing(false));
            }}
            disabled={refreshing || needsLogin}
            aria-label="刷新用量"
            title="刷新最新配额与状态"
          >
            <Icon name="refresh" size={13} className={refreshing ? "is-spinning" : undefined} />
          </button>
        ) : null}
        {needsLogin ? (
          <button type="button" className="btn btn-sm btn-primary" onClick={onRelogin}>
            重新授权
          </button>
        ) : null}
      </div>
    </div>
  );

  return (
    <Drawer
      label={`账号详情 ${title}`}
      onClose={onClose}
      head={head}
      footer={
        <footer className="dr-foot">
          <DeleteAccount
            onConfirm={async () => {
              await onRemove();
              onClose();
            }}
            warning="将从本机彻底清除这个 Claude 账号的所有凭证缓存。"
          />
          <span className="faint tiny">{a.createdAt ? `${timeAgo(a.createdAt)}添加` : "本地账号"}</span>
        </footer>
      }
    >
      <div className="dr-body">
        <GatewayLane enabled={a.enabled} current={isCurrent} onToggle={(on) => void onToggle(on)} />
        <div className="dr-panel">
          <div className="strong small">使用配额</div>
          {isApiKey ? (
            <div className="stack" style={{ gap: 8 }}>
              <div className="dr-row">
                <span className="k">计费模式</span>
                <span className="v">Anthropic Console API Key (按余额扣费)</span>
              </div>
              <div className="dr-row">
                <span className="k">周期额度</span>
                <span className="v">无固定周期限制，依组织充值额度调度</span>
              </div>
            </div>
          ) : pct != null ? (
            <>
              <QuotaRows rows={[{ key: "credit", label: period, hint: `${period}已用 ${pct}%`, percent: pct }]} />
              {quota?.periodEnd ? (
                <div className="dr-row" style={{ marginTop: 4 }}>
                  <span className="k">周期重置</span>
                  <span className="v strong">
                    {day(quota.periodEnd)} · {leftLabel(quota.periodEnd)}
                  </span>
                </div>
              ) : null}
            </>
          ) : (
            <p className="faint tiny" style={{ margin: 0 }}>
              {needsLogin ? "重新授权后方可获取周期与额度信息。" : "尚未同步用量，可点击上方刷新按钮手动查询。"}
            </p>
          )}
        </div>
        <div className="dr-panel">
          <div className="strong small">凭证与会话</div>
          <div className="dr-row">
            <span className="k">授权形式</span>
            <span className="v">{isApiKey ? "Console API Key" : a.hasRefresh ? "订阅会话 (支持自动续期)" : "网页订阅会话"}</span>
          </div>
          <div className="dr-row">
            <span className="k">访问凭证</span>
            <span className="v">
              {a.accessExpiresAt && !needsLogin
                ? `有效期剩余 ${tokenLeft(a.accessExpiresAt)}`
                : a.hasToken
                  ? "凭证就绪"
                  : "—"}
            </span>
          </div>
          {a.keyHint ? (
            <div className="dr-row">
              <span className="k">密钥识别</span>
              <code className="v tiny mono">••••{a.keyHint}</code>
            </div>
          ) : null}
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
