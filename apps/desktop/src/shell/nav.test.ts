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
    expect(go("channels", { tab: "advanced" })).toEqual({ section: "channels" });
  });

  it("maps legacy gateway routes onto overview", () => {
    expect(parseRoute("#gateway")).toEqual({ section: "overview" });
    expect(parseRoute("#use/gateway")).toEqual({ section: "overview" });
    expect(parseRoute("#gateway/pool")).toEqual({ section: "overview" });
    // 别的页不认 sub。
    expect(go("accounts", { sub: "pool" })).toEqual({ section: "accounts" });
  });

  it("handles channels pool and models sub-paths via query", () => {
    const rPool = go("channels", { channel: "chatgpt", sub: "pool" });
    expect(routeHash(rPool)).toBe("#channels?channel=chatgpt&sub=pool");
    expect(parseRoute(routeHash(rPool))).toEqual({
      section: "channels",
      channel: "chatgpt",
      sub: "pool",
    });

    const rModels = go("channels", { channel: "cursor", sub: "models" });
    expect(routeHash(rModels)).toBe("#channels?channel=cursor&sub=models");
    expect(parseRoute(routeHash(rModels))).toEqual({
      section: "channels",
      channel: "cursor",
      sub: "models",
    });
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
    expect(parseRoute("#gateway/cloud?model=x")).toEqual({ section: "overview" });
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
    expect(routeHash(go("accounts", { platform: "zcode" }))).toBe("#accounts/zcode");
    expect(parseRoute("#accounts/zcode")).toEqual({ section: "accounts", platform: "zcode" });
    expect(routeHash(go("accounts", { platform: "provider" }))).toBe("#accounts/provider");
    expect(parseRoute("#accounts/provider")).toEqual({ section: "accounts", platform: "provider" });
    expect(routeHash(go("accounts", { platform: "qoder" }))).toBe("#accounts/qoder");
    expect(parseRoute("#accounts/qoder")).toEqual({ section: "accounts", platform: "qoder" });
    expect(routeHash(go("accounts", { platform: "claude" }))).toBe("#accounts/claude");
    expect(parseRoute("#accounts/claude")).toEqual({ section: "accounts", platform: "claude" });
    expect(routeHash(go("accounts", { platform: "antigravity" }))).toBe("#accounts/antigravity");
    expect(parseRoute("#accounts/antigravity")).toEqual({ section: "accounts", platform: "antigravity" });
    // 接入页带客户端；上一版的 `?provider=` 落到供应商通道。
    expect(routeHash(go("connect", { client: "codex", channel: "chatgpt" }))).toBe("#connect?client=codex&channel=chatgpt");
    expect(parseRoute("#connect?client=codex&channel=chatgpt")).toEqual({ section: "connect", client: "codex", channel: "chatgpt" });
    expect(parseRoute("#connect?client=nope")).toEqual({ section: "connect" });
    expect(parseRoute("#connect?provider=abc")).toEqual({ section: "connect", channel: "provider" });
    expect(go("models", { client: "claude" })).toEqual({ section: "models" });
    // 没写平台 = Cursor（页面自己取默认），地址保持最短。
    expect(routeHash(go("accounts"))).toBe("#accounts");
    expect(parseRoute("#accounts/nope")).toEqual({ section: "accounts" });
    // 别的页不认平台。
    expect(go("overview", { platform: "chatgpt" })).toEqual({ section: "overview" });
  });

  it("falls back to the default instead of failing on garbage", () => {
    expect(parseRoute("")).toEqual(DEFAULT_ROUTE);
    expect(parseRoute("#nope")).toEqual(DEFAULT_ROUTE);
    expect(parseRoute("#connect/whatever")).toEqual({ section: "connect" });
    expect(parseRoute("#/models")).toEqual({ section: "models" });
  });

  it("maps last version's hashes onto the new pages", () => {
    expect(parseRoute("#use/gateway")).toEqual({ section: "overview" });
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
  it("organizes into three clear groups for Nexus 2.0 IA", () => {
    const placed = NAV_GROUPS.flatMap((g) => g.items);
    expect(new Set(placed).size).toBe(placed.length);
    expect(placed).toEqual([
      "overview",
      "connect",
      "channels",
      "models",
      "playground",
      "accounts",
    ]);
  });

  it("organizes relay group", () => {
    const relay = NAV_GROUPS.find((g) => g.id === "relay")!;
    expect(relay.label).toBe("路由与网关");
    expect(relay.items).toEqual(["connect", "channels", "models", "playground"]);
  });

  it("organizes home group for overview", () => {
    const home = NAV_GROUPS.find((g) => g.id === "home")!;
    expect(home.label).toBeNull();
    expect(home.items).toEqual(["overview"]);
  });

  it("organizes assets group for accounts", () => {
    const assets = NAV_GROUPS.find((g) => g.id === "assets")!;
    expect(assets.label).toBe("资产库");
    expect(assets.items).toEqual(["accounts"]);
  });
});

