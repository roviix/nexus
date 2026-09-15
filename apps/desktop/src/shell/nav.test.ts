import { describe, expect, it } from "vitest";
import { DEFAULT_ROUTE, go, NAV_GROUPS, parseRoute, routeHash, SECTIONS, SETTINGS_TABS } from "./nav";

describe("route ⇄ hash", () => {
  it("round-trips every section", () => {
    for (const s of SECTIONS) {
      const r = go(s.id);
      expect(parseRoute(routeHash(r))).toEqual(r);
    }
  });

  it("carries channel and model for connect / models", () => {
    const r = go("connect", { channel: "chatgpt", model: "gpt-5.6-sol" });
    expect(routeHash(r)).toBe("#connect?channel=chatgpt&model=gpt-5.6-sol");
    expect(parseRoute(routeHash(r))).toEqual(r);
    expect(parseRoute("#models?channel=grok")).toEqual({ section: "models", channel: "grok" });
    expect(parseRoute("#connect?model=gpt-5.6-sol")).toEqual({ section: "connect", model: "gpt-5.6-sol" });
    // 上一版把号源写在子路径里：`#connect/local` 落回接入页。
    expect(parseRoute("#connect/local")).toEqual({ section: "connect" });
    expect(parseRoute("#models/local")).toEqual({ section: "models" });
  });

  it("puts the playground's view in the path and its model in the query", () => {
    for (const view of ["chat", "image", "video", "assets"] as const) {
      const r = go("playground", { view });
      expect(routeHash(r)).toBe(`#playground/${view}`);
      expect(parseRoute(routeHash(r))).toEqual(r);
    }
    // 模型广场「试一下」把模型带进来：子项 + 模型都要能往返。
    const r = go("playground", { view: "image", model: "gpt-image-1" });
    expect(routeHash(r)).toBe("#playground/image?model=gpt-image-1");
    expect(parseRoute(routeHash(r))).toEqual(r);
    expect(parseRoute("#playground")).toEqual({ section: "playground" });
    expect(parseRoute("#playground/nope?source=mars")).toEqual({ section: "playground" });
    expect(parseRoute("#playground?model=a/b c")).toEqual({ section: "playground", model: "a/b c" });
    // 别的页不认 view。
    expect(go("connect", { view: "assets" })).toEqual({ section: "connect" });
    // 模型 id 里的字符要能安全往返。
    const odd = go("connect", { model: "a/b c&d" });
    expect(parseRoute(routeHash(odd))).toEqual(odd);
  });

  it("carries an email for the switcher", () => {
    const r = go("switcher", { email: "a+b@example.com" });
    expect(routeHash(r)).toBe("#switcher?email=a%2Bb%40example.com");
    expect(parseRoute(routeHash(r))).toEqual(r);
  });

  it("puts the settings tab in the path so other pages can land on it", () => {
    for (const t of SETTINGS_TABS) {
      const r = go("settings", { tab: t.id });
      expect(routeHash(r)).toBe(`#settings/${t.id}`);
      expect(parseRoute(routeHash(r))).toEqual(r);
    }
    expect(parseRoute("#settings")).toEqual({ section: "settings" });
    expect(parseRoute("#settings/nope")).toEqual({ section: "settings" });
    // 别的页不认 tab。
    expect(go("gateway", { tab: "advanced" })).toEqual({ section: "gateway" });
  });

  it("puts the gateway's Cursor pool on its own sub-path", () => {
    const r = go("gateway", { sub: "pool" });
    expect(routeHash(r)).toBe("#gateway/pool");
    expect(parseRoute(routeHash(r))).toEqual(r);
    expect(parseRoute("#gateway")).toEqual({ section: "gateway" });
    expect(parseRoute("#gateway/nope")).toEqual({ section: "gateway" });
    // 旧地址 #use/gateway 落回网关主页，不带下钻。
    expect(parseRoute("#use/gateway")).toEqual({ section: "gateway" });
    // 别的页不认 sub。
    expect(go("accounts", { sub: "pool" })).toEqual({ section: "accounts" });
  });

  it("ignores params on pages that do not take them", () => {
    expect(
      go("accounts", {
        channel: "chatgpt",
        model: "x",
        email: "a@b.com",
      }),
    ).toEqual({
      section: "accounts",
    });
    expect(routeHash(go("accounts", { channel: "chatgpt" }))).toBe("#accounts");
    expect(parseRoute("#gateway/cloud?model=x")).toEqual({ section: "gateway" });
    expect(go("connect", { email: "a@b.com" })).toEqual({ section: "connect" });
    expect(parseRoute("#accounts?email=a@b.com")).toEqual({ section: "accounts" });
  });

  it("carries the account platform as a sub-path, defaulting to Cursor", () => {
    expect(go("accounts", { platform: "chatgpt" })).toEqual({ section: "accounts", platform: "chatgpt" });
    expect(routeHash(go("accounts", { platform: "chatgpt" }))).toBe("#accounts/chatgpt");
    expect(parseRoute("#accounts/chatgpt")).toEqual({ section: "accounts", platform: "chatgpt" });
    expect(routeHash(go("accounts", { platform: "grok" }))).toBe("#accounts/grok");
    expect(parseRoute("#accounts/grok")).toEqual({ section: "accounts", platform: "grok" });
    expect(routeHash(go("accounts", { platform: "kiro" }))).toBe("#accounts/kiro");
    expect(parseRoute("#accounts/kiro")).toEqual({ section: "accounts", platform: "kiro" });
    // 没写平台 = Cursor（页面自己取默认），地址保持最短。
    expect(routeHash(go("accounts"))).toBe("#accounts");
    expect(parseRoute("#accounts/nope")).toEqual({ section: "accounts" });
    // 别的页不认平台。
    expect(go("gateway", { platform: "chatgpt" })).toEqual({ section: "gateway" });
  });

  it("falls back to the default instead of failing on garbage", () => {
    expect(parseRoute("")).toEqual(DEFAULT_ROUTE);
    expect(parseRoute("#nope")).toEqual(DEFAULT_ROUTE);
    expect(parseRoute("#connect/whatever")).toEqual({ section: "connect" });
    expect(parseRoute("#/models")).toEqual({ section: "models" });
  });

  it("maps last version's hashes onto the new pages", () => {
    expect(parseRoute("#use/gateway")).toEqual({ section: "gateway" });
    expect(parseRoute("#use/switcher")).toEqual({ section: "switcher" });
    expect(parseRoute("#use/sand")).toEqual({ section: "panel", mode: "sand" });
    expect(parseRoute("#sand")).toEqual({ section: "panel", mode: "sand" });
    expect(parseRoute("#crsr")).toEqual({ section: "panel", mode: "crsr" });
    // 新地址自己也带档位，认不出的档位就回缺省（页面按盘上决定）。
    expect(parseRoute("#panel/crsr")).toEqual({ section: "panel", mode: "crsr" });
    expect(parseRoute("#panel/whatever")).toEqual({ section: "panel" });
    expect(routeHash(go("panel", { mode: "sand" }))).toBe("#panel/sand");
    expect(parseRoute("#use")).toEqual({ section: "switcher" });
  });
});

describe("sidebar groups", () => {
  it("place every section exactly once, with settings pinned outside the groups", () => {
    const placed = NAV_GROUPS.flatMap((g) => g.items);
    expect(new Set(placed).size).toBe(placed.length);
    const all = SECTIONS.map((s) => s.id).filter((id) => id !== "settings");
    expect([...placed].sort()).toEqual([...all].sort());
  });

  it("keep the Cursor panel page alone in the patch group — it is not a way of using the pool", () => {
    const patch = NAV_GROUPS.find((g) => g.items.includes("panel"))!;
    expect(patch.items).toEqual(["panel"]);
    // 中转 API 一组按「看 → 用 → 接 → 引擎」排：先浏览模型，再在游乐场上手，再配客户端，
    // 最后是本机那台引擎。
    const relay = NAV_GROUPS.find((g) => g.id === "relay")!;
    expect(relay.items).toEqual(["models", "playground", "connect", "gateway"]);
  });

  it("call the pool group 账号, not Cursor 账号 — ChatGPT accounts live there too", () => {
    const accounts = NAV_GROUPS.find((g) => g.items.includes("accounts"))!;
    expect(accounts.label).toBe("账号");
    expect(accounts.items).toEqual(["accounts", "switcher"]);
  });
});
