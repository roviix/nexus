/**
 * Google 反重力 (Antigravity) 账号详情抽屉。
 *
 * 现代化、克制、精致质感（完全对标 Cursor 账号抽屉）：
 * - 账号身份、Google 品牌、套餐档位与健康状态；
 * - 本地网关一键加入 / 移出（与 Cursor 抽屉一致）；
 * - 官方标准双分类配额（Gemini Models / Claude & GPT models）长条比例仪表与倒计时；
 * - 凭证信息（GCP 项目 ID、自动续期状态）；
 * - 页脚删除账号（安全两步确认）。
 */
import { useState } from "react";
import type { DeviceAccount, GatewayCandidate } from "../../ipc/types";
import { QuotaRows } from "../../ui/AccountLine";
import { CopyButton, Drawer, Health, Icon } from "../../ui/primitives";
import { timeAgo } from "../../ui/format";
import { VendorLogo } from "../../relay/VendorLogo";
import { DeleteAccount, NoteLine } from "./AccountDrawer";
import { GatewayLane } from "./GatewayCard";
import { antigravityQuotaRows, formatTierLabel, planClassName } from "./antigravity";

export function AntigravityDrawer({
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
  onNote: (note: string | null) => Promise<void>;
}) {
  const [refreshing, setRefreshing] = useState(false);

  const isCurrent = lane?.state.kind === "current";
  const needsLogin = a.status !== "active";
  const usage = a.antigravityUsage;
  const plan = formatTierLabel(usage?.tierName || a.tier || a.planType);
  const planCls = planClassName(usage?.tierName || a.tier || a.planType);
  const title = a.label || a.email || a.accountRef;

  const forbidden = usage?.lastStatus === "forbidden";
  const rows = antigravityQuotaRows(usage);
  const modelNames = (usage?.modelBuckets ?? [])
    .map((bucket) => bucket.displayName || bucket.modelId)
    .filter((name) => name.trim().length > 0);

  const handleRefresh = async () => {
    if (refreshing) return;
    setRefreshing(true);
    try {
      await onRefresh();
    } finally {
      setRefreshing(false);
    }
  };

  const head = (
    <div className="dr-id">
      <div className="dr-title">
        <VendorLogo vendor="google" size={18} />
        <span className="dr-email selectable truncate" title={title}>
          {title}
        </span>
        <CopyButton value={title} icon label="复制账号" />
      </div>

      <div className="dr-badges">
        {plan ? <span className={planCls}>{plan}</span> : null}
        <span className="dr-meta">
          {needsLogin ? (
            <Health tone="warn">需重新授权</Health>
          ) : forbidden ? (
            <Health tone="bad">账号被拒绝</Health>
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
          title="刷新最新配额"
          aria-label="刷新"
        >
          <Icon name="refresh" size={13} className={refreshing ? "is-spinning" : undefined} />
        </button>

        {needsLogin ? (
          <button type="button" className="btn btn-sm btn-primary" onClick={onRelogin}>
            重新授权
          </button>
        ) : null}
      </div>
    </div>
  );

  const foot = (
    <footer className="dr-foot">
      <DeleteAccount
        onConfirm={async () => {
          await onRemove();
          onClose();
        }}
        warning="将从本地清除该 Google 账号的所有凭证与配额缓存。"
      />
      <span className="faint tiny">
        {a.createdAt ? `${timeAgo(a.createdAt)}添加` : "本地账号"}
      </span>
    </footer>
  );

  return (
    <Drawer label={`账号详情 ${title}`} onClose={onClose} head={head} footer={foot}>
      <div className="dr-body">
        {/* 本地网关：对齐 Cursor 抽屉的 usedin-row 结构 */}
        <GatewayLane enabled={a.enabled} current={isCurrent} onToggle={(on) => void onToggle(on)} />

        <div className="dr-panel">
          <div className="strong small">使用配额</div>
          {rows.length > 0 ? (
            <>
              <QuotaRows rows={rows} />
              {usage?.resetsAt ? (
                <div className="dr-row" style={{ marginTop: 4 }}>
                  <span className="k">周期重置</span>
                  <span className="v strong">
                    {day(usage.resetsAt)} · {leftLabel(usage.resetsAt)}
                  </span>
                </div>
              ) : null}
            </>
          ) : (
            <p className="faint tiny" style={{ margin: 0 }}>
              {needsLogin ? "重新授权后方可获取套餐与额度信息。" : "尚未同步用量，可点击上方刷新按钮手动查询。"}
            </p>
          )}
        </div>

        {modelNames.length > 0 ? (
          <div className="dr-panel">
            <div className="strong small">可用模型</div>
            <p className="faint tiny" style={{ margin: 0 }}>
              {modelNames.join(" · ")}
            </p>
          </div>
        ) : null}

        <div className="dr-panel">
          <div className="strong small">凭证与会话</div>
          <div className="dr-row">
            <span className="k">GCP 项目</span>
            <span className="v row" style={{ gap: 6, alignItems: "center" }}>
              <code className="tiny mono">{usage?.projectId || a.projectId || "未记录"}</code>
              {usage?.projectId || a.projectId ? (
                <CopyButton value={(usage?.projectId || a.projectId)!} label="复制项目" icon />
              ) : null}
            </span>
          </div>
          <div className="dr-row">
            <span className="k">授权续期</span>
            <span className="v">{a.hasRefresh ? "Google 会话 (支持自动续期)" : "单次授权令牌"}</span>
          </div>
          <div className="dr-row">
            <span className="k">访问令牌</span>
            <span className="v">
              {a.accessExpiresAt && !needsLogin
                ? `有效期剩余 ${tokenLeft(a.accessExpiresAt)}`
                : a.hasToken
                  ? "凭证就绪"
                  : "—"}
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
