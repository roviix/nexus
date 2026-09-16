/**
 * 假的 `@tauri-apps/api/core`：按命令名回 fixtures。
 *
 * 数据只求形状对、数量够看版式：几种状态的号、开着的网关、装了一半的 Sand、
 * 一份本地网关的模型目录。想看空态，在地址上加 `&empty=1`。
 */
import { TRY_EVENT, type LocalModel, type TryEvent, type TryFrame } from "../src/ipc/models";
import { emitMock } from "./mock-event";
import { fakeImageSrc, handlePlayground } from "./mock-playground";
import type {
  Account,
  AccountBilling,
  AccountUsage,
  ActivityEntry,
  AppStatus,
  AuthBackup,
  ChatGptAccount,
  ChatGptBilling,
  ChatGptUsage,
  CursorRelease,
  GatewayCandidate,
  GatewayLane,
  GatewayStatus,
  LocalBackup,
  Overview,
  RemoteOverview,
  SandStatus,
  CrsrStatus,
  SwitchProfile,
  SwitchProgress,
  UsageDay,
  UsageHour,
  UsageSummary,
} from "../src/ipc/types";

const q = new URLSearchParams(location.search);
const EMPTY = q.get("empty") === "1";
/** `&nocursor=1`：本机找不到 Cursor —— 看设置页顶上的横幅和「只读」降级长什么样。 */
const NO_CURSOR = q.get("nocursor") === "1";

const NOW = Date.now();
const H = 3_600_000;
const D = 24 * H;
const iso = (msAgo: number) => new Date(NOW - msAgo).toISOString();

// ── 账号 ────────────────────────────────────────────────────────────────────

/** 本地零点：mock 里「今天」的窗口从这儿起，和真实链路（前端递给 Rust 的那一刻）同一个口径。 */
const DAY_START = (() => {
  const d = new Date(NOW);
  d.setHours(0, 0, 0, 0);
  return d.getTime();
})();

const usage = (over: Partial<AccountUsage>): AccountUsage => ({
  fetchedAt: iso(20 * 60_000),
  plan: "pro",
  subscriptionStatus: "active",
  cycleStart: NOW - 12 * D,
  cycleEnd: NOW + 18 * D,
  totalPercentUsed: 42,
  autoPercentUsed: 12,
  apiPercentUsed: 61,
  // breakdown 三项都是消费分量；额度上限是 plan.limit。
  includedCents: 840,
  bonusCents: 0,
  spendCents: 840,
  planLimitCents: 2000,
  inputTokens: 4_180_000,
  outputTokens: 612_000,
  cacheReadTokens: 2_900_000,
  cacheWriteTokens: 380_000,
  byModel: [
    { model: "claude-sonnet-5", tier: 1, cents: 512, input: 2_100_000, output: 380_000, cacheRead: 1_800_000, cacheWrite: 240_000 },
    { model: "gpt-5.6-sol-max-fast", tier: 1, cents: 188, input: 900_000, output: 120_000, cacheRead: 600_000, cacheWrite: 90_000 },
    { model: "composer-2.5-fast", tier: 2, cents: 96, input: 980_000, output: 92_000, cacheRead: 420_000, cacheWrite: 40_000 },
    { model: "claude-opus-5", tier: 1, cents: 44, input: 200_000, output: 20_000, cacheRead: 80_000, cacheWrite: 10_000 },
  ],
  // 刷用量时多问的两个时间窗：今天、近 7 天。
  today: {
    start: DAY_START,
    end: NOW,
    cents: 118,
    inputTokens: 520_000,
    outputTokens: 71_000,
    cacheReadTokens: 310_000,
    cacheWriteTokens: 42_000,
    byModel: [
      { model: "claude-sonnet-5", tier: 1, cents: 96, input: 400_000, output: 60_000, cacheRead: 280_000, cacheWrite: 36_000 },
      { model: "composer-2.5-fast", tier: 2, cents: 22, input: 120_000, output: 11_000, cacheRead: 30_000, cacheWrite: 6_000 },
    ],
  },
  week: {
    start: DAY_START - 6 * D,
    end: NOW,
    cents: 466,
    inputTokens: 2_300_000,
    outputTokens: 340_000,
    cacheReadTokens: 1_500_000,
    cacheWriteTokens: 210_000,
    byModel: [
      { model: "claude-sonnet-5", tier: 1, cents: 288, input: 1_200_000, output: 210_000, cacheRead: 1_000_000, cacheWrite: 140_000 },
      { model: "gpt-5.6-sol-max-fast", tier: 1, cents: 110, input: 600_000, output: 80_000, cacheRead: 400_000, cacheWrite: 50_000 },
      { model: "composer-2.5-fast", tier: 2, cents: 68, input: 500_000, output: 50_000, cacheRead: 100_000, cacheWrite: 20_000 },
    ],
  },
  bot: { percentUsed: 30, resetAt: NOW + 3 * D, hasAvailable: true, access: "granted", planLabel: "Pro" },
  ...over,
});

const billing = (over: Partial<AccountBilling>): AccountBilling => ({
  fetchedAt: iso(40 * 60_000),
  currency: "usd",
  collectionMethod: "charge_automatically",
  subscriptionStatus: "active",
  interval: "month",
  currentPeriodStart: NOW - 12 * D,
  currentPeriodEnd: NOW + 18 * D,
  cancelAtPeriodEnd: false,
  items: [{ name: "Cursor Pro", interval: "month", unitAmount: 2000, quantity: 1, currency: "usd" }],
  listPrice: 2000,
  currentAmount: 2000,
  discountState: "none",
  invoices: [
    {
      number: "INV-1",
      created: NOW - 12 * D,
      status: "paid",
      description: "Cursor Pro",
      subtotal: 2000,
      total: 1000,
      amountDue: 0,
      amountPaid: 1000,
      currency: "usd",
      discounts: [{ name: "Referred by a friend", amountOff: 1000, duration: "once", currency: "usd" }],
      lines: [{ description: "1 × Cursor Pro (at $20.00 / month)", amount: 2000, quantity: 1 }],
    },
  ],
  ...over,
});

const account = (over: Partial<Account> & { email: string }): Account => ({
  id: over.email,
  source: "local",
  status: "active",
  tags: [],
  codeChannel: "auto",
  hasRefresh: true,
  hasAccess: false,
  hasPassword: true,
  hasEmailPassword: false,
  hasRecoveryEmail: false,
  hasApiKey: false,
  createdAt: iso(9 * D),
  updatedAt: iso(H),
  lastCheckedAt: iso(20 * 60_000),
  seq: 0,
  availability: "long_lived",
  ...over,
});

