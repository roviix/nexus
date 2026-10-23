/**
 * 账号 · Google 反重力 (Antigravity) 专用管理页。
 *
 * 现代化、丝滑体验：
 * - 平台页签导航与专属二级控制栏（全部 / 活跃就绪 / 需重新授权 快捷筛选 + 实时搜索）；
 * - 顶部操作区集中提供「⟳ 刷新全部」与「+ 添加账号」主按钮；
 * - 账号卡片精简收敛（仅保留 3 款旗舰模型配额与重置倒计时，高度统一）；
 * - 点击任意卡片平滑滑出右侧详情抽屉（本地网关一键接入、设为主用号、查看全部 25+ 分桶明细、凭证与安全管理）；
 * - 彻底移除「待接力 / 网关走这里」等杂乱术语，还界面以优雅从容。
 */
import { useCallback, useEffect, useMemo, useState, type ReactNode } from "react";
import { laneOf } from "../../gateway/channels";
import { antigravity, gateway } from "../../ipc/api";
import type { DeviceAccount, GatewayCandidate, GatewayStatus } from "../../ipc/types";
import type { Route } from "../../shell/nav";
import { Empty, ErrorNote, Icon } from "../../ui/primitives";
import { AntigravityAccountCard } from "./AntigravityAccountCard";
import { AntigravityDrawer } from "./AntigravityDrawer";
import { AddModal, ANTIGRAVITY } from "./DeviceAccounts";

type FilterTab = "all" | "active" | "needs_login";

