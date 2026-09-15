/**
 * 订阅账单的展示语汇。
 *
 * 门户读到的折扣有四种结论，其中 `unknown` 和 `none` 不能混：没读到不是没有。
 */
import type { AccountBilling, BillingDiscount, DiscountState } from "../ipc/types";
import { moneyFx } from "./usage";

export function discountStateLabel(state?: DiscountState | null): string {
  switch (state) {
    case "active":
      return "折扣生效";
    case "none":
      return "无折扣";
    case "expired":
      return "折扣已结束";
    default:
      return "未知";
  }
}

export function durationText(d?: BillingDiscount | null): string {
  if (!d?.duration) return "";
  if (d.duration === "once") return "一次性";
  if (d.duration === "forever") return "长期有效";
  if (d.duration === "repeating") {
    return d.durationInMonths ? `重复 ${d.durationInMonths} 个月` : "按月重复";
  }
  return d.duration;
}

export function discountOffText(d?: BillingDiscount | null, currency?: string | null): string {
  if (!d) return "";
  if (d.percentOff != null) return `−${trimNum(d.percentOff)}%`;
  if (d.amountOff != null) return `−${moneyFx(d.amountOff, d.currency ?? currency)}`;
  return "";
}

export function intervalLabel(interval?: string | null): string {
  switch (interval) {
    case "month":
      return "月付";
    case "year":
      return "年付";
    case "week":
      return "周付";
    case "day":
      return "日付";
    default:
      return interval ?? "";
  }
}

export function invoiceStatusLabel(status?: string | null): string {
  switch (status) {
    case "paid":
      return "已付";
    case "open":
      return "待付";
    case "draft":
      return "草稿";
    case "uncollectible":
      return "无法收取";
    case "void":
      return "已作废";
    default:
      return status ?? "";
  }
}

export function subStatusLabel(status?: string | null): string {
  switch (status) {
    case "active":
      return "订阅中";
    case "trialing":
      return "试用";
    case "past_due":
      return "欠费";
    case "canceled":
      return "已取消";
    case "unpaid":
      return "未付款";
    case "incomplete":
      return "未完成";
    default:
      return status ?? "";
  }
}

/** 订阅异常才说：active 是默认态，摆出来是噪音。 */
export function subStatusAlert(status?: string | null): "warn" | "bad" | null {
  if (status === "trialing" || status === "past_due" || status === "unpaid") return "warn";
  if (status === "canceled" || status === "incomplete") return "bad";
  return null;
}

export function collectionLabel(method?: string | null): string {
  if (method === "charge_automatically") return "自动续费";
  if (method === "send_invoice") return "账单收款";
  return "";
}

export function productName(billing?: AccountBilling | null): string {
  return billing?.items?.find((i) => i.name)?.name || "";
}

function trimNum(n: number): string {
  return Number.isInteger(n) ? String(n) : n.toFixed(1).replace(/\.0$/, "");
}
