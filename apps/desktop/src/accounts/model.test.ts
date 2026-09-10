import { describe, expect, it } from "vitest";
import type { Account } from "../ipc/types";
import { createCursorAccountView } from "./model";

function account(overrides: Partial<Account> = {}): Account {
  return {
    id: "account-1",
    email: "User@Example.com",
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
    ...overrides,
  };
}

describe("account presentation model", () => {
  it("uses managed identity and full Cursor usage when available", () => {
    const managed = account({
      usage: {
        fetchedAt: "2026-09-03T00:00:00Z",
        plan: "pro",
        totalPercentUsed: 42,
      },
      lastCheckedAt: "2026-09-03T00:00:00Z",
    });
    const view = createCursorAccountView({
      label: "different@example.com",
      managed,
      placement: { kind: "library", label: "账号库" },
    });

    expect(view.key).toBe("cursor:managed:account-1");
    expect(view.label).toBe("User@Example.com");
    expect(view.membership).toBe("pro");
    expect(view.usage).toEqual({
      kind: "cursor",
      value: managed.usage,
      checkedAt: managed.lastCheckedAt,
    });
  });

  it("keeps a runtime-only account visible with an honest summary", () => {
    const view = createCursorAccountView({
      label: " Runtime@Example.com ",
      fallbackPercentUsed: 73,
      placement: { kind: "gateway", label: "网关号池" },
    });

    expect(view.key).toBe("cursor:external:runtime@example.com");
    expect(view.managed).toBeNull();
    expect(view.usage).toEqual({
      kind: "summary",
      label: "总额度",
      percentUsed: 73,
    });
  });

  it("never turns missing usage into zero", () => {
    const view = createCursorAccountView({
      label: "history@example.com",
      placement: { kind: "switcher", label: "切号池" },
      unavailableReason: "只有登录态快照",
    });

    expect(view.usage).toEqual({
      kind: "unavailable",
      reason: "只有登录态快照",
    });
  });
});