export const ACCOUNTS: Account[] = EMPTY
  ? []
  : [
      // 按需计费三种形态各来一个（不封顶 / 有上限 / 没开），末行才看得出真实光景。
      account({ email: "arvid.pfeffer@outlook.com", createdAt: iso(15 * 60_000), note: "主力", tags: ["主力"], hasApiKey: true, usage: usage({ onDemandEnabled: true, onDemandUsedCents: 2115, creditGrantTotalCents: 10000, creditGrantUsedCents: 0, creditGrantRemainingCents: 10000 }), billing: billing({}) }),
      account({ email: "mara.quill@outlook.com", createdAt: iso(3 * H), source: "purchased", usage: usage({ totalPercentUsed: 91, apiPercentUsed: 97, autoPercentUsed: 80, onDemandEnabled: true, onDemandUsedCents: 315, onDemandLimitCents: 5000, creditGrantTotalCents: 2500, creditGrantUsedCents: 400, creditGrantRemainingCents: 2100 }) }),
      account({
        email: "tobias.rennick@outlook.com",
        createdAt: iso(1 * D),
        source: "purchased",
        usage: usage({ plan: "ultra", totalPercentUsed: 8, apiPercentUsed: 3, autoPercentUsed: 5 }),
        billing: billing({
          items: [{ name: "Cursor Ultra", interval: "month", unitAmount: 20000, quantity: 1, currency: "usd" }],
          listPrice: 20000,
          currentAmount: 0,
          discountState: "active",
          discount: {
            state: "active",
            name: "SuperGrok Heavy",
            amountOff: 20000,
            currency: "usd",
            duration: "repeating",
            durationInMonths: 6,
            startsAt: NOW - 20 * D,
            endsAt: NOW + 160 * D,
          },
          invoices: [
            {
              number: "INV-U1",
              created: NOW - 20 * D,
              status: "paid",
              description: "Cursor Ultra",
              subtotal: 20000,
              total: 0,
              amountDue: 0,
              amountPaid: 0,
              currency: "usd",
              discounts: [{ name: "SuperGrok Heavy", amountOff: 20000, duration: "repeating", durationInMonths: 6, currency: "usd" }],
              lines: [{ description: "1 × Cursor Ultra (at $200.00 / month)", amount: 20000, quantity: 1 }],
            },
          ],
        }),
      }),
      account({ email: "junko.hale@outlook.com", createdAt: iso(2 * D), source: "local", status: "needs_login", hasRefresh: false, availability: "logged_out", usage: null }),
      // 仅会话的号：此刻能用，到期就掉；卡上不算问题，分布条里单独一段。
      account({ email: "sunniva.brekke@outlook.com", createdAt: iso(4 * D), hasRefresh: false, hasAccess: true, accessExpiresAt: iso(-2 * D), availability: "session", usage: usage({ totalPercentUsed: 34, apiPercentUsed: 40, autoPercentUsed: 30 }) }),
      // 归档的号：默认不出现，「已归档」视图里能取回。
      account({ email: "old.batch.01@outlook.com", createdAt: iso(15 * D), source: "purchased", status: "dead", availability: "dead", archivedAt: iso(3 * D), usage: usage({ totalPercentUsed: 100, apiPercentUsed: 100, autoPercentUsed: 100 }) }),
      account({ email: "old.batch.02@outlook.com", createdAt: iso(18 * D), source: "purchased", archivedAt: iso(3 * D), usage: usage({ totalPercentUsed: 12 }) }),
      // 这个号当天就回血：末行的倒计时会换成钟点。
      account({ email: "pilar.osei@outlook.com", createdAt: iso(7 * D), source: "local", usage: usage({ totalPercentUsed: 100, apiPercentUsed: 100, autoPercentUsed: 100, bot: { percentUsed: 100, resetAt: NOW + 5 * H, hasAvailable: false, access: "granted" } }) }),
      account({ email: "wen.abernathy@outlook.com", createdAt: iso(10 * D), usage: undefined, lastCheckedAt: null }),
    ];

const chatgptUsage = (over: Partial<ChatGptUsage> = {}): ChatGptUsage => ({
  primary: { usedPercent: 28, resetAtMs: NOW + 4 * H + 50 * 60_000, windowMinutes: 300 },
  secondary: { usedPercent: 55, resetAtMs: NOW + 6 * D + 21 * H, windowMinutes: 10080 },
  planType: "pro",
  checkedAt: iso(21 * 60_000),
  source: "wham/usage",
  allowed: true,
  limitReached: false,
  userId: "user-preview",
  additional: [
    {
      name: "GPT-5.3-Codex-Spark",
      feature: "codex_bengalfox",
      allowed: true,
      limitReached: false,
      primary: { usedPercent: 100, resetAtMs: NOW + 4 * H + 50 * 60_000, windowMinutes: 300 },
      secondary: { usedPercent: 96, resetAtMs: NOW + 6 * D + 17 * H, windowMinutes: 10080 },
    },
  ],
  credits: { hasCredits: false, unlimited: false, overageLimitReached: false, balance: "0", resetAvailable: 0 },
  ...over,
});

const chatgptAccount = (over: Partial<ChatGptAccount> & { email: string }): ChatGptAccount => ({
  id: over.email,
  accountRef: `acct_${over.email.replace(/[^a-z0-9]/g, "").slice(0, 16)}`,
  planType: "pro",
  userId: "user-preview",
  organizationId: "org-personal",
  organizationTitle: "Personal",
  status: "active",
  enabled: true,
  note: null,
  usage: chatgptUsage(),
  billing: {
    planType: "pro",
    subscriptionPlan: "chatgptproplan",
    hasActiveSubscription: true,
    expiresAt: new Date(NOW + 12 * D).toISOString(),
    willRenew: true,
    billingPeriod: "monthly",
    checkedAt: iso(21 * 60_000),
    source: "accounts/check",
  } satisfies ChatGptBilling,
  lastCheckedAt: iso(21 * 60_000),
  lastError: null,
  hasRefresh: true,
  accessExpiresAt: new Date(NOW + 8 * D).toISOString(),
  createdAt: iso(23 * D),
  updatedAt: iso(21 * 60_000),
  traffic: { requests: 1600, tokens: 12_000_000, errors: 4, days: 90 },
  ...over,
});

export const CHATGPT_ACCOUNTS: ChatGptAccount[] = EMPTY
  ? []
  : [
      chatgptAccount({
        email: "arvid.pfeffer@outlook.com",
        userId: "fb87c718-66c5-4169-a66f-06fe7c3c2471",
        traffic: { requests: 1600, tokens: 12_000_000, errors: 2, days: 90 },
      }),
      chatgptAccount({
        email: "mara.quill@outlook.com",
        userId: "31570e240-4eedd-4e566-b8ee-e7314ffc",
        planType: "plus",
        billing: {
          planType: "plus",
          subscriptionPlan: "chatgptplusplan",
          hasActiveSubscription: true,
          expiresAt: new Date(NOW + 6 * D).toISOString(),
          willRenew: null,
          billingPeriod: "monthly",
          checkedAt: iso(21 * 60_000),
          source: "accounts/check",
        },
        usage: chatgptUsage({
          primary: { usedPercent: 63, resetAtMs: NOW + 6 * D + 15 * H, windowMinutes: 300 },
          secondary: { usedPercent: 96, resetAtMs: NOW + 6 * D + 17 * H, windowMinutes: 10080 },
          additional: [
            {
              name: "GPT-5.3-Codex-Spark",
              feature: "codex_bengalfox",
              allowed: true,
              limitReached: false,
              primary: { usedPercent: 100, resetAtMs: NOW + 4 * H + 50 * 60_000, windowMinutes: 300 },
              secondary: { usedPercent: 96, resetAtMs: NOW + 6 * D + 17 * H, windowMinutes: 10080 },
            },
          ],
        }),
        traffic: { requests: 1300, tokens: 11_800_000, errors: 1, days: 90 },
      }),
    ];

// ── 切号 ────────────────────────────────────────────────────────────────────

const OVERVIEW: Overview = {
  current: EMPTY
    ? null
    : {
        email: "arvid.pfeffer@outlook.com",
        membership: "pro",
        signupType: "Auth_0",
        subscriptionStatus: "active",
        hasAccessToken: true,
        hasRefreshToken: true,
        keyCount: 6,
      },
  machineIdShort: "3f9a…c1e2",
  machineIdOwner: EMPTY ? null : "arvid.pfeffer@outlook.com",
  hasOriginalMachine: true,
  cursorRunning: true,
  // 两把必需的键都要在，否则 `isWritable` 判成键名漂移，整页降级只读 —— 预览里看到的
  // 就永远是那条琥珀横幅加一列灰按钮。
  check: NO_CURSOR
    ? { dbPresent: false, tablePresent: false, presentKeys: [], missingKeys: [], cursorVersion: null }
    : {
        dbPresent: true,
        tablePresent: true,
        presentKeys: ["cursorAuth/accessToken", "cursorAuth/refreshToken", "cursorAuth/cachedEmail"],
        missingKeys: [],
        cursorVersion: "3.19.13",
      },
};

