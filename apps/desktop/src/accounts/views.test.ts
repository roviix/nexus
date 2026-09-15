import { describe, expect, it } from "vitest";
import type { Account } from "../ipc/types";
import { applyAvailFilter, applyPlanFilter, canQueryUsage, canUseDashboard, hasLiveAccess } from "../ui/accounts";
import {
  BUILTIN_VIEWS,
  DEFAULT_VIEW,
  isDefaultView,
  loadSavedViews,
  loadSpec,
  matchView,
  normalizeSpec,
  persistSavedViews,
  saveSpec,
  upsertView,
} from "./views";

let seq = 0;
function acct(over: Partial<Account> = {}): Account {
  seq += 1;
  return {
    id: `a${seq}`,
    email: `u${seq}@example.com`,
    source: "local",
    status: "active",
    tags: [],
    codeChannel: "auto",
    hasRefresh: true,
    hasAccess: false,
    hasPassword: false,
    hasEmailPassword: false,
    hasRecoveryEmail: false,
    hasApiKey: false,
    createdAt: "2026-09-01T00:00:00Z",
    updatedAt: "2026-09-01T00:00:00Z",
    seq,
    availability: "long_lived",
    ...over,
  };
}

function memStore() {
  const m = new Map<string, string>();
  return {
    getItem: (k: string) => m.get(k) ?? null,
    setItem: (k: string, v: string) => void m.set(k, v),
    removeItem: (k: string) => void m.delete(k),
    size: () => m.size,
  };
}

describe("availability filter", () => {
  it("filters on the answer Rust gave and passes everything through on 'all'", () => {
    // 可用性不在前端算：Rust 的 `Account::availability` 是唯一口径，这里只认它。
    const gone = acct({ availability: "logged_out", hasRefresh: false });
    const list = [acct(), gone, acct({ availability: "dead" })];
    expect(applyAvailFilter(list, "all")).toHaveLength(3);
    expect(applyAvailFilter(list, "logged_out").map((a) => a.id)).toEqual([gone.id]);
    expect(applyAvailFilter(list, "dead")).toHaveLength(1);
  });

  it("still knows which accounts can be refreshed and which can use the dashboard", () => {
    const now = Date.parse("2026-09-09T12:00:00Z");
    const live = acct({ hasRefresh: false, hasAccess: true, accessExpiresAt: "2026-09-09T20:00:00Z" });
    const expired = acct({ hasRefresh: false, hasAccess: true, accessExpiresAt: "2026-09-09T11:00:00Z" });
    // 到期前 60 秒以内也算过期：和 Rust 侧的余量一致，避免拿一把马上失效的 token 去撞接口。
    const nearlyExpired = acct({ hasRefresh: false, hasAccess: true, accessExpiresAt: "2026-09-09T12:00:30Z" });
    expect(hasLiveAccess(live, now)).toBe(true);
    expect(canQueryUsage(live, now)).toBe(true);
    expect(canQueryUsage(expired, now)).toBe(false);
    expect(hasLiveAccess(nearlyExpired, now)).toBe(false);
    expect(canQueryUsage(acct({ hasRefresh: false, hasAccess: true, accessExpiresAt: "2026-09-09T11:00:00Z", hasApiKey: true }), now)).toBe(true);
    expect(canUseDashboard(acct({ hasRefresh: false, hasAccess: true, accessExpiresAt: "2026-09-09T11:00:00Z", hasApiKey: true }), now)).toBe(false);
  });
});

describe("paid plan filter", () => {
  it("keeps every paid tier and drops free / unknown", () => {
    const list = [
      acct({ membership: "pro" }),
      acct({ membership: "ultra" }),
      acct({ membership: "free" }),
      acct({ membership: undefined }),
    ];
    expect(applyPlanFilter(list, "paid").map((a) => a.membership)).toEqual(["pro", "ultra"]);
  });
});

describe("view matching", () => {
  it("recognises builtin views and reports custom otherwise", () => {
    expect(matchView(DEFAULT_VIEW, [])?.id).toBe("all");
    const paid = BUILTIN_VIEWS.find((v) => v.id === "paid-ready")!;
    expect(matchView(paid.spec, [])?.id).toBe("paid-ready");
    expect(matchView({ ...DEFAULT_VIEW, plan: "pro" }, [])).toBeNull();
  });

  it("upsert replaces a view with the same name instead of duplicating it", () => {
    const a = upsertView([], "主力", { ...DEFAULT_VIEW, plan: "pro" });
    const b = upsertView(a, "主力", { ...DEFAULT_VIEW, plan: "ultra" });
    expect(b.map((v) => v.spec.plan)).toEqual(["ultra"]);
  });
});

describe("persistence", () => {
  it("round-trips the spec and clears storage when back at default", () => {
    const store = memStore();
    const spec = { ...DEFAULT_VIEW, plan: "paid" as const, sort: "reset" as const };
    saveSpec(spec, store);
    expect(loadSpec(store)).toEqual(spec);
    saveSpec(DEFAULT_VIEW, store);
    expect(store.size()).toBe(0);
    expect(isDefaultView(loadSpec(store))).toBe(true);
  });

  it("tolerates stale enums on disk by falling back per field", () => {
    const spec = normalizeSpec({ plan: "gone", sort: "checked", cred: 42, archived: "yes" });
    expect(spec.plan).toBe("all");
    expect(spec.sort).toBe("checked");
    expect(spec.avail).toBe("all");
    expect(spec.archived).toBe(false);
  });

  it("upgrades the previous shape: filter → quota, cred → avail", () => {
    // 上一版存的是 `{ filter（额度）, cred（凭证）}`。存过「付费可用」那类视图的人不该丢掉它。
    const spec = normalizeSpec({ filter: "warn", cred: "needsAuth", plan: "paid", sort: "added" });
    expect(spec.quota).toBe("warn");
    expect(spec.avail).toBe("logged_out");
    expect(spec.plan).toBe("paid");
    expect(normalizeSpec({ cred: "authorized" }).avail).toBe("long_lived");
    expect(normalizeSpec({ cred: "any" }).avail).toBe("all");
  });

  it("has an archived builtin view that no ordinary filter combination collides with", () => {
    const archived = BUILTIN_VIEWS.find((v) => v.id === "archived")!;
    expect(archived.spec.archived).toBe(true);
    expect(matchView({ ...DEFAULT_VIEW, archived: true }, [])?.id).toBe("archived");
    expect(matchView(DEFAULT_VIEW, [])?.id).toBe("all");
  });

  it("drops the retired sorts (status / headroom) back to the default", () => {
    expect(normalizeSpec({ sort: "headroom" }).sort).toBe("added");
    expect(normalizeSpec({ sort: "status" }).sort).toBe("added");
  });

  it("only persists user-made views, never the builtins", () => {
    const store = memStore();
    const own = upsertView([], "主力", { ...DEFAULT_VIEW, plan: "pro" });
    persistSavedViews([...BUILTIN_VIEWS, ...own], store);
    expect(loadSavedViews(store).map((v) => v.label)).toEqual(["主力"]);
    persistSavedViews([], store);
    expect(store.size()).toBe(0);
  });

  it("survives garbage in storage", () => {
    const store = memStore();
    store.setItem("nexus.accounts.view", "{not json");
    store.setItem("nexus.accounts.savedViews", "[1, 2]");
    expect(loadSpec(store)).toEqual(DEFAULT_VIEW);
    expect(loadSavedViews(store)).toEqual([]);
  });
});
