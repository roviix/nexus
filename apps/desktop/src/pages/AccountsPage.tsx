/**
 * 我的账号（ARCHITECTURE §5.1）。
 *
 * 顶部按**平台**分页签：Cursor、ChatGPT、Grok Build、Kiro。号形状不同（Cursor 的能切进 IDE、
 * 进三种池；其余只喂本机网关），混在一列里两边都看不懂，所以各占一页签。
 * 地址是 `#accounts` / `#accounts/chatgpt` / `#accounts/grok` / `#accounts/kiro`。
 *
 * Cursor 那一页签：一列账号卡负责「扫」，右侧抽屉负责「看」。几十个号的时候，用户进来通常带着一个
 * 具体问题 ——「哪个还能用」「哪个快重置了」「哪些掉了登录」—— 所以顶上是一条按**可用性**分段的
 * 分布条（图例就是筛子），工具栏左边是视图芯片（一组存好的筛子），右边是搜索、额度 / 所在池 / 档位
 * 三个筛子和排序，而不是一排统计数字。筛选 / 排序的组合会记住，也能起名存成视图
 * （`accounts/views.ts`），常问的问题一键就回到那一组筛子。
 *
 * 归档：多选几个号「归档」，它们就从这一页消失（也不再参与批量刷新、不进网关候选），
 * 只在「已归档」视图里能看到、能取回。凭证一个字节不动。
 */
import { useCallback, useEffect, useMemo, useState, type ReactNode } from "react";
import { AccountCard } from "../accounts/AccountCard";
import { AccountInspector } from "../accounts/AccountInspector";
import { createCursorAccountView } from "../accounts/model";
import { PoolChips } from "../accounts/PoolChips";
import {
  matchesPoolFilter,
  POOL_FILTER_LABEL,
  usePools,
  type PoolFilter,
} from "../accounts/pools";
import { accounts, backup, grokbot, onAccountRefreshed, onOauthState, switcher } from "../ipc/api";
import type { Account, ExportOutcome, GrokBotStatus } from "../ipc/types";
import { ACCOUNT_PLATFORMS, go, type AccountPlatform, type Route } from "../shell/nav";
import {
  BUILTIN_VIEWS,
  isDefaultView,
  loadSavedViews,
  loadSpec,
  matchView,
  persistSavedViews,
  saveSpec,
  upsertView,
  type SavedView,
  type ViewSpec,
} from "../accounts/views";
import {
  accountPlanGroup,
  applyAvailFilter,
  applyPlanFilter,
  applyQuotaFilter,
  canQueryUsage,
  isPaidPlan,
  matchesQuery,
  PLAN_FILTER_LABEL,
  PLAN_FILTER_ORDER,
  QUOTA_LABEL,
  QUOTA_ORDER,
  SORT_LABEL,
  sortAccounts,
  summarize,
  type AccountSort,
  type PlanFilter,
  type QuotaFilter,
} from "../ui/accounts";
import { Banner, Empty, ErrorNote, Icon, Picker } from "../ui/primitives";
import { AddAccountModal } from "./accounts/AddAccountModal";
import { AuthorizeModal } from "./accounts/AuthorizeModal";
import { ChatGptAccounts } from "./accounts/ChatGptAccounts";
import { GrokAccounts, KiroAccounts } from "./accounts/DeviceAccounts";

const SORTS: AccountSort[] = ["added", "reset", "botReset", "checked"];
const POOL_FILTERS: PoolFilter[] = ["any", "switcher", "gateway", "unpooled"];

/**
 * 页头只有一行：平台页签站在标题的位置，动作贴右。这一页叫什么侧栏里已经写着，再摆一个大标题
 * 只是把页签和动作挤成两行、三样东西三个高度 —— 页签本身就是这一页的名字。
 */
export function AccountsPage({ route, onGo }: { route: Route; onGo: (r: Route) => void }) {
  const platform: AccountPlatform = route.platform ?? "cursor";
  const tabs = (
    <div className="tabs" role="tablist" aria-label="账号平台">
      {ACCOUNT_PLATFORMS.map((p) => (
        <button
          key={p.id}
          type="button"
          role="tab"
          className="tab"
          aria-selected={platform === p.id}
          onClick={() => onGo(go("accounts", { platform: p.id === "cursor" ? undefined : p.id }))}
        >
          {p.label}
        </button>
      ))}
    </div>
  );
  return (
    <div>
      {platform === "chatgpt" ? (
        <ChatGptAccounts tabs={tabs} />
      ) : platform === "grok" ? (
        <GrokAccounts tabs={tabs} onGo={onGo} />
      ) : platform === "kiro" ? (
        <KiroAccounts tabs={tabs} onGo={onGo} />
      ) : (
        <CursorAccounts tabs={tabs} onGo={onGo} />
      )}
    </div>
  );
}