const profile = (email: string, i: number, over: Partial<SwitchProfile> = {}): SwitchProfile => ({
  id: `p${i}`,
  email,
  membership: null,
  note: null,
  machineIds: {
    "telemetry.machineId": "3f9a".padEnd(64, "0"),
    "telemetry.macMachineId": "aa".padEnd(64, "1"),
    "telemetry.devDeviceId": "0f6a8a6e-0000-4000-8000-000000000000",
    "telemetry.sqmId": "{00000000-0000-0000-0000-000000000000}",
  },
  createdAt: iso((i + 3) * D),
  updatedAt: iso(H),
  lastSwitchedAt: null,
  hasAuth: true,
  refreshIsPlaceholder: false,
  isCurrent: false,
  ...over,
});

// 切号池是账号总库的显式子集。
const PROFILES: SwitchProfile[] = ACCOUNTS.slice(0, 3).map((a, i) =>
  profile(a.email, i, {
    membership: a.usage?.plan ?? null,
    note: a.note ?? null,
    lastSwitchedAt: i === 0 ? iso(2 * H) : null,
    isCurrent: i === 0,
  }),
);
if (!EMPTY) {
  // 历史切号档可能还在，但总账号库已经删了它。用来核对“未托管账号”抽屉的降级形态。
  PROFILES.push(
    profile("history.only@example.com", PROFILES.length, {
      membership: "pro",
      lastSwitchedAt: iso(12 * D),
    }),
  );
}
let profileSequence = PROFILES.length;

function storeProfile(email: string): SwitchProfile {
  const existing = PROFILES.find((entry) =>
    entry.email.localeCompare(email, undefined, { sensitivity: "accent" }) === 0
  );
  if (existing) {
    existing.hasAuth = true;
    existing.updatedAt = new Date().toISOString();
    return existing;
  }

  const sourceAccount = ACCOUNTS.find((entry) => entry.email === email);
  const stored = profile(email, profileSequence, {
    membership: sourceAccount?.usage?.plan ?? sourceAccount?.membership ?? null,
    note: sourceAccount?.note ?? null,
  });
  profileSequence += 1;
  PROFILES.push(stored);
  return stored;
}

const BACKUPS: AuthBackup[] = [
  { id: "b1", email: "arvid.pfeffer@outlook.com", createdAt: iso(2 * H), reason: "pre-switch" },
  { id: "b2", email: "mara.quill@outlook.com", createdAt: iso(3 * D), reason: "manual" },
];

// ── 本地备份（~/.roviix/backups）────────────────────────────────────────────

const LOCAL_BACKUP_DIR = "/Users/me/.roviix/backups";

function localBackup(msAgo: number, reason: LocalBackup["reason"], sizeBytes: number): LocalBackup {
  const stamp = iso(msAgo).replace(/[-:]/g, "").replace(/\.\d{3}Z$/, "Z");
  const fileName = reason === "manual" ? `nexus-${stamp}.db` : `nexus-${stamp}-${reason}.db`;
  return { fileName, path: `${LOCAL_BACKUP_DIR}/${fileName}`, reason, createdAt: iso(msAgo), sizeBytes };
}

const LOCAL_BACKUPS: LocalBackup[] = EMPTY
  ? []
  : [
      localBackup(3 * H, "pre-restore", 418 * 1024),
      localBackup(D, "manual", 412 * 1024),
      localBackup(9 * D, "manual", 377 * 1024),
    ];

// ── 网关 ────────────────────────────────────────────────────────────────────

/**
 * 网关能看到的号（有来源能给凭证的）。名单（`ROSTER`）决定哪些真进队；
 * 剩下的在快照里是 `available`。预览里加 / 移都会改这份名单，刷新页面就复原。
 */
const GATEWAY_SEEN: Array<Omit<GatewayCandidate, "state">> = EMPTY
  ? []
  : [
      { label: "arvid.pfeffer@outlook.com", source: "cursor_login", pinned: true, storedId: null, percentUsed: 42 },
      { label: "mara.quill@outlook.com", source: "stored", pinned: false, storedId: "mara.quill@outlook.com", percentUsed: 91 },
      { label: "tobias.rennick@outlook.com", source: "stored", pinned: false, storedId: "tobias.rennick@outlook.com", percentUsed: 8 },
      { label: "pilar.osei@outlook.com", source: "stored", pinned: false, storedId: "pilar.osei@outlook.com", percentUsed: 100 },
      { label: "wen.abernathy@outlook.com", source: "stored", pinned: false, storedId: "wen.abernathy@outlook.com", percentUsed: null },
    ];

const STATE_OF: Record<string, GatewayCandidate["state"]> = {
  "arvid.pfeffer@outlook.com": { kind: "current" },
  "mara.quill@outlook.com": { kind: "quota_line" },
  "pilar.osei@outlook.com": { kind: "exhausted", reason: "额度用尽", retryInSecs: 1500 },
};

// 三个进队、一个名单里有但网关看不到（junko 待登录），其余两个可添加。
const ROSTER = new Set<string>(
  EMPTY ? [] : ["arvid.pfeffer@outlook.com", "mara.quill@outlook.com", "pilar.osei@outlook.com", "junko.hale@outlook.com"],
);

function laneSnapshot(): GatewayLane {
  const candidates = GATEWAY_SEEN.filter((c) => ROSTER.has(c.label)).map((c) => ({ ...c, state: STATE_OF[c.label] ?? { kind: "ready" as const } }));
  const seen = new Set(GATEWAY_SEEN.map((c) => c.label));
  return {
    current: candidates.find((c) => c.state.kind === "current")?.label ?? null,
    candidates,
    missing: [...ROSTER].filter((e) => !seen.has(e)),
    available: GATEWAY_SEEN.filter((c) => !ROSTER.has(c.label)).map((c) => ({ label: c.label, source: c.source, pinned: c.pinned, percentUsed: c.percentUsed })),
  };
}

const GATEWAY: GatewayStatus = {
  running: EMPTY
    ? null
    : {
        addr: "127.0.0.1:8787",
        baseUrl: "http://127.0.0.1:8787",
        startedAt: iso(40 * 60_000),
      },
  settings: { port: 8787, autostart: false, forceModel: null, defaultChannel: "cursor" },
  restartNeeded: false,
  apiKeySet: true,
  lane: laneSnapshot(),
  channels: [
    {
      id: "chatgpt",
      label: "ChatGPT",
      vendor: "openai",
      ready: !EMPTY && CHATGPT_ACCOUNTS.length > 0,
      mediaReady: false,
      lane: EMPTY
        ? { current: null, candidates: [], missing: [], available: [] }
        : {
            current: CHATGPT_ACCOUNTS[0]?.email ?? null,
            candidates: CHATGPT_ACCOUNTS.filter((a) => a.enabled).map((a, i) => ({
              label: a.email ?? a.accountRef,
              source: "chatgpt",
              pinned: false,
              storedId: a.id,
              percentUsed: a.usage?.primary?.usedPercent ?? null,
              state: i === 0 ? { kind: "current" as const } : { kind: "ready" as const },
            })),
            missing: [],
            available: [],
          },
      chatModels: ["chatgpt/gpt-5.4", "chatgpt/gpt-5.6-sol", "chatgpt/gpt-5.5"],
      imageModels: ["chatgpt/gpt-image-2"],
      videoModels: [],
      prefixes: ["chatgpt/", "codex/"],
    },
    {
      id: "grok",
      label: "Grok Build",
      vendor: "xai",
      ready: !EMPTY,
      mediaReady: !EMPTY,
      lane: EMPTY
        ? { current: null, candidates: [], missing: [], available: [] }
        : {
            current: "you@x.ai",
            candidates: [{ label: "you@x.ai", source: "grok", pinned: false, storedId: "g1", percentUsed: 37, state: { kind: "current" } }],
            missing: [],
            available: [],
          },
      chatModels: ["grok/grok-4.6", "grok/grok-4.5"],
      imageModels: ["grok/grok-imagine-image", "grok/grok-imagine-image-quality"],
      videoModels: ["grok/grok-imagine-video-1.5"],
      prefixes: ["grok/", "xai/"],
    },
    {
      id: "kiro",
      label: "Kiro",
      vendor: "aws",
      ready: false,
      mediaReady: false,
      lane: { current: null, candidates: [], missing: [], available: [] },
      chatModels: ["kiro/kiro-claude-sonnet-4.5"],
      imageModels: [],
      videoModels: [],
      prefixes: ["kiro/"],
    },
  ],
  mediaJobs: [],
};

