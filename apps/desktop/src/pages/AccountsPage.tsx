/**
 * 我的账号（ARCHITECTURE §5.1）。
 *
 * 顶部按**平台**分页签：Cursor、ChatGPT、Grok Build、Kiro。号形状不同（Cursor 的能切进 IDE、
 * 进三种池；其余只喂本机网关），混在一列里两边都看不懂，所以各占一页签。
 * 地址是 `#accounts` / `#accounts/chatgpt` / `#accounts/grok` / `#accounts/kiro`。
 *
 * Cursor 那一页签：一列账号卡负责「扫」，右侧抽屉负责「看」。几十个号的时候，用户进来通常带着一个
 * 具体问题 ——「哪个还能用」「哪个快重置了」「哪些掉了登录」—— 所以工具栏左边是一排按**可用性**
 * 分档的胶囊（每一枚既是计数也是筛子），右边是搜索和额度 / 档位 / 所在池 / 分组四个筛子加排序。
 * 数字不单独摆一排：它们长在各自的筛子上，数的都是「点下去会剩几个」（见 `applyFacets`）。
 * 筛选 / 排序的组合会记住，也能起名存成视图（`accounts/views.ts`），常问的问题一键就回到那一组筛子。
 *
 * 归档：多选几个号「归档」，它们就从默认列表消失、不进网关候选；切到「已归档」还能看、
 * 能取回、也能刷用量。凭证一个字节不动。
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
import {
  accounts,
  backup,
  grokbot,
  onAccountProvisioned,
  onAccountRefreshed,
  onOauthState,
  switcher,
} from "../ipc/api";
import type {
  Account,
  ExportOutcome,
  GrokBotStatus,
  ProvisionPlan,
  ProvisionReport,
} from "../ipc/types";
import { ACCOUNT_PLATFORMS, go, type AccountPlatform, type Route } from "../shell/nav";
import { maskEmail } from "../ui/format";
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

import { COPY_FORMATS, copyInfoMap, loadCopyChoice, saveCopyChoice, type CopyChoice } from "../accounts/copy";
import { loadProvisionPlan, saveProvisionPlan } from "../accounts/provision";
import { looksLikeLookupPaste, matchLookup } from "../accounts/lookup";
import {
  accountPlanGroup,
  applyFacets,
  AVAIL_LABEL,
  AVAIL_ORDER,
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
  type AccountFacets,
  type AccountSort,
  type PlanFilter,
  type QuotaFilter,
} from "../ui/accounts";
import { Banner, Empty, ErrorNote, Icon, Picker } from "../ui/primitives";
import { AddAccountModal } from "./accounts/AddAccountModal";
import { AuthorizeModal } from "./accounts/AuthorizeModal";
import { CopySelectedModal } from "./accounts/CopySelectedModal";
import { LookupModal } from "./accounts/LookupModal";
import { ProvisionModal } from "./accounts/ProvisionModal";
import { ChatGptAccounts } from "./accounts/ChatGptAccounts";
import { GrokAccounts, KiroAccounts, ZcodeAccounts } from "./accounts/DeviceAccounts";

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
      ) : platform === "zcode" ? (
        <ZcodeAccounts tabs={tabs} onGo={onGo} />
      ) : (
        <CursorAccounts tabs={tabs} onGo={onGo} />
      )}
    </div>
  );
}

function CursorAccounts({ tabs, onGo }: { tabs: ReactNode; onGo: (r: Route) => void }) {
  const [list, setList] = useState<Account[]>([]);
  /** Cursor 此刻登录的邮箱（小写）：卡片高亮「当前登录」，抽屉里禁用切号。 */
  const [inCursor, setInCursor] = useState<string | null>(null);
  const [error, setError] = useState<unknown>(null);
  const [loading, setLoading] = useState(true);
  const [refreshing, setRefreshing] = useState<Set<string>>(new Set());
  const [notice, setNotice] = useState<string | null>(null);
  const [exporting, setExporting] = useState(false);
  /** 最近一次导出。不自动消失：这条横幅上写着「文件里是明文凭证」，得让人看见。 */
  const [exported, setExported] = useState<ExportOutcome | null>(null);

  const [query, setQuery] = useState("");
  // 批量查找：`lookup` 是正在生效的邮箱清单（替代关键字搜索），`lookingUp` 是弹窗（带预填文本）。
  const [lookup, setLookup] = useState<string[] | null>(null);
  const [lookingUp, setLookingUp] = useState<{ text: string } | null>(null);
  const [missingOpen, setMissingOpen] = useState(false);
  /**
   * 筛选 + 排序的当前组合。四个筛子各管一维（可用性 / 额度 / 所在池 / 档位），互不相干，可以同时下；
   * 整组落盘，下次进来还在原地。
   */
  const [spec, setSpecState] = useState<ViewSpec>(loadSpec);
  const [savedViews, setSavedViews] = useState<SavedView[]>(loadSavedViews);
  const { avail, quota, pool, plan, tag = "all", sort, archived } = spec;
  /** 多选：只在按下「选择」后出现，选完做一件事（归档 / 取回 / 刷新）就退出。 */
  const [selecting, setSelecting] = useState(false);
  const [selected, setSelected] = useState<Set<string>>(new Set());
  /** 只看已选：把列表收成选中的那几个。挑一批出来截图时，别把没选的也拍进去。 */
  const [previewOnly, setPreviewOnly] = useState(false);
  const [archiving, setArchiving] = useState(false);
  // 复制弹窗：开着时记着上次选的格式与附加项；复制中禁掉按钮，别连点两次。
  const [copying, setCopying] = useState(false);
  const [copyBusy, setCopyBusy] = useState(false);
  const [copyChoice, setCopyChoice] = useState<CopyChoice>(loadCopyChoice);
  /**
   * 自动配置弹窗。`provisionIds` 是这一轮要配的号（多选那批，或刚导入那批），`provisionReports`
   * 是后端逐个推回来的结果。跑起来后弹窗不换界面，原地长出进度。
   */
  const [provisionIds, setProvisionIds] = useState<string[] | null>(null);
  const [provisionBusy, setProvisionBusy] = useState(false);
  const [provisionReports, setProvisionReports] = useState<ProvisionReport[]>([]);
  const [provisionPlan, setProvisionPlan] = useState<ProvisionPlan>(loadProvisionPlan);

  /** 账号打码开关：点击小眼睛切换明文 / 打码展示。 */
  const [masked, setMasked] = useState(() => {
    try {
      return localStorage.getItem("nexus.accounts.masked") === "1";
    } catch {
      return false;
    }
  });

  const toggleMasked = () => {
    setMasked((prev) => {
      const next = !prev;
      try {
        localStorage.setItem("nexus.accounts.masked", next ? "1" : "0");
      } catch {}
      return next;
    });
  };
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

  // 批量自动配置：逐个把结果填进弹窗。列表统一在整批跑完后刷一次——每个号都 reload 一遍，
  // 几十个号就是几十次全量查库，弹窗那边已经在实时显示了。
  useEffect(() => {
    const off = onAccountProvisioned((r) => {
      setProvisionReports((prev) => [...prev, r]);
    });
    return () => void off.then((fn) => fn());
  }, []);

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

  /**
   * 这一页此刻在看的那一堆：没归档的（默认）或已归档的。两堆从不混在一列里 ——
   * 除了批量查找：清单上的号在哪堆都得找出来，归档的在卡上另标一枚「已归档」。
   */
  const shelf = useMemo(
    () => (lookup ? list : list.filter((a) => Boolean(a.archivedAt) === archived)),
    [list, archived, lookup],
  );
  const lookupResult = useMemo(() => (lookup ? matchLookup(list, lookup) : null), [list, lookup]);
  const searched = useMemo(
    () => (lookupResult ? lookupResult.found : shelf.filter((a) => matchesQuery(a, query))),
    [shelf, query, lookupResult],
  );
  const allTags = useMemo(() => {
    const set = new Set<string>();
    for (const a of list) {
      for (const t of a.tags ?? []) {
        if (t && t.trim()) set.add(t.trim());
      }
    }
    return Array.from(set).sort();
  }, [list]);

  /**
   * 五个筛子拢成一组。「所在池」要查两份名单、「分组」的候选是用户自己起的标签名，
   * 所以这两维给的是判定函数（`ui/accounts.ts` 的 `AccountFacets`）。
   */
  const facets = useMemo<AccountFacets>(
    () => ({
      avail,
      quota,
      plan,
      inPool: (a) => matchesPoolFilter(pools.membership(a.email), pool),
      hasTag: (a) => {
        if (!tag || tag === "all") return true;
        if (tag === "_untagged") return !a.tags || a.tags.length === 0;
        return a.tags?.includes(tag) ?? false;
      },
    }),
    [avail, quota, plan, pool, tag, pools],
  );

  const shown = useMemo(() => {
    const filtered = applyFacets(searched, facets);
    // 批量查找的结果按清单的顺序排，不套排序：对着自己手里那份一行行核对，顺序一乱就对不上。
    return lookup ? filtered : sortAccounts(filtered, sort);
  }, [searched, facets, sort, lookup]);

  /**
   * 真正铺在页面上的那几张卡。开着「只看已选」时是选中的那批，否则就是 `shown`。
   *
   * 这一层只管显示，`shown` 仍是「筛完之后的全集」：操作条上的 x / y、全选的目标、
   * 以及选中集的剪枝都还照着全集算 —— 否则一开预览，count 就等于 total，「全选」当场
   * 变成「清空」，按下去列表直接空掉。
   */
  const visible = useMemo(
    () => (selecting && previewOnly ? shown.filter((a) => selected.has(a.id)) : shown),
    [shown, selecting, previewOnly, selected],
  );

  /*
   * 每个筛子上的数都是「点下去会剩几个」：其余几维照旧下上，自己那一维跳过
   * （`applyFacets` 的 except）。这排数字以前数的是搜索之后、筛子之前的底数，
   * 于是下了额度或档位的筛子之后，顶上写着 40 个、眼前只剩 6 张卡 —— 对不上的数字没人信。
   * 自己那一维跳过是为了还能换档：把它也算上，选中一档后别的档全成 0，想换得先清筛。
   */
  const stats = useMemo(() => summarize(applyFacets(searched, facets, "avail")), [searched, facets]);
  /** 档位下拉。选中的那档即使此刻一个号都没有也留着，否则按钮上会突然改口叫「档位」。 */
  const planOptions = useMemo(() => {
    const candidates = applyFacets(searched, facets, "plan");
    const counts = new Map<PlanFilter, number>();
    let paid = 0;
    for (const a of candidates) {
      const g = accountPlanGroup(a);
      counts.set(g, (counts.get(g) ?? 0) + 1);
      if (isPaidPlan(g)) paid += 1;
    }
    const groups = PLAN_FILTER_ORDER.filter((g) => counts.has(g) || g === plan);
    return [
      { id: "all" as PlanFilter, label: PLAN_FILTER_LABEL.all, meta: candidates.length },
      { id: "paid" as PlanFilter, label: PLAN_FILTER_LABEL.paid, meta: paid },
      ...groups.map((g) => ({ id: g as PlanFilter, label: PLAN_FILTER_LABEL[g], meta: counts.get(g) ?? 0 })),
    ];
  }, [searched, facets, plan]);
  const quotaOptions = useMemo(() => {
    const s = summarize(applyFacets(searched, facets, "quota"));
    return [
      { id: "all" as QuotaFilter, label: "额度：全部", meta: s.total },
      ...QUOTA_ORDER.filter((q) => s.quota[q] > 0 || q === quota).map((q) => ({
        id: q as QuotaFilter,
        label: QUOTA_LABEL[q],
        meta: s.quota[q],
      })),
    ];
  }, [searched, facets, quota]);
  const poolOptions = useMemo(() => {
    const candidates = applyFacets(searched, facets, "pool");
    return POOL_FILTERS.map((p) => ({
      id: p,
      label: POOL_FILTER_LABEL[p],
      meta: candidates.filter((a) => matchesPoolFilter(pools.membership(a.email), p)).length,
    }));
  }, [searched, facets, pools]);
  const tagOptions = useMemo(() => {
    const candidates = applyFacets(searched, facets, "tag");
    return [
      { id: "all", label: "全部", meta: candidates.length },
      ...allTags.map((t) => ({
        id: t,
        label: t,
        meta: candidates.filter((a) => a.tags?.includes(t)).length,
      })),
      { id: "_untagged", label: "未分组", meta: candidates.filter((a) => !a.tags || a.tags.length === 0).length },
    ];
  }, [searched, facets, allTags]);
  /**
   * 「已归档」那一枚上的数：点下去会看到几个。归档的号是另一堆（`shelf` 二选一），
   * 所以单独数一次；点它会把可用性复位成「全部」，这里也就跳过那一维。
   */
  const archivedCount = useMemo(() => {
    if (lookupResult) return lookupResult.archived.length;
    const box = list.filter((a) => Boolean(a.archivedAt) && matchesQuery(a, query));
    return applyFacets(box, facets, "avail").length;
  }, [list, query, facets, lookupResult]);
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
  // 刷的是眼前这批：筛子 / 搜索 / 归档视图下只动看得见的号，别把别的档也带上。
  const refreshTargets = useMemo(() => visible.filter((a) => canQueryUsage(a)).map((a) => a.id), [visible]);
  const refreshable = refreshTargets.length;
  const narrowed = query.trim() !== "" || lookup != null || !isDefaultView(spec);

  // 选中的号按列表顺序排：复制出来的顺序就是眼前看到的顺序。
  const selectedAccounts = useMemo(() => shown.filter((a) => selected.has(a.id)), [shown, selected]);

  /**
   * 弹窗里要配的那几个号。从**全量列表**里找而不是 `shown`：刚导入的那批很可能被当前筛子
   * 挡着（正看着「已归档」、或搜索框里还有字），但它们确实是要配的那几个。
   */
  const provisionAccounts = useMemo(() => {
    if (!provisionIds) return null;
    const want = new Set(provisionIds);
    return list.filter((a) => want.has(a.id));
  }, [provisionIds, list]);

  /** 按清单找号。找到的替代关键字搜索；要刷新就把库里有的、能查的那几个一起刷。 */
  function runLookup(emails: string[], refresh: boolean) {
    setLookingUp(null);
    setQuery("");
    setMissingOpen(false);
    setLookup(emails);
    if (refresh) {
      const ids = matchLookup(list, emails)
        .found.filter((a) => canQueryUsage(a))
        .map((a) => a.id);
      if (ids.length) void refreshIds(ids);
    }
  }
  function clearLookup() {
    setLookup(null);
    setMissingOpen(false);
  }
  /** 把眼前这批查找结果全部选上，进多选：接着复制 / 归档 / 刷新都是一步。 */
  function selectLookupResults() {
    setSelecting(true);
    setSelected(new Set(shown.map((a) => a.id)));
  }

  async function copySelected(choice: CopyChoice) {
    if (!selectedAccounts.length) return;
    setCopyBusy(true);
    try {
      const ids = selectedAccounts.map((a) => a.id);
      const info = copyInfoMap(selectedAccounts, choice.extras);
      const text = await accounts.copySelected(ids, choice.format, info);
      await navigator.clipboard.writeText(text);
      saveCopyChoice(choice);
      setCopyChoice(choice);
      const label = COPY_FORMATS.find((f) => f.id === choice.format)?.label ?? "信息";
      const withInfo = choice.extras.length ? "（带说明）" : "";
      setNotice(`已复制 ${ids.length} 个账号的${label}${withInfo}到剪贴板。`);
      setError(null);
      setCopying(false);
    } catch (err) {
      setError(err);
    } finally {
      setCopyBusy(false);
    }
  }

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
    setPreviewOnly(false);
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
  /**
   * 给一批号跑自动配置。
   *
   * 逐个结果走 `accounts://provisioned` 事件填进弹窗（见上面那个 effect），所以这里只管起头、
   * 收尾、和整批级别的错误（闸被占着、计划全空）。跑完统一 `reload()`：这几步改的是 has_refresh /
   * has_api_key / 用量，全都是卡片上显示的东西。
   */
  async function provisionIdsNow(ids: string[], plan: ProvisionPlan) {
    if (!ids.length) return;
    saveProvisionPlan(plan);
    setProvisionPlan(plan);
    setProvisionBusy(true);
    setProvisionReports([]);
    try {
      await accounts.provisionAll(ids, plan);
      setError(null);
    } catch (err) {
      setError(err);
      setProvisionIds(null);
    } finally {
      setProvisionBusy(false);
      await reload();
    }
  }

  async function refreshSelected() {
    const ids = [...selected].filter((id) => shown.some((a) => a.id === id && canQueryUsage(a)));
    if (!ids.length) return;
    exitSelecting();
    await refreshIds(ids);
  }

  /** 刷一批号的用量。调用方负责挑出能查的那几个。 */
  async function refreshIds(ids: string[]) {
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
    await refreshIds(refreshTargets);
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
              data-tip={
                refreshing.size > 0
                  ? `刷新中 ${refreshing.size}`
                  : narrowed || archived
                    ? `刷新当前 ${refreshable} 个账号的用量`
                    : "刷新用量"
              }
              aria-label={narrowed || archived ? `刷新当前 ${refreshable} 个账号的用量` : "刷新用量"}
              disabled={refreshing.size > 0}
              onClick={() => void refreshAll()}
            >
              <Icon name="refresh" size={15} className={refreshing.size ? "is-spinning" : undefined} />
            </button>
          ) : null}
          <button
            type="button"
            className={`btn btn-icon${masked ? " is-active" : ""}`}
            data-tip={masked ? "显示邮箱" : "隐藏邮箱"}
            aria-label={masked ? "显示邮箱" : "隐藏邮箱"}
            onClick={toggleMasked}
          >
            <Icon name={masked ? "eyeOff" : "eye"} size={15} />
          </button>
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
            title={`Grok Bot 登着 ${masked ? maskEmail(grokMissing) : grokMissing}，账号库里还没有它。`}
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

              {/* 一档一枚，顺序照 AVAIL_ORDER（从好到坏）。空的那一档不占位 ——
                  但「可用」和正筛着的那一档留着：前者是这排的主角，后者一消失就把自己的
                  筛子也带走了，用户会以为号丢了。 */}
              {AVAIL_ORDER.filter((k) => stats.by[k] > 0 || k === "long_lived" || avail === k).map((k) => (
                <button
                  key={k}
                  type="button"
                  className={`status-pill${avail === k && !archived ? " is-active" : ""}`}
                  onClick={() => setSpec({ avail: k, archived: false })}
                >
                  <i className={`status-dot is-${k}`} />
                  <span>{AVAIL_LABEL[k]}</span>
                  <span className="pill-n">{stats.by[k]}</span>
                </button>
              ))}

              <div className="status-pill-sep" aria-hidden />

              <button
                type="button"
                className={`status-pill${archived ? " is-active is-archived" : ""}`}
                onClick={() => setSpec({ archived: !archived, avail: "all" })}
              >
                <Icon name="archive" size={12} />
                <span>已归档</span>
                {archivedCount > 0 ? <span className="pill-n">{archivedCount}</span> : null}
              </button>
            </div>

            <div className="acct-controls">
              {lookup && lookupResult ? (
                // 清单生效时搜索框让位给一枚标签：清单和关键字不叠加，一次只按一种方式找。
                <div className="search is-lookup" role="status">
                  <Icon name="clipboard" size={14} />
                  <button
                    type="button"
                    className="search-lookup"
                    onClick={() => setLookingUp({ text: lookup.join("\n") })}
                    title="批量查找中 · 点击改清单"
                  >
                    清单 {lookup.length} · 找到 {lookupResult.found.length}
                  </button>
                  <button type="button" className="search-clear" onClick={clearLookup} aria-label="清除批量查找">
                    <Icon name="close" size={12} />
                  </button>
                </div>
              ) : (
                <label className="search">
                  <Icon name="search" size={14} />
                  <input
                    value={query}
                    onChange={(e) => setQuery(e.target.value)}
                    onPaste={(e) => {
                      // 粘进来的是一份清单（两个以上邮箱）就别当关键字搜了，直接转批量查找。
                      const text = e.clipboardData.getData("text");
                      if (looksLikeLookupPaste(text)) {
                        e.preventDefault();
                        setLookingUp({ text });
                      }
                    }}
                    placeholder="搜索邮箱、备注、标签"
                    spellCheck={false}
                  />
                  {query ? (
                    <button type="button" className="search-clear" onClick={() => setQuery("")} aria-label="清空">
                      <Icon name="close" size={12} />
                    </button>
                  ) : null}
                </label>
              )}
              <button
                type="button"
                className={`btn btn-icon${lookup ? " is-active" : ""}`}
                data-tip="批量查找：粘一份邮箱清单"
                aria-label="批量查找"
                onClick={() => setLookingUp({ text: lookup?.join("\n") ?? "" })}
              >
                <Icon name="clipboard" size={14} />
              </button>

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
                options={poolOptions}
                onChange={(p) => setSpec({ pool: p })}
              />
              {allTags.length > 0 ? (
                <Picker<string>
                  icon="folder"
                  label="分组"
                  value={tag}
                  options={tagOptions}
                  onChange={(t) => setSpec({ tag: t })}
                />
              ) : null}
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

          {lookup && lookupResult ? (
            <LookupBar
              total={lookup.length}
              found={lookupResult.found.length}
              shown={shown.length}
              archived={lookupResult.archived.length}
              missing={lookupResult.missing}
              missingOpen={missingOpen}
              masked={masked}
              busy={refreshing.size > 0}
              onToggleMissing={() => setMissingOpen((v) => !v)}
              onCopyMissing={async () => {
                try {
                  await navigator.clipboard.writeText(lookupResult.missing.join("\n"));
                  setNotice(`已复制 ${lookupResult.missing.length} 个未找到的邮箱。`);
                } catch (err) {
                  setError(err);
                }
              }}
              onSelectAll={selecting ? undefined : selectLookupResults}
              onRefresh={() => void refreshIds(shown.filter((a) => canQueryUsage(a)).map((a) => a.id))}
              onClear={clearLookup}
            />
          ) : null}

          {selecting ? (
            <SelectBar
              count={selected.size}
              total={shown.length}
              archived={archived}
              busy={archiving}
              previewOnly={previewOnly}
              onTogglePreview={() => setPreviewOnly((v) => !v)}
              onAll={() => setSelected(new Set(shown.map((a) => a.id)))}
              onNone={() => setSelected(new Set())}
              onArchive={() => void archiveSelected(!archived)}
              onRefresh={() => void refreshSelected()}
              onCopy={() => setCopying(true)}
              onProvision={() => setProvisionIds([...selected])}
              onExit={exitSelecting}
            />
          ) : null}

          {visible.length === 0 ? (
            <Empty
              title={
                previewOnly && selecting
                  ? "一个号都还没选"
                  : lookup
                    ? lookupResult?.found.length
                      ? "找到的号都被筛子挡住了"
                      : "清单上的号一个都不在库里"
                    : archived && !narrowed
                      ? "没有归档的账号"
                      : "没有匹配的账号"
              }
              action={
                previewOnly && selecting ? (
                  <button type="button" className="btn btn-sm" onClick={() => setPreviewOnly(false)}>
                    显示全部
                  </button>
                ) : narrowed ? (
                  <button
                    type="button"
                    className="btn btn-sm"
                    onClick={() => {
                      setQuery("");
                      clearLookup();
                      setSpec({ avail: "all", quota: "all", pool: "any", plan: "all", tag: "all" });
                    }}
                  >
                    清除筛选
                  </button>
                ) : undefined
              }
            />
          ) : (
            <div className="accts">
              {visible.map((a) => {
                const refreshingAccount = refreshing.has(a.id);
                const picked = selected.has(a.id);
                const usingCursor = Boolean(inCursor && a.email.toLowerCase() === inCursor);
                return (
                  <AccountCard
                    key={a.id}
                    view={createCursorAccountView({
                      label: a.email,
                      displayLabel: masked ? maskEmail(a.email) : undefined,
                      managed: a,
                      placement: { kind: "library", label: "账号库" },
                    })}
                    highlighted={selecting ? picked : a.id === openId}
                    current={usingCursor}
                    onOpen={selecting ? () => toggleSelect(a.id) : () => setOpenId(a.id)}
                    badges={
                      <>
                        {/* 批量查找把两堆混在一列里了，归档的那几个得标出来。 */}
                        {lookup && a.archivedAt ? <span className="pill">已归档</span> : null}
                        <PoolChips membership={pools.membership(a.email)} />
                      </>
                    }
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
          existingTags={allTags}
          onClose={() => setAdding(false)}
          onAdded={async () => {
            setAdding(false);
            await reload();
          }}
          onImported={async (outcome, provision) => {
            setAdding(false);
            setNotice(`已导入 ${outcome.imported} 个账号。`);
            await reload();
            // 勾了「顺手配置」就直接开跑，不再让人确认一遍：勾的时候已经是确认了。
            if (provision && outcome.ids.length) {
              setProvisionIds(outcome.ids);
              void provisionIdsNow(outcome.ids, provisionPlan);
            }
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

      {lookingUp ? (
        <LookupModal initialText={lookingUp.text} onClose={() => setLookingUp(null)} onLookup={runLookup} />
      ) : null}

      {provisionAccounts ? (
        <ProvisionModal
          accounts={provisionAccounts}
          initial={provisionPlan}
          running={provisionBusy}
          reports={provisionReports}
          masked={masked}
          onClose={() => {
            // 跑完了才允许关（弹窗自己 disable 了取消键），这里顺手把上一轮的结果清掉。
            setProvisionIds(null);
            setProvisionReports([]);
            if (selecting) exitSelecting();
          }}
          onStart={(plan) => void provisionIdsNow(provisionAccounts.map((a) => a.id), plan)}
        />
      ) : null}

      {copying ? (
        <CopySelectedModal
          accounts={selectedAccounts}
          initial={copyChoice}
          busy={copyBusy}
          masked={masked}
          onClose={() => setCopying(false)}
          onCopy={(choice) => void copySelected(choice)}
        />
      ) : null}
    </>
  );
}

/* ── 批量查找结果栏 ─────────────────────────────────────────────────────── */

/**
 * 清单生效时挂在列表上方：清单几个、找到几个、几个已归档、几个没找到。
 * 没找到的能展开看、能整份复制 —— 那份名单往往要回去问人「这几个号是不是给错了」。
 * 动作：全选这些（进多选，接着复制 / 归档一步到位）、刷新用量、清除。
 */
function LookupBar({
  total,
  found,
  shown,
  archived,
  missing,
  missingOpen,
  masked,
  busy,
  onToggleMissing,
  onCopyMissing,
  onSelectAll,
  onRefresh,
  onClear,
}: {
  total: number;
  found: number;
  /** 找到之后又被筛子筛剩的数：和 found 不一样时提醒一句。 */
  shown: number;
  archived: number;
  missing: string[];
  missingOpen: boolean;
  masked: boolean;
  busy: boolean;
  onToggleMissing: () => void;
  onCopyMissing: () => void;
  onSelectAll?: () => void;
  onRefresh: () => void;
  onClear: () => void;
}) {
  const allFound = missing.length === 0;
  return (
    <div className={`lookupbar${allFound ? " is-complete" : ""}`} role="region" aria-label="批量查找结果">
      <div className="lookupbar-row">
        <span className="lookupbar-n">
          清单 <b className="num">{total}</b> 个 · 找到 <b className="num">{found}</b>
          {archived > 0 ? (
            <>
              {" "}
              · <span className="faint">{archived} 个已归档</span>
            </>
          ) : null}
          {shown !== found ? (
            <>
              {" "}
              · <span className="faint">筛子留下 {shown}</span>
            </>
          ) : null}
        </span>
        {missing.length > 0 ? (
          <button type="button" className="lookupbar-missing" aria-expanded={missingOpen} onClick={onToggleMissing}>
            <i className="status-dot is-logged_out" />
            {missing.length} 个未找到
            <i className={`lookupbar-caret${missingOpen ? " is-open" : ""}`} aria-hidden />
          </button>
        ) : (
          <span className="lookupbar-ok">全部找到</span>
        )}

        <span className="grow" />

        {onSelectAll && shown > 0 ? (
          <button type="button" className="btn btn-sm" onClick={onSelectAll}>
            <Icon name="check" size={13} />
            全选这些
          </button>
        ) : null}
        {shown > 0 ? (
          <button type="button" className="btn btn-sm" disabled={busy} onClick={onRefresh}>
            <Icon name="refresh" size={13} className={busy ? "is-spinning" : undefined} />
            刷新用量
          </button>
        ) : null}
        <button type="button" className="btn btn-sm btn-quiet" onClick={onClear} aria-label="清除批量查找">
          <Icon name="close" size={12} />
        </button>
      </div>

      {missingOpen && missing.length > 0 ? (
        <div className="lookupbar-list">
          <div className="lookupbar-emails mono">
            {missing.map((m) => (
              <span key={m}>{masked ? maskEmail(m) : m}</span>
            ))}
          </div>
          <button type="button" className="btn btn-sm btn-quiet" onClick={onCopyMissing} title="把没找到的邮箱复制成一行一个">
            <Icon name="copy" size={12} />
            复制这 {missing.length} 个
          </button>
        </div>
      ) : null}
    </div>
  );
}

/* ── 多选操作条 ───────────────────────────────────────────────────────────── */

/**
 * 按下「选择」后出现在列表上方：选了几个、全选 / 清空，以及能对这一批做的事。
 * 动作是复制、刷新用量、归档（或取回）。归档视图里也能刷用量——收起来不等于用量过期了。
 * 删除故意不放：它不可逆，一个个删让人多想一秒。
 */
function SelectBar({
  count,
  total,
  archived,
  busy,
  previewOnly,
  onTogglePreview,
  onAll,
  onNone,
  onArchive,
  onRefresh,
  onCopy,
  onProvision,
  onExit,
}: {
  count: number;
  total: number;
  archived: boolean;
  busy: boolean;
  previewOnly: boolean;
  onTogglePreview: () => void;
  onAll: () => void;
  onNone: () => void;
  onArchive: () => void;
  onRefresh: () => void;
  onCopy: () => void;
  onProvision: () => void;
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
      {/* 挑一批出来截图时，把没选中的收起来。只改显示，选中集和上面那个 x / y 都不动。 */}
      <button
        type="button"
        className={`btn btn-sm ${previewOnly ? "btn-soft" : "btn-quiet"}`}
        aria-pressed={previewOnly}
        disabled={count === 0 && !previewOnly}
        onClick={onTogglePreview}
        title={previewOnly ? "把没选中的账号也显示回来" : "列表里只留下选中的账号，方便截图"}
      >
        <Icon name={previewOnly ? "eyeOff" : "eye"} size={13} />
        只看已选
      </button>

      <span className="grow" />

      {/* 格式和附带说明在弹窗里选：操作条上放一个下拉，选项一多就挤不下、也讲不清。 */}
      <button
        type="button"
        className="btn btn-sm"
        disabled={count === 0 || busy}
        onClick={onCopy}
        title="选择格式与附带说明后复制到剪贴板"
      >
        <Icon name="copy" size={13} />
        复制…
      </button>

      {/* 铸 key / 换 session / 开按需 / 数据保留 / 刷用量。哪几件在弹窗里勾：
          这批号新旧不一，需要的步骤本来就不一样，操作条上放不下也讲不清。 */}
      <button
        type="button"
        className="btn btn-sm"
        disabled={count === 0 || busy}
        onClick={onProvision}
        title="铸 crsr_ Key、换桌面 session、按需开到不封顶、开数据保留、刷用量"
      >
        <Icon name="settings" size={13} />
        配置…
      </button>

      <button type="button" className="btn btn-sm" disabled={count === 0 || busy} onClick={onRefresh}>
        <Icon name="refresh" size={13} />
        刷新用量
      </button>
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