function CursorAccounts({ tabs, onGo }: { tabs: ReactNode; onGo: (r: Route) => void }) {
  const [list, setList] = useState<Account[]>([]);
  /** Cursor 此刻登录的邮箱（小写），用于禁用当前账号的切号动作。 */
  const [inCursor, setInCursor] = useState<string | null>(null);
  const [error, setError] = useState<unknown>(null);
  const [loading, setLoading] = useState(true);
  const [refreshing, setRefreshing] = useState<Set<string>>(new Set());
  const [notice, setNotice] = useState<string | null>(null);
  const [exporting, setExporting] = useState(false);
  /** 最近一次导出。不自动消失：这条横幅上写着「文件里是明文凭证」，得让人看见。 */
  const [exported, setExported] = useState<ExportOutcome | null>(null);

  const [query, setQuery] = useState("");
  /**
   * 筛选 + 排序的当前组合。四个筛子各管一维（可用性 / 额度 / 所在池 / 档位），互不相干，可以同时下；
   * 整组落盘，下次进来还在原地。
   */
  const [spec, setSpecState] = useState<ViewSpec>(loadSpec);
  const [savedViews, setSavedViews] = useState<SavedView[]>(loadSavedViews);
  const { avail, quota, pool, plan, sort, archived } = spec;
  /** 多选：只在按下「选择」后出现，选完做一件事（归档 / 取回 / 刷新）就退出。 */
  const [selecting, setSelecting] = useState(false);
  const [selected, setSelected] = useState<Set<string>>(new Set());
  const [archiving, setArchiving] = useState(false);
  const setSpec = useCallback((patch: Partial<ViewSpec>) => {
    setSpecState((prev) => {
      const next = { ...prev, ...patch };
      saveSpec(next);
      return next;
    });
  }, []);
  function saveCurrentView() {
    const name = window.prompt("给这组筛选起个名字", "")?.trim();
    if (!name) return;
    if (BUILTIN_VIEWS.some((v) => v.label === name)) {
      setError(new Error(`「${name}」是内建视图的名字，换一个。`));
      return;
    }
    const next = upsertView(savedViews, name, spec);
    persistSavedViews(next);
    setSavedViews(next);
  }
  function removeView(id: string) {
    const next = savedViews.filter((v) => v.id !== id);
    persistSavedViews(next);
    setSavedViews(next);
  }

  /** 抽屉里打开的那个号。存 id 不存对象 —— 刷完用量后抽屉要跟着更新。 */
  const [openId, setOpenId] = useState<string | null>(null);
  const [adding, setAdding] = useState(false);
  const [authorizing, setAuthorizing] = useState<Account | null>(null);
  /** 切号池 / 网关号池的名单：卡片上那两枚「所在池」小标靠它。 */
  const pools = usePools();
  /**
   * Grok Bot 客户端登着谁（只读缓存 / 凭证文件里的 email，不弹钥匙串）。它登的是 Cursor 账号——
   * 不在库里就给一键收进来，refresh token 和 OAuth 拿到的是同一种东西。
   */
  const [grok, setGrok] = useState<GrokBotStatus | null>(null);
  const [importingGrok, setImportingGrok] = useState(false);

  const reload = useCallback(async () => {
    try {
      setList(await accounts.list());
      setError(null);
    } catch (err) {
      setError(err);
    } finally {
      setLoading(false);
    }
    // 拿不到就当不知道：这只影响一个按钮的措辞，不值得把整页顶成错误态。
    try {
      setInCursor((await switcher.overview()).current?.email?.toLowerCase() ?? null);
    } catch {
      setInCursor(null);
    }
    try {
      setGrok(await grokbot.status());
    } catch {
      setGrok(null);
    }
    await pools.reload();
  }, [pools.reload]);

  /** Grok Bot 登着、库里却没有的那个号（email 已知时才判断得出来）。 */
  const grokMissing = useMemo(() => {
    const email = grok?.activeEmail?.toLowerCase();
    if (!email || !grok?.app.installed) return null;
    return list.some((a) => a.email.toLowerCase() === email) ? null : email;
  }, [grok, list]);

  async function importFromGrok() {
    setImportingGrok(true);
    try {
      const a = await grokbot.importActiveAccount();
      setNotice(`已把 Grok Bot 登着的 ${a.email} 收进账号库。`);
      setError(null);
      await reload();
    } catch (err) {
      setError(err);
    } finally {
      setImportingGrok(false);
    }
  }

  useEffect(() => {
    void reload();
  }, [reload]);

  // 批量刷用量时逐个亮起来，而不是整片转圈。
  useEffect(() => {
    const off = onAccountRefreshed((r) => {
      setRefreshing((prev) => {
        const next = new Set(prev);
        next.delete(r.id);
        return next;
      });
      void reload();
    });
    return () => void off.then((fn) => fn());
  }, [reload]);

  // 授权是后台任务：弹窗关掉之后 token 才到是常事（§2.3 就是这么承诺的）。
  useEffect(() => {
    const off = onOauthState((s) => {
      if (s.state === "succeeded") void reload();
    });
    return () => void off.then((fn) => fn());
  }, [reload]);

  useEffect(() => {
    if (!notice) return;
    const timer = window.setTimeout(() => setNotice(null), 5000);
    return () => window.clearTimeout(timer);
  }, [notice]);

  /** 这一页此刻在看的那一堆：没归档的（默认）或已归档的。两堆从不混在一列里。 */
  const shelf = useMemo(() => list.filter((a) => Boolean(a.archivedAt) === archived), [list, archived]);
  const searched = useMemo(() => shelf.filter((a) => matchesQuery(a, query)), [shelf, query]);
  // 分布条数的是搜索之后、筛子之前的那一批：它是「当前这批号什么光景」的底数，
  // 拿筛完的结果去数，点一下筛子那条横条就只剩自己那一段了。
  const stats = useMemo(() => summarize(searched), [searched]);
  /**
   * 档位下拉的选项与计数。数的是**全量**而不是搜索/筛选后的那批：选项跟着筛子结果走的话，
   * 选中那一档、别的档就从这个下拉里消失了，想换一档得先清筛 —— 兜了一圈。
   * 选中的那档即使此刻一个号都没有也留着，否则按钮上会突然改口叫「档位」。
   */
  const planOptions = useMemo(() => {
    const counts = new Map<PlanFilter, number>();
    let paid = 0;
    for (const a of shelf) {
      const g = accountPlanGroup(a);
      counts.set(g, (counts.get(g) ?? 0) + 1);
      if (isPaidPlan(g)) paid += 1;
    }
    const groups = PLAN_FILTER_ORDER.filter((g) => counts.has(g) || g === plan);
    return [
      { id: "all" as PlanFilter, label: PLAN_FILTER_LABEL.all, meta: shelf.length },
      { id: "paid" as PlanFilter, label: PLAN_FILTER_LABEL.paid, meta: paid },
      ...groups.map((g) => ({ id: g as PlanFilter, label: PLAN_FILTER_LABEL[g], meta: counts.get(g) ?? 0 })),
    ];
  }, [shelf, plan]);
  const quotaOptions = useMemo(() => {
    const all = summarize(shelf);
    return [
      { id: "all" as QuotaFilter, label: "额度：全部", meta: shelf.length },
      ...QUOTA_ORDER.filter((q) => all.quota[q] > 0 || q === quota).map((q) => ({
        id: q as QuotaFilter,
        label: QUOTA_LABEL[q],
        meta: all.quota[q],
      })),
    ];
  }, [shelf, quota]);
  const shown = useMemo(
    () =>
      sortAccounts(
        applyPlanFilter(applyQuotaFilter(applyAvailFilter(searched, avail), quota), plan).filter((a) =>
          matchesPoolFilter(pools.membership(a.email), pool),
        ),
        sort,
      ),
    [searched, avail, quota, plan, pool, sort, pools],
  );
  const activeView = useMemo(() => matchView(spec, savedViews), [spec, savedViews]);
  const openAccount = useMemo(() => list.find((a) => a.id === openId) ?? null, [list, openId]);
  const openView = useMemo(
    () =>
      openAccount
        ? createCursorAccountView({
            label: openAccount.email,
            managed: openAccount,
            placement: { kind: "library", label: "账号库" },
          })
        : null,
    [openAccount],
  );
  // 归档的号不参与批量刷新：收起来就是不想再管它。
  const refreshable = useMemo(() => list.filter((a) => !a.archivedAt && canQueryUsage(a)).length, [list]);
  const narrowed = query.trim() !== "" || !isDefaultView(spec);

  // 列表变了（刷新、归档、删除），选中集只留还在眼前的那些。
  useEffect(() => {
    setSelected((prev) => {
      const visible = new Set(shown.map((a) => a.id));
      const next = new Set([...prev].filter((id) => visible.has(id)));
      return next.size === prev.size ? prev : next;
    });
  }, [shown]);

  function toggleSelect(id: string) {
    setSelected((prev) => {
      const next = new Set(prev);
      if (next.has(id)) next.delete(id);
      else next.add(id);
      return next;
    });
  }
  function exitSelecting() {
    setSelecting(false);
    setSelected(new Set());
  }
  /** 归档 / 取回选中的号。当前看的是没归档的那堆就是归档，看的是已归档那堆就是取回。 */
  async function archiveSelected(toArchive: boolean) {
    const ids = [...selected];
    if (!ids.length) return;
    setArchiving(true);
    try {
      await accounts.setArchived(ids, toArchive);
      setNotice(toArchive ? `已归档 ${ids.length} 个账号，在「已归档」视图里能取回。` : `已取回 ${ids.length} 个账号。`);
      setError(null);
      exitSelecting();
      await reload();
    } catch (err) {
      setError(err);
    } finally {
      setArchiving(false);
    }
  }
  async function refreshSelected() {
    const ids = [...selected].filter((id) => shown.some((a) => a.id === id && canQueryUsage(a)));
    if (!ids.length) return;
    setRefreshing(new Set(ids));
    exitSelecting();
    try {
      await accounts.refreshAll(ids);
    } catch (err) {
      setError(err);
    } finally {
      setRefreshing(new Set());
      await reload();
    }
  }

  async function refreshOne(id: string) {
    setRefreshing((p) => new Set(p).add(id));
    try {
      await accounts.refreshUsage(id);
      setError(null);
    } catch (err) {
      setError(err);
    } finally {
      setRefreshing((p) => {
        const next = new Set(p);
        next.delete(id);
        return next;
      });
      await reload();
    }
  }

  async function refreshAll() {
    const ids = list.filter((a) => !a.archivedAt && canQueryUsage(a)).map((a) => a.id);
    if (!ids.length) return;
    setRefreshing(new Set(ids));
    try {
      await accounts.refreshAll(ids);
    } catch (err) {
      setError(err);
    } finally {
      setRefreshing(new Set());
      await reload();
    }
  }

  /**
   * 把全部账号连凭证导出成一份清单文件。与「批量添加」是同一种文件的两个方向：
   * 换台机器，把这份文件粘进去就回来了。
   */
  async function exportAll() {
    if (exporting) return;
    setExporting(true);
    try {
      setExported(await accounts.exportDump());
      setError(null);
    } catch (err) {
      setError(err);
    } finally {
      setExporting(false);
    }
  }

  async function revealExport(path: string) {
    try {
      await backup.reveal(path);
      setError(null);
    } catch (err) {
      setError(err);
    }
  }

  return (
    <>
      <div className="page-head acct-head">
        {tabs}
        <div className="row">
          {/* 刷新和导出只留图标：两个动作各自的图标已经说清楚了，文案交给悬停时的气泡，
              这样标题行上唯一带字的就是主动作「添加账号」。
              一个号都没有 refresh_token 时「刷新用量」无事可做，别摆一个灰按钮占位。 */}
          {refreshable > 0 ? (
            <button
              type="button"
              className="btn btn-icon"
              data-tip={refreshing.size > 0 ? `刷新中 ${refreshing.size}` : "刷新用量"}
              aria-label="刷新用量"
              disabled={refreshing.size > 0}
              onClick={() => void refreshAll()}
            >
              <Icon name="refresh" size={15} className={refreshing.size ? "is-spinning" : undefined} />
            </button>
          ) : null}
          {shelf.length > 0 ? (
            <button
              type="button"
              className="btn btn-icon"
              aria-pressed={selecting}
              data-tip={selecting ? "退出选择" : "选择多个"}
              aria-label="选择多个账号"
              onClick={() => (selecting ? exitSelecting() : setSelecting(true))}
            >
              <Icon name="check" size={15} />
            </button>
          ) : null}
          {/* 空列表上没什么可导的，不摆。 */}
          {list.length > 0 ? (
            <button
              type="button"
              className="btn btn-icon"
              disabled={exporting}
              data-tip={exporting ? "导出中" : "导出全部账号"}
              aria-label="导出全部账号"
              onClick={() => void exportAll()}
            >
              <Icon name="export" size={15} />
            </button>
          ) : null}
          <button type="button" className="btn btn-primary" onClick={() => setAdding(true)}>
            <Icon name="plus" size={14} />
            添加账号
          </button>
        </div>
      </div>

      <ErrorNote error={error} onRetry={() => void reload()} />
      {exported ? (
        <div style={{ marginBottom: 14 }}>
          <Banner
            tone="warn"
            title={`已导出 ${exported.count} 个账号到 ${exported.path}`}
            hint="文件里是明文凭证（refresh_token、密码），只有你这个用户能读。用完删掉，或放进密码管理器。"
            action={
              <span className="row" style={{ gap: 6 }}>
                <button type="button" className="btn btn-sm" onClick={() => void revealExport(exported.path)}>
                  显示文件
                </button>
                <button type="button" className="btn btn-sm btn-quiet" onClick={() => setExported(null)}>
                  知道了
                </button>
              </span>
            }
          />
        </div>
      ) : null}
      {notice ? (
        <div style={{ marginBottom: 14 }}>
          <Banner
            tone="ok"
            title={notice}
            action={
              <button type="button" className="btn btn-sm btn-quiet" onClick={() => setNotice(null)}>
                知道了
              </button>
            }
          />
        </div>
      ) : null}
      {grokMissing ? (
        <div style={{ marginBottom: 14 }}>
          <Banner
            tone="default"
            title={`Grok Bot 登着 ${grokMissing}，账号库里还没有它。`}
            hint="收进来后可查用量、切号、进各个池。"
            action={
              <button type="button" className="btn btn-sm btn-primary" disabled={importingGrok} onClick={() => void importFromGrok()}>
                {importingGrok ? "收进中…" : "收进账号库"}
              </button>
            }
          />
        </div>
      ) : null}

      {loading ? (
        <div className="accts">
          <div className="skeleton" style={{ height: 164 }} />
          <div className="skeleton" style={{ height: 164 }} />
          <div className="skeleton" style={{ height: 164 }} />
        </div>
      ) : list.length === 0 ? (
        <Empty
          title="还没有账号"
          action={
            <button type="button" className="btn btn-primary" onClick={() => setAdding(true)}>
              <Icon name="plus" size={14} />
              添加账号
            </button>
          }
        />
      ) : (
        <>
          {/* 统一高级控制台：左边是状态胶囊（一键下钻到特定状态），右边是搜索与属性筛选 */}
          <div className="unified-toolbar">
            <div className="status-pills" role="tablist" aria-label="账号状态视图">
              <button
                type="button"
                className={`status-pill${avail === "all" && !archived ? " is-active" : ""}`}
                onClick={() => setSpec({ avail: "all", archived: false })}
              >
                <span>全部</span>
                <span className="pill-n">{stats.total}</span>
              </button>

              <button
                type="button"
                className={`status-pill is-avail${avail === "long_lived" && !archived ? " is-active" : ""}`}
                onClick={() => setSpec({ avail: "long_lived", archived: false })}
              >
                <i className="status-dot is-long_lived" />
                <span>可用</span>
                <span className="pill-n">{stats.by.long_lived}</span>
              </button>

              {stats.by.session > 0 ? (
                <button
                  type="button"
                  className={`status-pill is-session${avail === "session" && !archived ? " is-active" : ""}`}
                  onClick={() => setSpec({ avail: "session", archived: false })}
                >
                  <i className="status-dot is-session" />
                  <span>仅会话</span>
                  <span className="pill-n">{stats.by.session}</span>
                </button>
              ) : null}

              {stats.by.logged_out > 0 ? (
                <button
                  type="button"
                  className={`status-pill is-logged_out${avail === "logged_out" && !archived ? " is-active" : ""}`}
                  onClick={() => setSpec({ avail: "logged_out", archived: false })}
                >
                  <i className="status-dot is-logged_out" />
                  <span>掉登录</span>
                  <span className="pill-n">{stats.by.logged_out}</span>
                </button>
              ) : null}

              {stats.by.dead > 0 ? (
                <button
                  type="button"
                  className={`status-pill is-dead${avail === "dead" && !archived ? " is-active" : ""}`}
                  onClick={() => setSpec({ avail: "dead", archived: false })}
                >
                  <i className="status-dot is-dead" />
                  <span>已失效</span>
                  <span className="pill-n">{stats.by.dead}</span>
                </button>
              ) : null}

              <div className="status-pill-sep" aria-hidden />

              <button
                type="button"
                className={`status-pill${archived ? " is-active is-archived" : ""}`}
                onClick={() => setSpec({ archived: !archived, avail: "all" })}
              >
                <Icon name="archive" size={12} />
                <span>已归档</span>
              </button>
            </div>

            <div className="acct-controls">
              <label className="search">
                <Icon name="search" size={14} />
                <input
                  value={query}
                  onChange={(e) => setQuery(e.target.value)}
                  placeholder="搜索邮箱、备注、标签"
                  spellCheck={false}
                />
                {query ? (
                  <button type="button" className="search-clear" onClick={() => setQuery("")} aria-label="清空">
                    <Icon name="close" size={12} />
                  </button>
                ) : null}
              </label>

              <Picker<QuotaFilter>
                icon="gauge"
                label="额度"
                value={quota}
                options={quotaOptions}
                onChange={(q) => setSpec({ quota: q })}
              />
              <Picker<PlanFilter>
                icon="crown"
                label="档位"
                value={plan}
                options={planOptions}
                onChange={(p) => setSpec({ plan: p })}
              />
              <Picker<PoolFilter>
                icon="layers"
                label="所在池"
                value={pool}
                options={POOL_FILTERS.map((p) => ({ id: p, label: POOL_FILTER_LABEL[p] }))}
                onChange={(p) => setSpec({ pool: p })}
              />
              <i className="acct-controls-sep" aria-hidden />
              <Picker<AccountSort>
                icon="sort"
                label="排序"
                value={sort}
                options={SORTS.map((s) => ({ id: s, label: SORT_LABEL[s] }))}
                onChange={(s) => setSpec({ sort: s })}
              />
              {savedViews.length > 0 ? (
                <Picker<string>
                  icon="bookmark"
                  label="视图"
                  value={activeView?.id ?? ""}
                  options={[
                    { id: "", label: activeView ? activeView.label : "常用视图" },
                    ...savedViews.map((v) => ({ id: v.id, label: v.label })),
                  ]}
                  onChange={(id) => {
                    const v = savedViews.find((x) => x.id === id);
                    if (v) setSpec(v.spec);
                  }}
                />
              ) : null}
              {activeView && !activeView.builtin ? (
                <button
                  type="button"
                  className="btn btn-sm btn-quiet"
                  onClick={() => removeView(activeView.id)}
                  title={`删除视图「${activeView.label}」`}
                  aria-label={`删除视图 ${activeView.label}`}
                >
                  <Icon name="trash" size={12} />
                </button>
              ) : null}
              {narrowed && !activeView ? (
                <button
                  type="button"
                  className="btn btn-sm btn-quiet"
                  onClick={saveCurrentView}
                  title="把当前的筛选和排序存成自定义视图"
                >
                  <Icon name="plus" size={11} />
                  存为视图
                </button>
              ) : null}
            </div>
          </div>

          {selecting ? (
            <SelectBar
              count={selected.size}
              total={shown.length}
              archived={archived}
              busy={archiving}
              onAll={() => setSelected(new Set(shown.map((a) => a.id)))}
              onNone={() => setSelected(new Set())}
              onArchive={() => void archiveSelected(!archived)}
              onRefresh={() => void refreshSelected()}
              onExit={exitSelecting}
            />
          ) : null}

          {shown.length === 0 ? (
            <Empty
              title={archived && !narrowed ? "没有归档的账号" : "没有匹配的账号"}
              action={
                narrowed ? (
                  <button
                    type="button"
                    className="btn btn-sm"
                    onClick={() => {
                      setQuery("");
                      setSpec({ avail: "all", quota: "all", pool: "any", plan: "all" });
                    }}
                  >
                    清除筛选
                  </button>
                ) : undefined
              }
            />
          ) : (
            <div className="accts">
              {shown.map((a) => {
                const refreshingAccount = refreshing.has(a.id);
                const picked = selected.has(a.id);
                return (
                  <AccountCard
                    key={a.id}
                    view={createCursorAccountView({
                      label: a.email,
                      managed: a,
                      placement: { kind: "library", label: "账号库" },
                    })}
                    highlighted={selecting ? picked : a.id === openId}
                    onOpen={selecting ? () => toggleSelect(a.id) : () => setOpenId(a.id)}
                    badges={<PoolChips membership={pools.membership(a.email)} />}
                    actions={
                      selecting ? (
                        <span className={`acct-pick${picked ? " is-on" : ""}`} aria-hidden>
                          {picked ? <Icon name="check" size={11} /> : null}
                        </span>
                      ) : (
                      <>
                        {canQueryUsage(a) ? (
                          // 一屏几十张卡就是几十枚这个键。做成实键，卡的右下角全是小方块；
                          // 它平时只是一个灰图标，停上去才显出边框。
                          <button
                            type="button"
                            className="ibtn"
                            disabled={refreshingAccount}
                            onClick={() => void refreshOne(a.id)}
                            aria-label="刷新用量"
                          >
                            <Icon
                              name="refresh"
                              size={13}
                              className={refreshingAccount ? "is-spinning" : undefined}
                            />
                          </button>
                        ) : (
                          <button
                            type="button"
                            className="btn btn-sm btn-soft"
                            onClick={() => setAuthorizing(a)}
                          >
                            授权
                          </button>
                        )}
                      </>
                      )
                    }
                  />
                );
              })}
            </div>
          )}
        </>
      )}

      {openView ? (
        <AccountInspector
          view={openView}
          inCursor={openView.label.toLowerCase() === inCursor}
          onClose={() => setOpenId(null)}
          onChanged={reload}
          onSwitch={() => onGo(go("switcher", { email: openView.label }))}
        />
      ) : null}

      {adding ? (
        <AddAccountModal
          onClose={() => setAdding(false)}
          onAdded={async () => {
            setAdding(false);
            await reload();
          }}
          onImported={async (n) => {
            setAdding(false);
            setNotice(`已导入 ${n} 个账号。`);
            await reload();
          }}
        />
      ) : null}

      {authorizing ? (
        <AuthorizeModal
          account={authorizing}
          onClose={() => {
            setAuthorizing(null);
            void reload();
          }}
        />
      ) : null}
    </>
  );
}

