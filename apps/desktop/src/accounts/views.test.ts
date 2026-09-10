import { describe, expect, it } from "vitest";
import type { Account } from "../ipc/types";
import { applyCredFilter, applyPlanFilter, canQueryUsage, credState, hasLiveAccess } from "../ui/accounts";
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
    createdAt: "2026-09-01T00:00:00Z",
    updatedAt: "2026-09-01T00:00:00Z",
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

describe("cred filter", () => {
  it("separates 'fixable' from 'dead' and from 'fine'", () => {
    expect(credState(acct())).toBe("authorized");
    expect(credState(acct({ hasRefresh: false }))).toBe("needsAuth");
    expect(credState(acct({ status: "needs_login" }))).toBe("needsAuth");
    // 失效的号即使还带着 refresh token 也不算「掉授权」：它修不回来，别混进待办里。
    expect(credState(acct({ status: "dead" }))).toBe("dead");
  });

  it("treats a session-token-only account as 'session' while the token lives, 'needsAuth' after", () => {
    const now = Date.parse("2026-09-09T12:00:00Z");
    const live = acct({ hasRefresh: false, hasAccess: true, accessExpiresAt: "2026-09-09T20:00:00Z" });
    const expired = acct({ hasRefresh: false, hasAccess: true, accessExpiresAt: "2026-09-09T11:00:00Z" });
    // 到期前 60 秒以内也算过期：和 Rust 侧的余量一致，避免拿一把马上失效的 token 去撞接口。
    const nearlyExpired = acct({ hasRefresh: false, hasAccess: true, accessExpiresAt: "2026-09-09T12:00:30Z" });
    expect(hasLiveAccess(live, now)).toBe(true);
    expect(canQueryUsage(live, now)).toBe(true);
    expect(credState(live, now)).toBe("session");
    expect(credState(expired, now)).toBe("needsAuth");
    expect(canQueryUsage(expired, now)).toBe(false);
    expect(credState(nearlyExpired, now)).toBe("needsAuth");
    // 有 refresh 的号不看 access：哪怕 access 过期，refresh 一换就有新的。
    expect(credState(acct({ hasAccess: true, accessExpiresAt: "2026-09-09T11:00:00Z" }), now)).toBe("authorized");
  });

  it("filters by cred state and passes everything through on 'any'", () => {
    const stale = acct({ hasRefresh: false });
    const list = [acct(), stale, acct({ status: "dead" })];
    expect(applyCredFilter(list, "any")).toHaveLength(3);
    expect(applyCredFilter(list, "needsAuth").map((a) => a.id)).toEqual([stale.id]);
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
    const spec = normalizeSpec({ plan: "gone", sort: "checked", cred: 42 });
    expect(spec.plan).toBe("all");
    expect(spec.sort).toBe("checked");
    expect(spec.cred).toBe("any");
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