// ── 本地用量（网关请求账本）────────────────────────────────────────────────

/** 最近 N 天，工作日多、周末少，今天只走了半天。 */
function usageSummary(days: number): UsageSummary {
  const out: UsageDay[] = [];
  const today = new Date(NOW);
  today.setHours(0, 0, 0, 0);
  for (let i = days - 1; i >= 0; i--) {
    const d = new Date(today.getTime() - i * D);
    const weekend = d.getDay() === 0 || d.getDay() === 6;
    const wave = 0.65 + 0.35 * Math.sin(i * 0.9);
    let calls = EMPTY ? 0 : Math.round((weekend ? 22 : 96) * wave);
    if (i === 0) calls = Math.round(calls * 0.45);
    const errors = Math.round(calls * (i % 5 === 0 ? 0.06 : 0.012));
    out.push({
      day: `${d.getFullYear()}-${String(d.getMonth() + 1).padStart(2, "0")}-${String(d.getDate()).padStart(2, "0")}`,
      calls,
      errors,
      inputTokens: calls * 3400,
      outputTokens: calls * 720,
    });
  }
  const sum = (rows: UsageDay[]) => rows.reduce((a, r) => ({ calls: a.calls + r.calls, errors: a.errors + r.errors, inputTokens: a.inputTokens + r.inputTokens, outputTokens: a.outputTokens + r.outputTokens }), { calls: 0, errors: 0, inputTokens: 0, outputTokens: 0 });
  const w = sum(out);
  const t = sum(out.slice(-1));
  // 今天的小时：上午一波、下午一波，当前小时之后全零。分配要和上面「今天」那一天的合计对得上。
  const nowHour = new Date(NOW).getHours();
  const shape = Array.from({ length: 24 }, (_, h) => (h > nowHour || h < 8 || EMPTY ? 0 : h < 12 ? 1 + (h - 8) * 0.5 : h < 14 ? 0.6 : 1.4 + Math.sin((h - 14) * 0.8)));
  const weight = shape.reduce((a, b) => a + b, 0) || 1;
  const hours: UsageHour[] = shape.map((s, h) => {
    const calls = Math.round((t.calls * s) / weight);
    return { hour: h, calls, errors: h === 10 ? Math.min(calls, Math.round(t.errors * 0.7)) : h === 15 ? Math.min(calls, t.errors - Math.round(t.errors * 0.7)) : 0, inputTokens: calls * 3400, outputTokens: calls * 720 };
  });
  const totals = (x: typeof w) => ({ ...x, cacheReadTokens: Math.round(x.inputTokens * 0.38), ttftP50Ms: x.calls ? 940 : null, durationP50Ms: x.calls ? 6800 : null });
  const share = (p: number) => Math.round(w.calls * p);
  return {
    days: out,
    hours,
    today: totals(t),
    window: totals(w),
    byModel: EMPTY
      ? []
      : [
          { name: "claude-sonnet-5", calls: share(0.52), errors: Math.round(w.errors * 0.4), tokens: Math.round((w.inputTokens + w.outputTokens) * 0.5) },
          { name: "gpt-5.6-sol-max-fast", calls: share(0.24), errors: Math.round(w.errors * 0.3), tokens: Math.round((w.inputTokens + w.outputTokens) * 0.27) },
          { name: "claude-opus-5", calls: share(0.13), errors: Math.round(w.errors * 0.25), tokens: Math.round((w.inputTokens + w.outputTokens) * 0.18) },
          { name: "auto", calls: share(0.08), errors: 0, tokens: Math.round((w.inputTokens + w.outputTokens) * 0.04) },
          { name: "gemini-3.7-flash", calls: share(0.03), errors: 0, tokens: Math.round((w.inputTokens + w.outputTokens) * 0.01) },
        ],
    byAccount: EMPTY
      ? []
      : [
          { name: "arvid.pfeffer@outlook.com", calls: share(0.61), errors: Math.round(w.errors * 0.5), tokens: Math.round((w.inputTokens + w.outputTokens) * 0.6) },
          { name: "mara.quill@outlook.com", calls: share(0.27), errors: Math.round(w.errors * 0.45), tokens: Math.round((w.inputTokens + w.outputTokens) * 0.3) },
          { name: "pilar.osei@outlook.com", calls: share(0.12), errors: 0, tokens: Math.round((w.inputTokens + w.outputTokens) * 0.1) },
        ],
    byChannel: EMPTY ? [] : [{ name: "cursor", calls: share(0.7), errors: w.errors, tokens: Math.round((w.inputTokens + w.outputTokens) * 0.7) }, { name: "grok", calls: share(0.3), errors: 0, tokens: Math.round((w.inputTokens + w.outputTokens) * 0.3) }],
    recent: EMPTY
      ? []
      : [
          { at: iso(3 * 60_000), account: "arvid.pfeffer@outlook.com", model: "claude-sonnet-5", routed: "claude-sonnet-5", ok: true, status: 200, kind: null, inputTokens: 4120, outputTokens: 860, ttftMs: 880, durationMs: 7400, channel: "cursor" },
          { at: iso(9 * 60_000), account: "arvid.pfeffer@outlook.com", model: "gpt-5.6-sol-max-fast", routed: "gpt-5.6-sol-max-fast", ok: true, status: 200, kind: null, inputTokens: 2210, outputTokens: 310, ttftMs: 620, durationMs: 2900, channel: "cursor" },
          { at: iso(31 * 60_000), account: "mara.quill@outlook.com", model: "claude-opus-5", routed: null, ok: false, status: 402, kind: "quota", inputTokens: 0, outputTokens: 0, ttftMs: null, durationMs: 340, channel: "cursor" },
          { at: iso(44 * 60_000), account: "arvid.pfeffer@outlook.com", model: "auto", routed: "composer-2.5-fast", ok: true, status: 200, kind: null, inputTokens: 980, outputTokens: 220, ttftMs: 410, durationMs: 1800, channel: "cursor" },
        ],
    since: EMPTY ? null : iso(41 * D),
  };
}

// ── 一键接入 / 权限预检 ──────────────────────────────────────────────────────

/** 预览里的「客户端配置」状态：Claude Code 已接到本地网关，Codex 还没接。接入 / 撤销会改它。 */
const CLIENTS: Record<string, { path: string; exists: boolean; baseUrl: string | null; model: string | null; revertible: boolean; appliedAt: string | null; pointsTo: "local" | "other" | "none" }> = {
  claude: EMPTY
    ? { path: "/Users/me/.claude/settings.json", exists: false, baseUrl: null, model: null, revertible: false, appliedAt: null, pointsTo: "none" }
    : { path: "/Users/me/.claude/settings.json", exists: true, baseUrl: "http://127.0.0.1:8787", model: "claude-sonnet-5", revertible: true, appliedAt: iso(2 * H), pointsTo: "local" },
  codex: { path: "/Users/me/.codex/config.toml", exists: !EMPTY, baseUrl: EMPTY ? null : "https://api.openai.com/v1", model: EMPTY ? null : "gpt-5-codex", revertible: false, appliedAt: null, pointsTo: EMPTY ? "none" : "other" },
};