/* ── 多选操作条 ───────────────────────────────────────────────────────────── */

/**
 * 按下「选择」后出现在列表上方：选了几个、全选 / 清空，以及能对这一批做的事。
 * 动作只有归档（或取回）和刷新用量 —— 删除故意不放：它不可逆，一个个删让人多想一秒。
 */
function SelectBar({
  count,
  total,
  archived,
  busy,
  onAll,
  onNone,
  onArchive,
  onRefresh,
  onExit,
}: {
  count: number;
  total: number;
  archived: boolean;
  busy: boolean;
  onAll: () => void;
  onNone: () => void;
  onArchive: () => void;
  onRefresh: () => void;
  onExit: () => void;
}) {
  return (
    <div className="selbar" role="toolbar" aria-label="批量操作">
      <span className="selbar-n">
        已选 <b className="num">{count}</b> / {total}
      </span>
      <button type="button" className="btn btn-sm btn-quiet" onClick={count === total ? onNone : onAll}>
        {count === total ? "清空" : "全选"}
      </button>
      <span className="grow" />
      {!archived ? (
        <button type="button" className="btn btn-sm" disabled={count === 0 || busy} onClick={onRefresh}>
          <Icon name="refresh" size={13} />
          刷新用量
        </button>
      ) : null}
      <button type="button" className="btn btn-sm btn-primary" disabled={count === 0 || busy} onClick={onArchive}>
        <Icon name={archived ? "undo" : "archive"} size={13} />
        {archived ? "取回" : "归档"}
      </button>
      <button type="button" className="btn btn-sm btn-quiet" onClick={onExit} aria-label="退出选择">
        <Icon name="close" size={12} />
      </button>
    </div>
  );
}
