/**
 * Qoder 卡片和抽屉共用的展示。套餐名、额度百分比都从快照来，这里不编数字。
 */
import type { DeviceAccount, QoderQuotaBucket, QoderUsage } from "../../ipc/types";

export function qoderTitle(a: Pick<DeviceAccount, "email" | "displayName" | "label" | "accountRef">): string {
  const email = a.email?.trim();
  if (email) return email;
  const name = a.displayName?.trim();
  if (name) return name;
  const label = a.label?.trim();
  if (label) return label;
  return `qoder…${a.accountRef.slice(-6)}`;
}

export function qoderRegion(backend: DeviceAccount["backend"]): string {
  return backend === "cn" ? "国内版" : "国际版";
}

export function qoderPlan(a: DeviceAccount): string {
  const plan = a.qoderUsage?.plan?.trim();
  return plan || "未同步套餐";
}

export function userTypeLabel(raw: string | null | undefined): string | null {
  if (!raw?.trim()) return null;
  const m = raw.trim().toLowerCase();
  if (m === "personal" || m === "individual") return "个人";
  if (m === "pro" || m === "professional") return "Pro";
  if (m === "team" || m === "teams") return "团队";
  if (m === "enterprise" || m === "org" || m === "organization") return "企业";
  if (m === "free") return "免费";
  return raw.trim();
}

export function bucketLabel(b: QoderQuotaBucket): string {
  if (b.id === "plan") return "套餐内";
  if (b.id === "addon") return "加购";
  if (b.id === "org") return "组织席位";
  return b.label?.replace(/额度/g, "")?.trim() || "额度";
}

export function primaryBucket(usage: QoderUsage | null | undefined): QoderQuotaBucket | null {
  const buckets = usage?.buckets ?? [];
  return (
    buckets.find((b) => b.id === "plan") ??
    buckets.find((b) => b.percent != null || b.total != null) ??
    buckets[0] ??
    null
  );
}

/** 主额度已用百分比。用尽时是 100；没有数字时是 null，不要画一条空条。 */
export function usedPercent(usage: QoderUsage | null | undefined): number | null {
  if (!usage) return null;
  if (usage.exceeded) return 100;
  const bucket = primaryBucket(usage);
  if (bucket?.percent != null && Number.isFinite(bucket.percent)) return clamp(bucket.percent);
  if (usage.percent != null && Number.isFinite(usage.percent)) return clamp(usage.percent);
  return null;
}

export function qty(n: number | null | undefined): string {
  if (n == null || !Number.isFinite(n)) return "—";
  const rounded = Math.abs(n - Math.round(n)) < 0.05 ? Math.round(n) : Math.round(n * 10) / 10;
  return rounded.toLocaleString("zh-CN");
}

export function creditUnit(usage: QoderUsage | null | undefined, bucket: QoderQuotaBucket | null): string {
  return bucket?.unit?.trim() || usage?.unit?.trim() || "Credits";
}

export function day(iso: string | null | undefined): string {
  if (!iso) return "—";
  const t = Date.parse(iso);
  if (!Number.isFinite(t)) return "—";
  const d = new Date(t);
  const m = String(d.getMonth() + 1).padStart(2, "0");
  const dd = String(d.getDate()).padStart(2, "0");
  return `${d.getFullYear()}-${m}-${dd}`;
}

export function leftLabel(iso: string | null | undefined, now = Date.now()): string {
  if (!iso) return "重置日未知";
  const t = Date.parse(iso);
  if (!Number.isFinite(t)) return "重置日未知";
  const days = Math.ceil((t - now) / 86_400_000);
  if (days <= 0) return "今天重置";
  if (days === 1) return "剩 1 天";
  return `剩 ${days} 天`;
}

export function resetSoon(iso: string | null | undefined, now = Date.now()): boolean {
  if (!iso) return false;
  const t = Date.parse(iso);
  return Number.isFinite(t) && t - now < 3 * 86_400_000 && t >= now - 86_400_000;
}

export function tokenLeft(iso: string | null | undefined, now = Date.now()): string {
  if (!iso) return "—";
  const t = Date.parse(iso);
  if (!Number.isFinite(t)) return "—";
  const sec = Math.round((t - now) / 1000);
  if (sec <= 0) return "即将续期";
  if (sec < 3600) return `${Math.max(1, Math.floor(sec / 60))} 分钟`;
  if (sec < 86400) return `${Math.floor(sec / 3600)} 小时`;
  return `${Math.floor(sec / 86400)} 天`;
}

function clamp(n: number): number {
  return Math.max(0, Math.min(100, Math.round(n)));
}