/** 权限：macOS 上两项系统弹窗类还没申请过；两项探针类已通过。`&denied=1` 看被拒的样子。 */
const DENIED = q.get("denied") === "1";
let PREFLIGHT_DONE = q.get("preflight") !== "0";
const PERMS = {
  platform: "macos" as const,
  items: [
    { id: "cursor_data", title: "写入 Cursor 登录态", usedBy: "切号", status: "ok" as const, detail: "/Users/me/Library/Application Support/Cursor/User/globalStorage/state.vscdb", canRequest: true, settingsUrl: null, required: true },
    { id: "client_configs", title: "写入客户端配置", usedBy: "一键接入（Claude Code / Codex）", status: "ok" as const, detail: "/Users/me/.claude · /Users/me/.codex", canRequest: true, settingsUrl: null, required: false },
    { id: "cursor_app", title: "修改 Cursor 安装", usedBy: "Sand 补丁", status: (DENIED ? "denied" : "unknown") as "denied" | "unknown" | "ok", detail: DENIED ? "在系统设置 → 隐私与安全性 → App 管理 里允许 Nexus。" : "还没申请过。", canRequest: true, settingsUrl: "x-apple.systempreferences:com.apple.preference.security?Privacy_AppBundles", required: false },
    { id: "cursor_automation", title: "退出与重启 Cursor", usedBy: "冷切换、Sand 补丁", status: "unknown" as "denied" | "unknown" | "ok", detail: "还没申请过。", canRequest: true, settingsUrl: "x-apple.systempreferences:com.apple.preference.security?Privacy_Automation", required: false },
  ],
};

// ── Sand ───────────────────────────────────────────────────────────────────

const SAND_RELEASE: CursorRelease = {
  version: "3.19.13",
  platform: "macos",
  downloads: [
    {
      architecture: "universal",
      url: "https://downloads.cursor.com/production/90de2327392570a5f5f625c656c6749d228e6437/darwin/universal/Cursor-darwin-universal.dmg",
    },
  ],
};

const MARKERS = {
  clientType: 23,
  eligibility: 0,
  managedLocalRoute: 1,
  localRuntimeLoad: 1,
  inferenceStream: 1,
  agentHostEnablement: 2,
  agentHostIdentity: 1,
  agentHostMoveExec: 1,
  managedSubagentRoute: 0,
  managedSubagentSession: 0,
  managedTaskTool: 1,
  managedActionRoute: 1,
  subagentResumeMode: 1,
  subagentCompletionWake: 2,
  subagentInteractionBubble: 1,
  subagentModelVariants: 2,
  contextWindow: 1,
  inferenceEndpoint: 0,
  grokbotStreamAuth: 0,
};

/**
 * 远程主机（remote SSH）预览：一台走网关、已装好、隧道已连；一台走代理、代理设置已配；
 * 一台连不上。第一台盘上还留着旧版「经本机网关」的改道——预览里要能看到那条红横幅。
 */
const REMOTE_OVERVIEW: RemoteOverview = {
  localCommit: "dd066f332fcea7382764400fde902f61920648d0",
  detectedProxyPort: 7890,
  hosts: EMPTY
    ? []
    : [
        {
          host: { host: "devbox-01", label: "公司工作站", route: "proxy", remotePort: 41777, proxyPort: null },
          status: {
            Ok: {
              host: "devbox-01",
              servers: [
                { commit: "2ba48ff3f7514cc4643c52ca9f7b3173d9b66130", version: "3.18.9", root: "/home/u/.cursor-server/bin/linux-x64/2ba48ff3" },
                { commit: "dd066f332fcea7382764400fde902f61920648d0", version: "3.19.13", root: "/home/u/.cursor-server/bin/linux-x64/dd066f33" },
              ],
              selected: { commit: "dd066f332fcea7382764400fde902f61920648d0", version: "3.19.13", root: "/home/u/.cursor-server/bin/linux-x64/dd066f33" },
              versionSupported: true,
              commitMatchesLocal: true,
              markers: {
                ...MARKERS,
                clientType: 2,
                agentHostEnablement: 0,
                subagentCompletionWake: 0,
                subagentModelVariants: 0,
                inferenceEndpoint: 2,
              },
              patchedFiles: [
                "out/vs/workbench/api/node/extensionHostProcess.js",
                "extensions/cursor-agent-host/dist/main.js",
                "extensions/cursor-agent-host/dist/4884.js",
              ],
              inferenceEndpoint: "http://127.0.0.1:41777",
              complete: true,
            },
          },
          tunnel: { spec: { host: "devbox-01", remotePort: 41777, localPort: 7890 }, phase: "connected", reconnects: 1, lastError: null, streams: 2 },
          localPort: 7890,
          proxyConfigured: "http://127.0.0.1:41777",
          localListening: true,
        },
        {
          // 代理模式：不改端点，Cursor 设置里的 HTTP_PROXY 指着隧道的远程口。隧道此刻在重连。
          host: { host: "lab-2", label: "实验室", route: "proxy", remotePort: 21890, proxyPort: null },
          status: {
            Ok: {
              host: "lab-2",
              servers: [{ commit: "dd066f332fcea7382764400fde902f61920648d0", version: "3.19.13", root: "/home/u/.cursor-server/bin/linux-x64/dd066f33" }],
              selected: { commit: "dd066f332fcea7382764400fde902f61920648d0", version: "3.19.13", root: "/home/u/.cursor-server/bin/linux-x64/dd066f33" },
              versionSupported: true,
              commitMatchesLocal: true,
              markers: {
                ...MARKERS,
                clientType: 2,
                agentHostEnablement: 0,
                subagentCompletionWake: 0,
                subagentModelVariants: 0,
                inferenceEndpoint: 0,
              },
              patchedFiles: [
                "out/vs/workbench/api/node/extensionHostProcess.js",
                "extensions/cursor-agent-host/dist/main.js",
              ],
              inferenceEndpoint: null,
              complete: true,
            },
          },
          tunnel: {
            spec: { host: "lab-2", remotePort: 21890, localPort: 7890 },
            phase: "reconnecting",
            reconnects: 2,
            lastError: "远程端口被别的程序占着（中继起不来）。给这台主机换一个远程端口。",
            streams: 0,
          },
          localPort: 7890,
          proxyConfigured: "http://127.0.0.1:21890",
          localListening: true,
        },
        {
          host: { host: "gpu-box", label: "", route: "direct", remotePort: 41777, proxyPort: null },
          status: { Err: "ssh gpu-box 失败（退出码 255）：Connection timed out during banner exchange\n连不上 gpu-box：确认网络 / VPN / ~/.ssh/config 里的 ProxyCommand。" },
          tunnel: { spec: null, phase: "stopped", reconnects: 0, lastError: null, streams: 0 },
          localPort: null,
          proxyConfigured: null,
          localListening: false,
        },
      ],
};

const SAND: SandStatus = {
  cursorVersion: "3.19.13",
  supportedVersion: "3.19.13",
  versionSupported: true,
  installed: !EMPTY,
  complete: !EMPTY,
  markers: EMPTY ? { ...MARKERS, clientType: 0 } : MARKERS,
  remainingIde: 0,
  foreignMarkers: 0,
  legacyMarkers: 0,
  patchedFiles: EMPTY ? [] : ["workbench.desktop.main.js", "4884.js", "main.js"],
  dryRun: null,
  backups: 2,
  // 预览里模拟「老安装：自摘要关」——正是这一页要提示用户重新安装切到新默认的状态。
  selfSummary: EMPTY ? null : false,
  grok45ViaCua: EMPTY ? null : false,
  inferenceEndpoint: null,
  grokbotAuth: EMPTY ? "off" : "box_relay",
  grokbotRelayConfigured: !EMPTY,
  grokbotDirectConfigured: !EMPTY,
};

