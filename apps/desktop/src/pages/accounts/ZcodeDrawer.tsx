/**
 * ZCode 账号详情抽屉。
 *
 * 现代化、克制、精致质感（完全对标 Cursor 与 Antigravity 抽屉）：
 * - 账号身份、智谱 GLM 标识、套餐档位与健康状态；
 * - 本地网关一键加入 / 移出 / 切为主用；
 * - 套餐与模型规格说明；
 * - 凭证信息（API Key / JWT、账号标识）；
 * - 页脚删除账号（安全二次确认）。
 */
import type { DeviceAccount, GatewayCandidate } from "../../ipc/types";
import { CopyButton, Drawer, Health } from "../../ui/primitives";
import { timeAgo } from "../../ui/format";
import { VendorLogo } from "../../relay/VendorLogo";
import { DeleteAccount, NoteLine } from "./AccountDrawer";
import { GatewayLane } from "./GatewayCard";

export function ZcodeDrawer({
  account: a,
  lane,
  onClose,
  onRemove,
  onToggle,
  onUse: _onUse,
  onNote,
}: {
  account: DeviceAccount;
  lane: GatewayCandidate | null;
  onClose: () => void;
  onRemove: () => Promise<void>;
  onToggle: (on: boolean) => Promise<void>;
  onUse: () => Promise<void>;
  onNote: (note: string | null) => Promise<void>;
}) {
  const isCurrent = lane?.state.kind === "current";
  const needsLogin = a.status !== "active";
  const title = a.label || a.email || a.accountRef;
  const isStartPlan = a.plan === "start-plan" || (!a.hasApiKey && a.hasJwt);
  const planName = isStartPlan
    ? "体验套餐"
    : a.family?.includes("team")
      ? "团队版"
      : a.family?.includes("individual")
        ? "个人版"
        : "编码套餐";
  const providerName = a.provider === "bigmodel" ? "智谱 BigModel" : "Z.AI 开放平台";

  const planClass = isStartPlan
    ? "plan plan-free"
    : a.family?.includes("team")
      ? "plan plan-team"
      : "plan plan-pro";

  const head = (
    <div className="dr-id">
      <div className="dr-title">
        <VendorLogo vendor="zhipu" size={18} />
        <span className="dr-email selectable truncate" title={title}>
          {title}
        </span>
        <CopyButton value={title} icon label="复制账号" />
      </div>

      <div className="dr-badges">
        <span className={planClass}>{planName}</span>
        <span className="pill pill-quiet">{providerName}</span>
        <span className="dr-meta">
          {needsLogin ? (
            <Health tone="warn">需登录</Health>
          ) : !a.enabled ? (
            <Health tone="warn">已暂停调度</Health>
          ) : (
            <Health tone="ok">健康可用</Health>
          )}
          <span>{a.lastCheckedAt ? `${timeAgo(a.lastCheckedAt)}检查` : "本地凭证"}</span>
        </span>
      </div>

      <NoteLine key={a.id} note={a.note ?? ""} onSave={onNote} />

      <div className="dr-actions">
        {needsLogin ? (
          <button type="button" className="btn btn-sm btn-primary" disabled>
            需官方客户端重新登录
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
        warning="将从本地彻底清除该 ZCode 账号的所有凭证记录。"
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

        <div className="dr-panel">
          <div className="strong small">套餐</div>
          <div className="dr-row">
            <span className="k">服务商</span>
            <span className="v">{providerName}</span>
          </div>
          <div className="dr-row">
            <span className="k">规格</span>
            <span className="v">{isStartPlan ? "体验额度" : planName}</span>
          </div>
          <div className="dr-row">
            <span className="k">路由</span>
            <span className="v mono tiny">glm-*</span>
          </div>
        </div>

        <div className="dr-panel">
          <div className="strong small">凭证</div>
          <div className="dr-row">
            <span className="k">形式</span>
            <span className="v">{a.hasApiKey ? "永久 API Key" : "体验令牌"}</span>
          </div>
          {a.keyHint ? (
            <div className="dr-row">
              <span className="k">识别</span>
              <code className="v tiny mono">{a.keyHint}••••</code>
            </div>
          ) : null}
          <div className="dr-row">
            <span className="k">标识</span>
            <span className="v row" style={{ gap: 6 }}>
              <code className="tiny mono">{a.accountRef.slice(-14)}</code>
              <CopyButton value={a.accountRef} label="复制标识" icon />
            </span>
          </div>
        </div>
      </div>
    </Drawer>
  );
}