export function AntigravityAccounts({
  tabs,
  onGo: _onGo,
}: {
  tabs: ReactNode;
  onGo: (r: Route) => void;
}) {
  const [list, setList] = useState<DeviceAccount[]>([]);
  const [loading, setLoading] = useState(true);
  const [refreshingAll, setRefreshingAll] = useState(false);
  const [error, setError] = useState<unknown>(null);
  const [status, setStatus] = useState<GatewayStatus | null>(null);

  const [filter, setFilter] = useState<FilterTab>("all");
  const [query, setQuery] = useState("");
  const [selectedId, setSelectedId] = useState<string | null>(null);
  const [adding, setAdding] = useState(false);
  const [refreshingIds, setRefreshingIds] = useState<Set<string>>(new Set());

  const reload = useCallback(async () => {
    try {
      const [accounts, gwStatus] = await Promise.all([
        antigravity.list(),
        gateway.status().catch(() => null),
      ]);
      setList(accounts);
      setStatus(gwStatus);
      setError(null);
    } catch (e) {
      setError(e);
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    void reload();
  }, [reload]);

  // 网关候选队列中的对应槽位
  const laneByLabel = useMemo(() => {
    const map = new Map<string, GatewayCandidate>();
    if (!status) return map;
    const lane = laneOf(status, "antigravity");
    for (const cand of lane.candidates) {
      map.set(cand.label.toLowerCase(), cand);
    }
    return map;
  }, [status]);

  const activeCount = useMemo(
    () => list.filter((a) => a.status === "active" && a.enabled).length,
    [list],
  );
  const needsLoginCount = useMemo(
    () => list.filter((a) => a.status !== "active").length,
    [list],
  );

  const filtered = useMemo(() => {
    return list.filter((a) => {
      if (filter === "active" && (a.status !== "active" || !a.enabled)) return false;
      if (filter === "needs_login" && a.status === "active") return false;
      if (query.trim()) {
        const q = query.trim().toLowerCase();
        const matchTitle = (a.label || a.email || a.accountRef).toLowerCase().includes(q);
        const matchNote = a.note?.toLowerCase().includes(q);
        const matchProject = a.projectId?.toLowerCase().includes(q);
        if (!matchTitle && !matchNote && !matchProject) return false;
      }
      return true;
    });
  }, [list, filter, query]);

  const selectedAccount = useMemo(
    () => list.find((a) => a.id === selectedId) ?? null,
    [list, selectedId],
  );

  const handleRefreshAll = async () => {
    if (refreshingAll) return;
    setRefreshingAll(true);
    try {
      await Promise.all(list.map((a) => antigravity.refreshQuota(a.id).catch(() => null)));
      await reload();
    } finally {
      setRefreshingAll(false);
    }
  };

  const handleToggle = async (id: string, on: boolean) => {
    await antigravity.setEnabled(id, on);
    await reload();
  };

  const handleUse = async (label: string) => {
    await antigravity.setCurrent(label);
    await reload();
  };

  const handleRemove = async (id: string) => {
    await antigravity.remove(id);
    if (selectedId === id) setSelectedId(null);
    await reload();
  };

  const handleRefreshOne = async (id: string) => {
    setRefreshingIds((p) => new Set(p).add(id));
    try {
      await antigravity.refreshQuota(id);
      await reload();
    } finally {
      setRefreshingIds((p) => {
        const next = new Set(p);
        next.delete(id);
        return next;
      });
    }
  };

  const handleNote = async (id: string, note: string | null) => {
    await antigravity.setNote(id, note);
    await reload();
  };

  return (
    <>
      <div className="page-head acct-head">
        {tabs}
        <div className="row">
          <button
            type="button"
            className="btn btn-icon"
            disabled={refreshingAll || list.length === 0}
            onClick={handleRefreshAll}
            data-tip={refreshingAll ? "刷新中…" : "刷新全部反重力账号用量"}
            aria-label="刷新全部账号用量"
          >
            <Icon name="refresh" size={15} className={refreshingAll ? "is-spinning" : undefined} />
          </button>
          <button type="button" className="btn btn-primary" onClick={() => setAdding(true)}>
            <Icon name="plus" size={14} />
            添加账号
          </button>
        </div>
      </div>

      <ErrorNote error={error} onRetry={() => void reload()} />

      {list.length > 0 ? (
        <div className="unified-toolbar">
          {/* 筛选胶囊 */}
          <div className="status-pills" role="radiogroup" aria-label="账号状态筛选">
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
            {needsLoginCount > 0 ? (
              <button
                type="button"
                className={`status-pill${filter === "needs_login" ? " is-active" : ""}`}
                onClick={() => setFilter("needs_login")}
              >
                <i className="status-dot is-dead" />
                <span>需重新授权</span>
                <span className="pill-n">{needsLoginCount}</span>
              </button>
            ) : null}
          </div>

          {/* 搜索栏：账号较多或正在搜索时展示 */}
          {list.length > 4 || query ? (
            <div className="acct-controls">
              <label className="search">
                <Icon name="search" size={14} />
                <input
                  value={query}
                  onChange={(e) => setQuery(e.target.value)}
                  placeholder="搜索账号或备注..."
                  spellCheck={false}
                />
                {query ? (
                  <button
                    type="button"
                    className="search-clear"
                    onClick={() => setQuery("")}
                    aria-label="清空搜索"
                  >
                    <Icon name="close" size={12} />
                  </button>
                ) : null}
              </label>
            </div>
          ) : null}
        </div>
      ) : null}

      {/* 账号网格 */}
      {loading ? (
        <div className="accts">
          <div className="skeleton" style={{ height: 164 }} />
          <div className="skeleton" style={{ height: 164 }} />
        </div>
      ) : list.length === 0 ? (
        <Empty
          title="还没有 Google 反重力 (Antigravity) 账号"
          action={
            <button type="button" className="btn btn-primary" onClick={() => setAdding(true)}>
              <Icon name="plus" size={14} />
              添加第一个反重力账号
            </button>
          }
        >
          通过 Google 官方 OAuth 授权登录，安全获取 Gemini 与 Claude 配额。
        </Empty>
      ) : filtered.length === 0 ? (
        <div className="faint tiny" style={{ padding: 32, textAlign: "center" }}>
          没有符合当前筛选或搜索条件的账号
        </div>
      ) : (
        <div className="accts">
          {filtered.map((a) => {
            const labelKey = (a.label || a.email || a.accountRef).toLowerCase();
            const lane = laneByLabel.get(labelKey) ?? null;
            return (
              <AntigravityAccountCard
                key={a.id}
                account={a}
                prefix="agy"
                lane={lane}
                disabled={refreshingAll}
                refreshing={refreshingIds.has(a.id) || refreshingAll}
                onOpen={() => setSelectedId(a.id)}
                onToggle={(on) => void handleToggle(a.id, on)}
                onUse={() => void handleUse(a.label || a.email || a.accountRef)}
                onRelogin={() => setAdding(true)}
                onRemove={() => void handleRemove(a.id)}
                onRefresh={() => handleRefreshOne(a.id)}
                onNote={(note) => handleNote(a.id, note)}
              />
            );
          })}
        </div>
      )}

      {/* 右侧滑出抽屉 */}
      {selectedAccount ? (
        <AntigravityDrawer
          account={selectedAccount}
          lane={laneByLabel.get((selectedAccount.label || selectedAccount.email || selectedAccount.accountRef).toLowerCase()) ?? null}
          onClose={() => setSelectedId(null)}
          onRefresh={async () => {
            await handleRefreshOne(selectedAccount.id);
          }}
          onRelogin={() => {
            setSelectedId(null);
            setAdding(true);
          }}
          onRemove={async () => {
            await handleRemove(selectedAccount.id);
          }}
          onToggle={async (on) => {
            await handleToggle(selectedAccount.id, on);
          }}
          onUse={async () => {
            await handleUse(selectedAccount.label || selectedAccount.email || selectedAccount.accountRef);
          }}
          onNote={async (note) => {
            await handleNote(selectedAccount.id, note);
          }}
        />
      ) : null}

      {/* 添加账号弹窗 */}
      {adding ? (
        <AddModal
          spec={ANTIGRAVITY}
          onClose={() => setAdding(false)}
          onAdded={async () => {
            setAdding(false);
            setLoading(true);
            await reload();
            setLoading(false);
          }}
        />
      ) : null}
    </>
  );
}
