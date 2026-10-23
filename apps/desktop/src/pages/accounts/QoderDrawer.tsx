/**
 * Qoder 账号详情抽屉。
 *
 * 身份、套餐、Credits 用量（套餐 / 加购 / 组织），以及 PAT 凭证。
 * 本地网关的加入和主用也在这一层。
 */
import { useState } from "react";
import type { DeviceAccount, GatewayCandidate } from "../../ipc/types";
import { QuotaRows } from "../../ui/AccountLine";
import { CopyButton, Drawer, Health, Icon } from "../../ui/primitives";
import { timeAgo } from "../../ui/format";
import { VendorLogo } from "../../relay/VendorLogo";
import { DeleteAccount, NoteLine } from "./AccountDrawer";
import { GatewayLane } from "./GatewayCard";
import {
  bucketLabel,
  creditUnit,
  day,
  leftLabel,
  primaryBucket,
  qoderPlan,
  qoderRegion,
  qoderTitle,
  qty,
  tokenLeft,
  usedPercent,
  userTypeLabel,
} from "./qoder";

export function QoderDrawer({
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
  const needsLogin = a.status !== "active" || !a.hasToken;
  const usage = a.qoderUsage ?? null;
  const title = qoderTitle(a);
  const plan = qoderPlan(a);
  const region = qoderRegion(a.backend);
  const kind = userTypeLabel(usage?.userType);
  const bucket = primaryBucket(usage);
  const pct = usedPercent(usage);
  const unit = creditUnit(usage, bucket);

  const handleRefresh = async () => {
    if (refreshing || needsLogin) return;
    setRefreshing(true);
    try {
      await onRefresh();
    } finally {
      setRefreshing(false);
    }
  };

  const planClass = plan.toLowerCase().includes("pro")
    ? "plan plan-pro"
    : plan.toLowerCase().includes("team") || plan.toLowerCase().includes("enterprise")
      ? "plan plan-team"
      : plan.toLowerCase().includes("free")
        ? "plan plan-free"
        : "plan";

  const head = (
    <div className="dr-id">
      <div className="dr-title">
        <VendorLogo vendor="qoder" size={18} />
        <span className="dr-email selectable truncate" title={title}>
          {title}
        </span>
        <CopyButton value={title} icon label="复制账号" />
      </div>

      <div className="dr-badges">
        <span className={planClass}>{plan}</span>
        <span className="pill pill-quiet">{region}</span>
        {kind && !plan.includes(kind) ? <span className="pill pill-quiet">{kind}</span> : null}
        <span className="dr-meta">
          {needsLogin ? (
            <Health tone="warn">需更新 PAT</Health>
          ) : usage?.exceeded ? (
            <Health tone="warn">额度已用尽</Health>
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
          onClick={() => void handleRefresh()}
          disabled={refreshing || needsLogin}
          title="刷新套餐和用量"
          aria-label="刷新"
        >
          <Icon name="refresh" size={13} className={refreshing ? "is-spinning" : undefined} />
        </button>

        {needsLogin ? (
          <button type="button" className="btn btn-sm btn-primary" onClick={onRelogin}>
            重新贴 PAT
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
        warning="将从本机彻底清除这把 Personal Access Token 和所有额度缓存。"
      />
      <span className="faint tiny">{a.createdAt ? `${timeAgo(a.createdAt)}添加` : "本地账号"}</span>
    </footer>
  );

  const rows = usage
    ? [
        ...(bucket && pct != null
          ? [
              {
                key: bucket.id,
                label: bucketLabel(bucket),
                hint: `${qty(bucket.used)} / ${qty(bucket.total)} ${unit}`,
                percent: pct,
                note: `${qty(bucket.used)} / ${qty(bucket.total)} ${unit}`,
              },
            ]
          : []),
        ...usage.buckets
          .filter((b) => b.id !== bucket?.id)
          .map((b) => ({
            key: b.id,
            label: bucketLabel(b),
            hint: `${qty(b.used)} / ${qty(b.total)}`,
            percent: b.percent != null ? Math.max(0, Math.min(100, Math.round(b.percent))) : null,
            note: `${qty(b.used)} / ${qty(b.total)}`,
          })),
      ]
    : [];

  return (
    <Drawer label={`账号详情 ${title}`} onClose={onClose} head={head} footer={foot}>
      <div className="dr-body">
        <GatewayLane enabled={a.enabled} current={isCurrent} onToggle={(on) => void onToggle(on)} />

        <div className="dr-panel">
          <div className="strong small">使用配额与 Credits</div>
          {usage ? (
            usage.unlimited && pct == null ? (
              <p className="faint tiny" style={{ margin: 0 }}>
                这个席位走组织共享额度，不按个人计数。
              </p>
            ) : (
              <>
                <QuotaRows rows={rows} />
                <div className="stack" style={{ gap: 8, marginTop: 4, paddingTop: 8, borderTop: "1px solid var(--color-line)" }}>
                  {usage.resetAt ? (
                    <div className="dr-row">
                      <span className="k">周期重置</span>
                      <span className="v strong">
                        {day(usage.resetAt)} · {leftLabel(usage.resetAt)}
                      </span>
                    </div>
                  ) : null}
                  {kind ? (
                    <div className="dr-row">
                      <span className="k">席位类型</span>
                      <span className="v">{kind}</span>
                    </div>
                  ) : null}
                </div>
              </>
            )
          ) : (
            <p className="faint tiny" style={{ margin: 0 }}>
              {needsLogin ? "重新贴 PAT 之后才能看到套餐和用量。" : "尚未同步用量，可点击上方刷新按钮手动查询。"}
            </p>
          )}
        </div>

        <div className="dr-panel">
          <div className="strong small">凭证与会话</div>
          <div className="dr-row">
            <span className="k">授权形式</span>
            <span className="v">Personal Access Token</span>
          </div>
          <div className="dr-row">
            <span className="k">服务区域</span>
            <span className="v">{region}</span>
          </div>
          {a.keyHint ? (
            <div className="dr-row">
              <span className="k">令牌识别</span>
              <code className="v tiny mono">••••{a.keyHint}</code>
            </div>
          ) : null}
          <div className="dr-row">
            <span className="k">自动换票</span>
            <span className="v">
              {needsLogin
                ? "失效，需重填"
                : a.accessExpiresAt
                  ? `${tokenLeft(a.accessExpiresAt)}后自动换`
                  : a.hasToken
                    ? "具备有效票据"
                    : "—"}
            </span>
          </div>
          {a.userId ? (
            <div className="dr-row">
              <span className="k">用户标识</span>
              <span className="v row" style={{ gap: 6, alignItems: "center" }}>
                <code className="tiny mono truncate">{a.userId}</code>
                <CopyButton value={a.userId} label="复制用户 ID" icon />
              </span>
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
