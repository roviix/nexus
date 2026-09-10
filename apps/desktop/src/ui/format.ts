/** 显示层的格式化。纯函数，好测。 */

/** 相对时间：列表里「3 分钟前」比一串 ISO 有用得多。 */
export function timeAgo(iso?: string | null, now = Date.now()): string {
  if (!iso) return "—";
  const t = Date.parse(iso);
  if (!Number.isFinite(t)) return "—";
  const diff = Math.round((now - t) / 1000);
  if (diff < 0) return "刚刚";
  if (diff < 60) return `${diff} 秒前`;
  if (diff < 3600) return `${Math.floor(diff / 60)} 分钟前`;
  if (diff < 86400) return `${Math.floor(diff / 3600)} 小时前`;
  if (diff < 86400 * 30) return `${Math.floor(diff / 86400)} 天前`;
  return new Date(t).toLocaleDateString("zh-CN");
}

/** 未来时刻：额度重置显示「还有 3 天」。 */
export function timeUntil(ms?: number | null, now = Date.now()): string {
  if (ms == null || !Number.isFinite(ms)) return "—";
  const diff = Math.round((ms - now) / 1000);
  if (diff <= 0) return "已重置";
  if (diff < 3600) return `${Math.max(1, Math.floor(diff / 60))} 分钟后`;
  if (diff < 86400) return `${Math.floor(diff / 3600)} 小时后`;
  return `${Math.floor(diff / 86400)} 天后`;
}

/** cents → 「$12.34」。Cursor 的额度全是美分。 */
export function money(cents?: number | null): string {
  if (cents == null || !Number.isFinite(cents)) return "—";
  return `$${(cents / 100).toFixed(2)}`;
}

/** 人民币，用于商城。 */
export function yuan(amount?: number | null): string {
  if (amount == null || !Number.isFinite(amount)) return "—";
  return `¥${amount.toFixed(2)}`;
}

/** 邮箱打码。列表默认打码，展开才看全（§8）。 */
export function maskEmail(email: string): string {
  const [local = "", domain = ""] = email.split("@");
  if (!domain) return email;
  const keep = local.length <= 2 ? 1 : 2;
  const tail = local.length > keep + 2 ? local.slice(-2) : "";
  return `${local.slice(0, keep)}****${tail}@${domain}`;
}

const STATUS_LABEL: Record<string, string> = {
  active: "可用",
  needs_login: "待登录",
  dead: "已失效",
};

export function accountStatusLabel(status: string): string {
  return STATUS_LABEL[status] ?? status;
}

const SOURCE_LABEL: Record<string, string> = {
  local: "自有",
  purchased: "已购",
};

export function accountSourceLabel(source: string): string {
  return SOURCE_LABEL[source] ?? source;
}

const ORDER_LABEL: Record<string, string> = {
  pending: "待支付",
  paid: "已付款",
  delivered: "已交付",
  closed: "已关闭",
  refunded: "已退款",
  ship_failed: "发货失败",
  timeout: "等待超时",
};

export function orderStatusLabel(status: string): string {
  return ORDER_LABEL[status] ?? status;
}

/** 文件大小：备份清单里「412 KB」比一串字节数好读。 */
export function bytes(n?: number | null): string {
  if (n == null || !Number.isFinite(n) || n < 0) return "—";
  if (n < 1024) return `${n} B`;
  if (n < 1024 * 1024) return `${(n / 1024).toFixed(n < 10 * 1024 ? 1 : 0)} KB`;
  return `${(n / (1024 * 1024)).toFixed(1)} MB`;
}

/** 订阅档。Cursor 原样返回 `pro_plus` 这种，直接显示不好看。 */
export function membershipLabel(plan?: string | null): string {
  if (!plan) return "—";
  return plan.replace(/_/g, " ");
}