const CRSR: CrsrStatus = {
  cursorVersion: "3.19.13",
  supportedVersion: "3.19.13",
  versionSupported: true,
  installed: false,
  complete: false,
  hits: 0,
  expectedHits: 2,
  anchors: 2,
  patchedFiles: [],
  sandConflict: null,
  backups: 0,
  credential: EMPTY
    ? null
    : {
        accountEmail: "arvid.pfeffer@outlook.com",
        accountId: "arvid.pfeffer@outlook.com",
        expiresAtMs: NOW + H,
        expired: false,
        canRenew: true,
      },
};


// ── 模型目录 ────────────────────────────────────────────────────────────────

const local = (id: string, series: string, variant: string, vendor: string, vendorLabel: string, aliases: string[] = [], note: string | null = null): LocalModel => ({
  id,
  vendor,
  vendorLabel,
  series,
  variant,
  aliases,
  note,
});

const LOCAL_MODELS: LocalModel[] = [
  local("cursor/auto", "cursor/auto", "standard", "cursor", "Cursor", ["claude-haiku-4", "haiku", "gpt-4o-mini", "gpt-4.1-mini", "gpt-3.5", "gpt-5-mini", "o1-mini"], "让 Cursor 按请求挑模型；认不出的客户端模型名也落到这里。"),
  local("cursor/claude-sonnet-5", "cursor/claude-sonnet-5", "standard", "anthropic", "Anthropic", ["claude-3-5-haiku", "claude-3-haiku", "claude-3-5-sonnet", "claude-3-7-sonnet", "claude-4-sonnet", "claude-sonnet-4"]),
  local("cursor/claude-opus-5", "cursor/claude-opus-5", "standard", "anthropic", "Anthropic", ["claude-3-opus", "claude-4-opus", "claude-opus-4"]),
  local("cursor/claude-opus-5-thinking-max-fast", "cursor/claude-opus-5", "thinking-max-fast", "anthropic", "Anthropic"),
  local("cursor/gpt-5.6-sol", "cursor/gpt-5.6-sol", "standard", "openai", "OpenAI", ["gpt-4o", "gpt-4.1", "gpt-4-turbo", "gpt-4", "gpt-5"]),
  local("cursor/gpt-5.6-sol-max-fast", "cursor/gpt-5.6-sol", "max-fast", "openai", "OpenAI"),
  local("cursor/gpt-5.6-terra", "cursor/gpt-5.6-terra", "standard", "openai", "OpenAI", ["gpt-5-codex", "o1"]),
  local("cursor/grok-4.6", "cursor/grok-4.6", "standard", "xai", "xAI", ["grok-3", "grok-2"]),
  local("cursor/grok-4.5", "cursor/grok-4.5", "standard", "xai", "xAI"),
  local("cursor/gemini-3.7-flash", "cursor/gemini-3.7-flash", "standard", "google", "Google", ["gemini-2.5-flash", "gemini-2.5-pro", "gemini-1.5", "gemini-"]),
  { ...local("cursor/nano-banana-2", "cursor/nano-banana-2", "standard", "google", "Google", ["gemini-3.1-flash-image"], "经 Cursor 出图，固定 1536×1024；账号需要 Developer 或 Sand 计划的生图权限。"), modality: "image" },
  local("grok/grok-4.6", "grok/grok-4.6", "standard", "xai", "xAI"),
  local("grok/grok-4.5", "grok/grok-4.5", "standard", "xai", "xAI"),
  { ...local("grok/grok-imagine-image", "grok/grok-imagine-image", "standard", "xai", "xAI", [], "经 Grok 订阅号出图。"), modality: "image" },
];

// ── 活动 / 状态 ────────────────────────────────────────────────────────────

const APP: AppStatus = {
  version: "0.1.0",
  cursor: OVERVIEW.check,
  cursorUserDir: "/Users/me/Library/Application Support/Cursor/User",
  cursorAppDir: NO_CURSOR ? null : "/Applications/Cursor.app",
  cursorVersion: NO_CURSOR ? null : "3.19.13",
  switchMachineIds: false,
  backupKeep: 10,
};

const ACTIVITY: ActivityEntry[] = [
  { id: 3, at: iso(40 * 60_000), level: "info", scope: "gateway", message: "网关已开：http://127.0.0.1:8787" },
  { id: 2, at: iso(2 * H), level: "info", scope: "switcher", email: "arvid.pfeffer@outlook.com", message: "切到这个号" },
  { id: 1, at: iso(3 * D), level: "warn", scope: "accounts", email: "junko.hale@outlook.com", message: "刷新用量失败：需要重新登录" },
];

const delay = <T,>(v: T, ms = 120): Promise<T> => new Promise((r) => setTimeout(() => r(v), ms));

/** 模拟一次流式回字：先思考几段，再逐词出正文，最后 done 带 usage。 */
async function fakeTry(id: string, model: string, prompt: string): Promise<void> {
  // `Omit` 会把可辨识联合拍平，所以这里按 TryEvent 收、再拼 id。
  const emit = (f: TryEvent) => emitMock(TRY_EVENT, { id, ...f } satisfies TryFrame);
  const routed = model === "auto" || model.endsWith("/auto") ? "composer-2.5-fast" : model;
  await delay(null, 350);
  emit({ kind: "routed", model: routed });
  for (const t of ["用户想让我", "介绍自己，", "并说出模型名。"]) {
    await delay(null, 160);
    emit({ kind: "thinking", text: t });
  }
  const answer = `我是经 Nexus 本地网关路由到的 ${routed}。你刚问的是：「${prompt.slice(0, 40)}」。我可以写代码、读文档、解释错误，也能在 Claude Code 或 Codex 里当你的后端。`;
  for (const piece of answer.match(/.{1,6}/g) ?? []) {
    await delay(null, 45);
    emit({ kind: "delta", text: piece });
  }
  await delay(null, 100);
  emit({ kind: "done", finish: "stop", usage: { promptTokens: 38, completionTokens: 72 } });
}

/**
 * 模拟一次切号：按真实顺序推进度事件。
 *
 * 事件名写死而不从 `ipc/api` 引 —— 那个模块 import 的正是本文件（被 vite alias 换掉的
 * `@tauri-apps/api/core`），引过来就成环了。
 */
async function fakeSwitch(email: string): Promise<void> {
  const emit = (p: SwitchProgress) => emitMock("switcher://progress", p);
  emit({ step: "started", email });
  await delay(null, 420);
  emit({ step: "backedUp", backupId: "b9", email: OVERVIEW.current?.email ?? null });
  await delay(null, 380);
  emit({ step: "hotLoginSent" });
  await delay(null, 520);
  emit({ step: "hotLoginConfirmed" });
  await delay(null, 160);
  emit({ step: "hotProfileWritten", written: 4, removed: 2 });
  await delay(null, 220);
  for (const profileEntry of PROFILES) {
    profileEntry.isCurrent = profileEntry.email === email;
    if (profileEntry.isCurrent) {
      profileEntry.lastSwitchedAt = new Date().toISOString();
    }
  }
  const sourceAccount = ACCOUNTS.find((entry) => entry.email === email);
  OVERVIEW.current = {
    email,
    membership: sourceAccount?.usage?.plan ?? sourceAccount?.membership ?? null,
    hasAccessToken: true,
    hasRefreshToken: true,
    keyCount: 6,
  };
  emit({ step: "done", email });
}

/**
 * `@tauri-apps/plugin-updater` 从 core 引这两个类；预览里没有更新器，给两个空壳让模块图能成型。
 * `plugin:updater|check` 落到下面的 default 分支回 undefined，更新提示就当「没有更新」。
 */
export class Resource {
  constructor(public readonly rid: number = 0) {}
  async close(): Promise<void> {}
}

export class Channel<T = unknown> {
  onmessage: (message: T) => void = () => {};
  toJSON(): string {
    return "__CHANNEL__:0";
  }
}

