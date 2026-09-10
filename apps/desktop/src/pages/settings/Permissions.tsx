/**
 * 权限清单：我们要动哪几样东西、各自现在能不能动。
 *
 * 两处用它：设置页的「权限」页签，和首次启动的「准备工作」弹窗。同一份组件，免得两处对
 * 「哪些项、什么状态、怎么申请」各说各话。
 *
 * 每一行：图标 / 名字 / 给谁用 / 状态 / 动作。状态只有四种词：已允许、被拒、未申请、不适用。
 * 被拒的给「打开系统设置」，未申请的给「申请」，已允许的什么都不给 —— 一列里绝大多数该是已允许，
 * 每行都挂个绿键等于没挂。同理，图标框只在被拒 / 未申请时变色：一列扫下来，有颜色的就是要处理的。
 */
import { useCallback, useEffect, useState } from "react";
import { errorText, perms as permsApi } from "../../ipc/api";
import type { PermItem, PermReport, PermStatus } from "../../ipc/types";
import { Banner, Health, Icon, Spinner } from "../../ui/primitives";

const STATUS_LABEL: Record<PermStatus, string> = {
  ok: "已允许",
  denied: "被拒",
  unknown: "未申请",
  not_applicable: "不适用",
};

/** 每一项权限对应一个隐喻：钥匙 = 登录态，笔 = 写配置，立方体 = 程序本体，电源 = 退出重启。 */
const ICONS: Record<string, string> = {
  cursor_data: "key",
  client_configs: "pencil",
  cursor_app: "box",
  cursor_automation: "power",
};

/** 行的色调：只给要处理的两种状态上色。 */
const ROW_TONE: Record<PermStatus, string> = {
  ok: "",
  denied: " is-bad",
  unknown: " is-warn",
  not_applicable: " is-off",
};

export function usePermissions() {
  const [report, setReport] = useState<PermReport | null>(null);
  const [busy, setBusy] = useState<string | "all" | null>(null);
  const [error, setError] = useState<string | null>(null);

  const check = useCallback(async () => {
    try {
      setReport(await permsApi.check());
      setError(null);
    } catch (e) {
      setError(errorText(e));
    }
  }, []);

  useEffect(() => {
    void check();
  }, [check]);

  /** 申请。`id` 缺省 = 全部会弹窗的项一起问 —— 首次启动就该一次问完。 */
  const request = useCallback(async (id?: string) => {
    setBusy(id ?? "all");
    setError(null);
    try {
      setReport(await permsApi.request(id));
    } catch (e) {
      setError(errorText(e));
    } finally {
      setBusy(null);
    }
  }, []);

  const openSettings = useCallback(async (id: string) => {
    try {
      await permsApi.openSettings(id);
    } catch (e) {
      setError(errorText(e));
    }
  }, []);

  return { report, busy, error, check, request, openSettings };
}

/** 有没有哪一项还没搞定（未申请或被拒），只算能申请的。 */
export function pendingItems(report: PermReport | null): PermItem[] {
  return (report?.items ?? []).filter((i) => (i.status === "unknown" || i.status === "denied") && i.canRequest);
}

export function PermissionList({
  report,
  busy,
  error,
  flush,
  onRequest,
  onOpenSettings,
}: {
  report: PermReport | null;
  busy: string | null;
  error: string | null;
  /** 已经在弹窗里：不再套一张卡，只留行间的发丝线。 */
  flush?: boolean;
  onRequest: (id?: string) => void;
  onOpenSettings: (id: string) => void;
}) {
  if (!report) {
    return (
      <div className="stack" style={{ gap: 8 }}>
        {error ? <Banner tone="bad" title={error} /> : <div className="skeleton" style={{ height: 168, borderRadius: 12 }} />}
      </div>
    );
  }
  return (
    <div className="stack" style={{ gap: 10 }}>
      {error ? <Banner tone="bad" title={error} /> : null}
      <div className={flush ? "opts is-flush" : "opts"}>
        {report.items.map((it) => {
          const pending = it.status === "unknown" || it.status === "denied";
          // 被拒时那句「去系统设置哪里允许」是下一步，得摆出来；其余时候补充信息退到悬停里。
          const desc = it.status === "denied" && it.detail ? it.detail : it.usedBy;
          return (
            <div key={it.id} className={`opt${ROW_TONE[it.status]}`} title={desc === it.detail ? undefined : it.detail ?? undefined}>
              <span className="opt-ico">
                <Icon name={ICONS[it.id] ?? "shield"} size={15} />
              </span>
              <div className="opt-copy">
                <div className="opt-title">
                  <span className="truncate">{it.title}</span>
                  {!it.required && pending ? <span className="tag">可选</span> : null}
                </div>
                <div className="opt-desc truncate">{desc}</div>
              </div>
              <div className="opt-ctl">
                {it.status === "ok" ? <Health tone="ok">{STATUS_LABEL.ok}</Health> : null}
                {it.status === "denied" ? <Health tone="bad">{STATUS_LABEL.denied}</Health> : null}
                {it.status === "unknown" ? <Health tone="warn">{STATUS_LABEL.unknown}</Health> : null}
                {it.status === "not_applicable" ? <span className="faint tiny">{STATUS_LABEL.not_applicable}</span> : null}
                {/* 行内的「申请」不做主按钮：主按钮是标题栏 / 弹窗页脚那一个「一次申请」，
                    一屏里同时亮三个绿键，就没有哪个是主的了。 */}
                {pending && it.canRequest ? (
                  <button type="button" className="btn btn-sm" disabled={busy != null} onClick={() => onRequest(it.id)}>
                    {busy === it.id || busy === "all" ? <Spinner /> : it.status === "denied" ? "重新申请" : "申请"}
                  </button>
                ) : null}
                {it.status === "denied" && it.settingsUrl ? (
                  <button type="button" className="btn btn-sm btn-icon btn-quiet" aria-label="打开系统设置" onClick={() => onOpenSettings(it.id)}>
                    <Icon name="external" size={13} />
                  </button>
                ) : null}
              </div>
            </div>
          );
        })}
      </div>
    </div>
  );
}
