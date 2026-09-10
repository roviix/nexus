import { useState, type ReactNode } from "react";
import { accounts } from "../ipc/api";
import {
  Banner,
  CopyButton,
  Drawer,
  ErrorNote,
  Tag,
} from "../ui/primitives";
import { QuotaBlank, QuotaSummary } from "../ui/AccountLine";
import { AccountDrawer } from "../pages/accounts/AccountDrawer";
import { AuthorizeModal } from "../pages/accounts/AuthorizeModal";
import type { AccountView } from "./model";
import { ACCOUNT_PLATFORM_LABEL } from "./model";

/**
 * 任意页面打开账号详情的统一控制器。
 *
 * 页面只保留“打开的是谁”和场景动作；刷新、授权、备注、删除都走同一套实现。未来平台通过
 * `AccountView.platform` 增加 inspector adapter，不要求每个业务页面重新写抽屉。
 */
export function AccountInspector({
  view,
  inCursor,
  onClose,
  onChanged,
  onSwitch,
  placementActions,
  onOpenLibrary,
}: {
  view: AccountView | null;
  inCursor: boolean;
  onClose: () => void;
  onChanged: () => void | Promise<void>;
  /** 把这个平台账号切入 Cursor；由页面决定是直接确认还是跳转。 */
  onSwitch: () => void;
  placementActions?: ReactNode;
  /** 未托管账号没有凭证可管，给它一扇去总账号库的门。 */
  onOpenLibrary?: () => void;
}) {
  const [refreshing, setRefreshing] = useState(false);
  const [authorizing, setAuthorizing] = useState(false);
  const [error, setError] = useState<unknown>(null);

  if (!view) return null;

  switch (view.platform) {
    case "cursor":
      if (!view.managed) {
        return (
          <UnmanagedAccountDrawer
            view={view}
            error={error}
            inCursor={inCursor}
            onSwitch={onSwitch}
            placementActions={placementActions}
            onClose={onClose}
            onOpenLibrary={onOpenLibrary}
          />
        );
      }
      // 把已收窄的对象捕获下来，异步回调里不靠非空断言维持类型安全。
      const managed = view.managed;

      return (
        <>
          <AccountDrawer
            account={managed}
            refreshing={refreshing}
            onClose={onClose}
            onRefresh={() => {
              void run(async () => {
                setRefreshing(true);
                try {
                  await accounts.refreshUsage(managed.id);
                } finally {
                  setRefreshing(false);
                }
              });
            }}
            onAuthorize={() => setAuthorizing(true)}
            inCursor={inCursor}
            onSwitch={onSwitch}
            placement={view.placement}
            placementActions={placementActions}
            actionError={error}
            onSaveNote={(note) =>
              run(() => accounts.patch(managed.id, { note }))
            }
            onReload={() => run(async () => undefined)}
            onRemove={() =>
              run(async () => {
                await accounts.remove(managed.id);
                onClose();
              })
            }
          />
          {authorizing ? (
            <AuthorizeModal
              account={managed}
              onClose={() => {
                setAuthorizing(false);
                void Promise.resolve(onChanged());
              }}
            />
          ) : null}
        </>
      );
  }

  async function run(action: () => Promise<unknown>): Promise<void> {
    setError(null);
    try {
      await action();
      await onChanged();
    } catch (nextError) {
      setError(nextError);
    }
  }
}

function UnmanagedAccountDrawer({
  view,
  error,
  inCursor,
  onSwitch,
  placementActions,
  onClose,
  onOpenLibrary,
}: {
  view: AccountView;
  error: unknown;
  inCursor: boolean;
  onSwitch: () => void;
  placementActions?: ReactNode;
  onClose: () => void;
  onOpenLibrary?: () => void;
}) {
  const head = (
    <div className="dr-id">
      <div className="dr-title">
        <span className="dr-email selectable truncate" title={view.label}>
          {view.label}
        </span>
        <CopyButton value={view.label} icon label="复制账号" />
      </div>
      <div className="dr-badges">
        <Tag>{ACCOUNT_PLATFORM_LABEL[view.platform]}</Tag>
        <span className="dr-meta">{view.placement.label}</span>
      </div>
      <div className="dr-actions">
        <button
          type="button"
          className={inCursor ? "btn btn-sm dr-cta" : "btn btn-sm btn-primary dr-cta"}
          disabled={inCursor}
          onClick={onSwitch}
        >
          {inCursor ? "Cursor 正在用" : "切号"}
        </button>
      </div>
    </div>
  );

  const foot = onOpenLibrary ? (
    <button type="button" className="btn" onClick={onOpenLibrary}>
      去账号
    </button>
  ) : undefined;

  return (
    <Drawer
      label={`账号详情 ${view.label}`}
      onClose={onClose}
      head={head}
      footer={foot}
    >
      <div className="dr-body">
        <ErrorNote error={error} />
        <div className="account-context">
          <span className="account-context-copy">
            <strong>{view.placement.label}</strong>
            {view.placement.detail ? <span>{view.placement.detail}</span> : null}
          </span>
          {placementActions ? <span className="row">{placementActions}</span> : null}
        </div>
        <Banner
          tone="warn"
          title="这个账号未在账号库托管"
          hint="这里只保留了场景所需的身份。完整用量、备注、授权和凭证都不可用；加入账号库后才能管理。"
        />
        <div className="card">
          <div className="section-head" style={{ margin: "0 0 12px" }}>
            <strong>已知用量</strong>
          </div>
          {view.usage.kind === "summary" ? (
            <QuotaSummary
              label={view.usage.label}
              percentUsed={view.usage.percentUsed}
            />
          ) : (
            <QuotaBlank>
              {view.usage.kind === "unavailable"
                ? view.usage.reason
                : "没有可展示的用量"}
            </QuotaBlank>
          )}
        </div>
      </div>
    </Drawer>
  );
}