/** 真 Tauri 里把路径 / id 变成 WebView 能加载的地址；预览里只有游乐场的图会走它。 */
export function convertFileSrc(filePath: string, protocol = "asset"): string {
  return protocol === "nexus-image" ? fakeImageSrc(filePath) : `${protocol}://localhost/${encodeURIComponent(filePath)}`;
}

export async function invoke<T>(cmd: string, args?: Record<string, unknown>): Promise<T> {
  const v = (x: unknown) => delay(x as T);
  const pg = handlePlayground(cmd, args);
  if (pg) return pg as Promise<T>;
  switch (cmd) {
    case "app_status":
      return v(APP);
    case "app_activity":
      return v(ACTIVITY);
    case "app_update_settings": {
      // 真命令回的是改完之后的整份状态，设置页拿它直接换掉手上的那份。
      const patch = (args ?? {}) as Partial<AppStatus>;
      Object.assign(APP, Object.fromEntries(Object.entries(patch).filter(([, val]) => val !== undefined)));
      if ("cursorAppDir" in patch) APP.cursorAppDir = patch.cursorAppDir || null;
      return v({ ...APP });
    }
    case "switcher_restore_machine":
      return delay("3f9a…c1e2" as T, 500);

    case "switcher_overview":
      return v(OVERVIEW);
    case "switcher_list":
      return v(PROFILES);
    case "switcher_backups":
      return v(BACKUPS);
    case "switcher_capture_current":
      return delay(storeProfile(OVERVIEW.current?.email ?? "someone@example.com") as T, 500);
    case "switcher_remove": {
      const index = PROFILES.findIndex((entry) => entry.id === args?.id);
      if (index >= 0) PROFILES.splice(index, 1);
      return v(undefined);
    }
    case "switcher_remove_backup":
      return v(undefined);
    case "switcher_switch_to":
    case "switcher_restore_backup": {
      const p = PROFILES.find((x) => x.id === args?.id);
      const email = p?.email ?? BACKUPS.find((b) => b.id === args?.id)?.email ?? "someone@example.com";
      await fakeSwitch(email);
      return { email, backupId: "b9", machineSwitched: false, cursorRelaunched: false, hot: true } as T;
    }

    case "accounts_list":
      return v(ACCOUNTS);
    case "accounts_set_archived": {
      const ids = new Set((args?.ids as string[]) ?? []);
      const at = args?.archived ? new Date().toISOString() : null;
      const hit = ACCOUNTS.filter((a) => ids.has(a.id));
      for (const a of hit) a.archivedAt = at;
      return delay(hit as T, 300);
    }
    case "accounts_refresh_usage": {
      const a = ACCOUNTS.find((x) => x.id === args?.id);
      if (!a?.usage) throw { code: "invalid_input", message: "这个号没有 refresh_token，查不了用量。", hint: "先授权。" };
      a.usage = { ...a.usage, fetchedAt: new Date().toISOString() };
      a.lastCheckedAt = a.usage.fetchedAt;
      return delay(a.usage as T, 700);
    }
    case "accounts_refresh_billing": {
      const a = ACCOUNTS.find((x) => x.id === args?.id);
      if (!a) throw { code: "account_not_found", message: "没有这个账号。" };
      a.billing = billing({
        ...(a.billing ?? {}),
        fetchedAt: new Date().toISOString(),
      });
      return delay(a.billing as T, 800);
    }
    case "accounts_set_on_demand": {
      const a = ACCOUNTS.find((x) => x.id === args?.id);
      if (!a?.usage) throw { code: "invalid_input", message: "这个号查不了用量，改不了按需。", hint: "先刷一次用量。" };
      const enabled = Boolean(args?.enabled);
      const limitCents = typeof args?.limitCents === "number" ? args.limitCents : null;
      const fetchedAt = new Date().toISOString();
      a.usage = {
        ...a.usage,
        onDemandEnabled: enabled,
        onDemandLimitCents: enabled ? limitCents : a.usage.onDemandLimitCents,
        fetchedAt,
      };
      a.lastCheckedAt = fetchedAt;
      return delay(a.usage as T, 500);
    }
    case "chatgpt_list":
      return v(CHATGPT_ACCOUNTS);
    case "chatgpt_models":
      return v([
        { slug: "gpt-5.4", reasoningLevels: ["low", "high"], preferWebsockets: true },
        { slug: "gpt-5.3-codex-spark", reasoningLevels: ["low", "medium"], preferWebsockets: true },
      ]);
    case "chatgpt_refresh_usage": {
      const a = CHATGPT_ACCOUNTS.find((x) => x.id === args?.id);
      if (!a?.usage) throw { code: "account_not_found", message: "没有这个 ChatGPT 账号。" };
      a.usage = { ...a.usage, checkedAt: new Date().toISOString() };
      a.lastCheckedAt = a.usage.checkedAt;
      if (a.billing) a.billing = { ...a.billing, checkedAt: a.usage.checkedAt };
      return delay(a.usage as T, 500);
    }
    case "chatgpt_refresh_billing": {
      const a = CHATGPT_ACCOUNTS.find((x) => x.id === args?.id);
      if (!a) throw { code: "account_not_found", message: "没有这个 ChatGPT 账号。" };
      a.billing = {
        planType: a.planType,
        subscriptionPlan: a.planType === "plus" ? "chatgptplusplan" : "chatgptproplan",
        hasActiveSubscription: true,
        expiresAt: a.billing?.expiresAt ?? new Date(NOW + 12 * D).toISOString(),
        willRenew: a.billing?.willRenew ?? true,
        billingPeriod: a.billing?.billingPeriod ?? "monthly",
        checkedAt: new Date().toISOString(),
        source: "accounts/check",
      };
      return delay(a.billing as T, 700);
    }
    case "chatgpt_set_enabled": {
      const a = CHATGPT_ACCOUNTS.find((x) => x.id === args?.id);
      if (!a) throw { code: "account_not_found", message: "没有这个 ChatGPT 账号。" };
      a.enabled = Boolean(args?.enabled);
      return v(a);
    }
    case "chatgpt_set_note": {
      const a = CHATGPT_ACCOUNTS.find((x) => x.id === args?.id);
      if (!a) throw { code: "account_not_found", message: "没有这个 ChatGPT 账号。" };
      a.note = typeof args?.note === "string" && args.note.trim() ? args.note : null;
      return v(a);
    }

    case "accounts_refresh_all":
      return v(ACCOUNTS.length);
    case "accounts_export_dump":
      return v({
        path: "/Users/me/.roviix/exports/nexus-accounts-20260903T080000Z.json",
        count: ACCOUNTS.length,
      });
    case "accounts_copy_selected": {
      const ids = new Set((args?.ids as string[]) ?? []);
      const format = String(args?.format ?? "email");
      const info = (args?.info as Record<string, string> | undefined) ?? {};
      const hit = ACCOUNTS.filter((a) => ids.has(a.id));
      if (format === "json") {
        const entries = hit.map((a) => ({ email: a.email, ...(info[a.id]?.trim() ? { info: info[a.id] } : {}) }));
        return delay(JSON.stringify({ format: "nexus-accounts/1", accounts: entries }, null, 2) as T, 200);
      }
      const tail =
        format === "email_password"
          ? "----password123"
          : format === "email_refresh"
            ? "----rt_fake_token_here"
            : format === "email_session"
              ? "----user_01MOCKUSER::eyJhbGciOiJIUzI1NiJ9.session_token_here.sig"
              : "";
      // 与 Rust 侧同一规则：说明另起一行；带了说明账号之间空一行。
      const annotated = hit.some((a) => info[a.id]?.trim());
      const blocks = hit.map((a) => `${a.email}${tail}${info[a.id]?.trim() ? `\n${info[a.id]}` : ""}`);
      return delay(blocks.join(annotated ? "\n\n" : "\n") as T, 200);
    }
    case "accounts_add_to_switch_book":
      return delay(storeProfile(String(args?.id ?? "")) as T, 900);

    case "backup_list":
      return v([...LOCAL_BACKUPS]);
    case "backup_create": {
      const made = localBackup(0, "manual", 414 * 1024);
      LOCAL_BACKUPS.unshift(made);
      return delay(made as T, 400);
    }
    case "backup_restore": {
      const safety = localBackup(0, "pre-restore", 414 * 1024);
      LOCAL_BACKUPS.unshift(safety);
      return delay({ restored: String(args?.fileName ?? ""), safety } as T, 700);
    }
    case "backup_remove": {
      const index = LOCAL_BACKUPS.findIndex((b) => b.fileName === args?.fileName);
      if (index >= 0) LOCAL_BACKUPS.splice(index, 1);
      return v(undefined);
    }
    case "backup_reveal":
      return v(undefined);

    case "gateway_status":
    case "gateway_set_current":
    case "gateway_reset_lane":
      return v({ ...GATEWAY, lane: laneSnapshot() });
    case "gateway_start":
      GATEWAY.running = { addr: "127.0.0.1:8787", baseUrl: "http://127.0.0.1:8787", startedAt: new Date().toISOString() };
      return delay({ ...GATEWAY, lane: laneSnapshot() } as T, 400);
    case "gateway_stop":
      GATEWAY.running = null;
      return delay({ ...GATEWAY, lane: laneSnapshot() } as T, 300);
    case "gateway_usage":
      return delay(usageSummary(Number(args?.days ?? 7)) as T, 260);
    case "gateway_enroll":
      for (const l of (args?.labels as string[]) ?? []) ROSTER.add(l.toLowerCase());
      return delay({ ...GATEWAY, lane: laneSnapshot() } as T, 400);
    case "gateway_unenroll":
      ROSTER.delete(String(args?.label ?? "").toLowerCase());
      return delay({ ...GATEWAY, lane: laneSnapshot() } as T, 300);
    case "gateway_update_settings": {
      const patch = (args?.patch ?? {}) as Partial<GatewayStatus["settings"]>;
      Object.assign(GATEWAY.settings, Object.fromEntries(Object.entries(patch).filter(([, val]) => val !== undefined)));
      return v({ ...GATEWAY.settings });
    }
    case "gateway_reveal_key":
    case "gateway_rotate_key":
      return v("nx-local-7f3a9c2e1b");
    case "gateway_models":
      return v(LOCAL_MODELS);
    case "gateway_try": {
      if (!GATEWAY.running) throw { code: "invalid_input", message: "网关没开。", hint: "先在「本地网关」里开启。" };
      const a = args as { requestId: string; model: string; prompt: string };
      await fakeTry(a.requestId, a.model, a.prompt);
      return undefined as T;
    }

    case "connect_inspect":
      return v(CLIENTS[String(args?.tool)]);
    case "connect_apply": {
      const a = args as { tool: string; model: string };
      const c = CLIENTS[a.tool];
      if (!c) throw { code: "invalid_input", message: `「${a.tool}」没有可以直接写的配置文件。` };
      const created = !c.exists;
      const backup = created || c.revertible ? null : `/Users/me/.roviix/backups/clients/${a.tool}/${c.path.split("/").pop()}.20260903T090000Z`;
      Object.assign(c, { exists: true, baseUrl: "http://127.0.0.1:8787", model: a.model, revertible: true, appliedAt: new Date().toISOString(), pointsTo: "local" });
      const files = [{ path: c.path, created, backup }];
      if (a.tool === "codex") files.push({ path: "/Users/me/.codex/auth.json", created: true, backup: null });
      return delay({ files } as T, 600);
    }
    case "connect_revert": {
      const c = CLIENTS[String(args?.tool)];
      if (!c) throw { code: "invalid_input", message: "没有这个工具。" };
      const restored = c.revertible && c.pointsTo !== "none" ? [c.path] : [];
      Object.assign(c, { exists: false, baseUrl: null, model: null, revertible: false, appliedAt: null, pointsTo: "none" });
      return delay({ restored, removed: restored.length ? [] : [c.path], stripped: [] } as T, 400);
    }

    case "perms_check":
      return v({ ...PERMS, preflightDone: PREFLIGHT_DONE });
    case "perms_request": {
      const id = args?.id as string | null | undefined;
      for (const it of PERMS.items) {
        if (id && it.id !== id) continue;
        if (it.status === "unknown") {
          it.status = DENIED && it.id === "cursor_app" ? "denied" : "ok";
          if (it.status === "ok") it.detail = "刚申请过，已允许。";
        }
      }
      return delay({ ...PERMS, preflightDone: PREFLIGHT_DONE } as T, 900);
    }
    case "perms_mark_preflight":
      PREFLIGHT_DONE = true;
      return v(undefined);
    case "perms_open_settings":
      return v(undefined);

    case "sand_release":
      return v(SAND_RELEASE);
    case "sand_status":
      return v(SAND);
    case "sand_backups":
      return v([]);
    case "crsr_status":
      return v(CRSR);
    case "crsr_backups":
      return v([]);
    case "sand_remote_overview":
      return delay(REMOTE_OVERVIEW as T, 500);
    case "sand_remote_hosts":
      return v(REMOTE_OVERVIEW.hosts.map((h) => h.host));
    case "sand_remote_tunnel_status": {
      const h = REMOTE_OVERVIEW.hosts.find((x) => x.host.host === args?.host);
      return v(h?.tunnel ?? { spec: null, phase: "stopped", reconnects: 0, lastError: null, streams: 0 });
    }
    case "sand_remote_tunnel_start": {
      const h = REMOTE_OVERVIEW.hosts.find((x) => x.host.host === args?.host);
      const remotePort = h?.host.remotePort ?? 41777;
      const localPort = h?.localPort ?? 8788;
      const t = { spec: { host: String(args?.host), remotePort, localPort }, phase: "connecting" as const, reconnects: 0, lastError: null, streams: 0 };
      if (h) h.tunnel = t;
      return v(t);
    }
    case "sand_remote_tunnel_stop": {
      const h = REMOTE_OVERVIEW.hosts.find((x) => x.host.host === args?.host);
      const t = { spec: null, phase: "stopped" as const, reconnects: 0, lastError: null, streams: 0 };
      if (h) h.tunnel = t;
      return v(t);
    }
    case "sand_remote_update_host": {
      const next = args?.host as RemoteOverview["hosts"][number]["host"];
      const slot = REMOTE_OVERVIEW.hosts.find((x) => x.host.host === next?.host);
      if (slot) {
        slot.host = next;
        // 跟着 Rust 侧的语义：切到代理模式才有那份代理设置，切走就摘掉。
        const local = next.proxyPort ?? REMOTE_OVERVIEW.detectedProxyPort ?? 7890;
        slot.localPort = next.route === "proxy" ? local : null;
        slot.proxyConfigured = next.route === "proxy" ? `http://127.0.0.1:${next.remotePort}` : null;
      }
      return delay(REMOTE_OVERVIEW.hosts.map((x) => x.host) as T, 400);
    }
    // 探针：隧道连着的那台报通，其余照实报断在第一跳——两种结果都要能在预览里看到。
    case "sand_remote_probe": {
      const h = REMOTE_OVERVIEW.hosts.find((x) => x.host.host === args?.host);
      const port = h?.host.remotePort ?? 41777;
      return delay(
        (h?.host.route === "proxy" && h.tunnel.phase === "connected"
          ? { ok: true, stage: "http", status: 404, detail: null, remotePort: port }
          : {
              ok: false,
              stage: "tunnel",
              status: null,
              detail: `connect ECONNREFUSED 127.0.0.1:${port}`,
              remotePort: port,
            }) as T,
        2600,
      );
    }

    default:
      console.warn("[preview] 没有 fixture 的命令", cmd, args);
      return v(undefined);
  }
}
