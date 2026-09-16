/**
 * 设置。
 *
 * 这一页承担一个具体职责：**当别的页面出问题时，答案在这里。** Cursor 找不到、
 * 凭证存不住、切号降级为只读 —— 原因和处置都摆在这一屏最上面，不管停在哪个页签。
 *
 * 五个页签按「常改 → 少改 → 只看」排：通用（外观、切号）、权限、高级（路径、备份）、
 * 日志、关于。页签在地址里（`#settings/advanced`），别的页能直达。
 *
 * 版式只有一种：左边一个坐在小方框里的图标、中间一个名字、右边控件（`.opt`）。
 * 图标替掉解释 —— 一行设置该靠名字和图标说清楚自己是什么，说不清是名字没起好，
 * 不是少了一段小字。留下的说明只有两类：动作的后果（「会重启 Cursor」），出错后的下一步。
 */
import { openUrl } from "@tauri-apps/plugin-opener";
import { relaunch } from "@tauri-apps/plugin-process";
import { check as checkUpdate, type Update } from "@tauri-apps/plugin-updater";
import { useCallback, useEffect, useRef, useState, type KeyboardEvent, type ReactNode } from "react";
import { app, switcher } from "../ipc/api";
import type { ActivityEntry, AppStatus } from "../ipc/types";
import { go, SETTINGS_TABS, type Route, type SettingsTab } from "../shell/nav";
import { timeAgo } from "../ui/format";
import { Mark } from "../ui/Mark";
import { Banner, Empty, ErrorNote, Health, Icon, Opt, Spinner, Switch, Tag } from "../ui/primitives";
import { Appearance } from "./settings/Appearance";
import { LocalBackups } from "./settings/LocalBackups";
import { PermissionList, pendingItems, usePermissions } from "./settings/Permissions";

type SettingsPatch = Parameters<typeof app.updateSettings>[0];

export function SettingsPage({ route, onGo }: { route: Route; onGo: (r: Route) => void }) {
  const tab: SettingsTab = route.tab ?? "general";
  const [status, setStatus] = useState<AppStatus | null>(null);
  const [error, setError] = useState<unknown>(null);
  const [busy, setBusy] = useState(false);
  // 权限提在页面这一层：页签上的角标和页签里的清单是同一份数据。
  const perms = usePermissions();
  const pending = pendingItems(perms.report);

  const reload = useCallback(async () => {
    try {
      setStatus(await app.status());
      setError(null);
    } catch (err) {
      setError(err);
    }
  }, []);

  useEffect(() => {
    void reload();
  }, [reload]);

  async function save(patch: SettingsPatch): Promise<boolean> {
    setBusy(true);
    setError(null);
    try {
      setStatus(await app.updateSettings(patch));
      return true;
    } catch (err) {
      setError(err);
      return false;
    } finally {
      setBusy(false);
    }
  }

  const goTab = (t: SettingsTab) => onGo(go("settings", { tab: t }));

  if (!status) {
    return (
      <div className="set-page">
        <ErrorNote error={error} onRetry={() => void reload()} />
        {!error ? <Spinner /> : null}
      </div>
    );
  }

  // Rust 算好的（`SchemaCheck::writable`）。这里曾经自己拼一遍，还漏了「没人登着就该放行」
  // 那一支 —— 于是一台刚装好、还没登录的 Cursor 一进设置页就顶着「格式与预期不符」。
  const writable = status.cursor.writable;

  return (
    <div className="set-page">
      <div className="page-head">
        <h1>设置</h1>
      </div>

      <ErrorNote error={error} onRetry={() => void reload()} />

      {/* 只在真出事时说话，说的是后果和下一步，不是原因。 */}
      {!writable ? (
        <div style={{ marginBottom: 12 }}>
          <Banner
            tone="warn"
            title={status.cursor.dbPresent ? "Cursor 的登录态格式与预期不符，切号已降级为只读" : "没找到 Cursor，切号已降级为只读"}
            hint={status.cursor.blockedReason ?? undefined}
            action={
              tab !== "advanced" ? (
                <button type="button" className="btn btn-sm" onClick={() => goTab("advanced")}>
                  指定目录
                </button>
              ) : undefined
            }
          />
        </div>
      ) : null}
      <div className="tabs set-tabs" role="tablist" aria-label="设置">
        {SETTINGS_TABS.map((t) => (
          <button
            key={t.id}
            type="button"
            role="tab"
            className="tab"
            aria-selected={tab === t.id}
            onClick={() => goTab(t.id)}
          >
            <Icon name={t.icon} size={13} className="tab-ico" />
            {t.label}
            {t.id === "permissions" && pending.length > 0 ? <span className="tab-n is-hot">{pending.length}</span> : null}
          </button>
        ))}
      </div>

      {tab === "general" ? <GeneralTab status={status} busy={busy} onSave={save} onError={setError} onReload={reload} /> : null}
      {tab === "permissions" ? <PermissionsTab perms={perms} pendingCount={pending.length} /> : null}
      {tab === "advanced" ? <AdvancedTab status={status} busy={busy} onSave={save} /> : null}
      {tab === "log" ? <LogTab /> : null}
      {tab === "about" ? <AboutTab status={status} writable={writable} /> : null}
    </div>
  );
}

/* ── 骨架 ───────────────────────────────────────────────────────────────── */

function Sect({ icon, title, actions, sub, children }: { icon: string; title: string; actions?: ReactNode; sub?: ReactNode; children: ReactNode }) {
  return (
    <section className="set-sect">
      <header className="set-sect-head">
        <Icon name={icon} size={14} className="set-sect-ico" />
        <h2>{title}</h2>
        {actions ? <span className="set-sect-acts">{actions}</span> : null}
      </header>
      {sub ? <p className="set-sect-sub">{sub}</p> : null}
      {children}
    </section>
  );
}

/* ── 通用 ───────────────────────────────────────────────────────────────── */

function GeneralTab({
  status,
  busy,
  onSave,
  onError,
  onReload,
}: {
  status: AppStatus;
  busy: boolean;
  onSave: (p: SettingsPatch) => Promise<boolean>;
  onError: (e: unknown) => void;
  onReload: () => Promise<void>;
}) {
  const [restoring, setRestoring] = useState(false);

  async function restoreMachine() {
    setRestoring(true);
    try {
      await switcher.restoreMachine(false);
      await onReload();
    } catch (err) {
      onError(err);
    } finally {
      setRestoring(false);
    }
  }

  return (
    <>
      <Sect icon="contrast" title="外观">
        <div className="opts">
          <Opt icon="contrast" title="主题">
            <Appearance />
          </Opt>
        </div>
      </Sect>

      <Sect icon="switcher" title="切号">
        <div className="opts">
          <Opt icon="cpu" title="切号时同步机器码" desc="会退出并重启 Cursor" tone={status.switchMachineIds ? "on" : undefined}>
            <Switch
              label="切号时同步机器码"
              checked={status.switchMachineIds}
              disabled={busy}
              onChange={(next) => void onSave({ switchMachineIds: next })}
            />
          </Opt>
          <Opt icon="layers" title="登录态备份保留" desc="切号前自动留的份数">
            <input
              className="input num opt-num"
              type="number"
              min={1}
              max={200}
              aria-label="登录态备份保留份数"
              defaultValue={status.backupKeep}
              disabled={busy}
              onBlur={(e) => {
                const n = Number(e.target.value);
                if (Number.isFinite(n) && n >= 1 && n !== status.backupKeep) void onSave({ backupKeep: n });
              }}
            />
          </Opt>
          <Opt icon="undo" title="本机原始机器码" desc="还原到第一次切号前的那份">
            <button type="button" className="btn btn-sm" disabled={busy || restoring} onClick={() => void restoreMachine()}>
              {restoring ? "还原中…" : "还原"}
            </button>
          </Opt>
        </div>
      </Sect>
    </>
  );
}

/* ── 权限 ───────────────────────────────────────────────────────────────── */

function PermissionsTab({ perms, pendingCount }: { perms: ReturnType<typeof usePermissions>; pendingCount: number }) {
  return (
    <Sect
      icon="shield"
      title="权限"
      actions={
        <>
          <button
            type="button"
            className="btn btn-sm btn-icon btn-soft"
            aria-label="重新检测"
            disabled={perms.busy != null}
            onClick={() => void perms.check()}
          >
            <Icon name="refresh" size={13} />
          </button>
          {/* 只剩一项时行内那个键就够了，标题栏不再重复一个。 */}
          {pendingCount > 1 ? (
            <button type="button" className="btn btn-sm btn-primary" disabled={perms.busy != null} onClick={() => void perms.request()}>
              {perms.busy === "all" ? "申请中…" : `一次申请 ${pendingCount} 项`}
            </button>
          ) : null}
        </>
      }
    >
      <PermissionList
        report={perms.report}
        busy={perms.busy}
        error={perms.error}
        onRequest={(id) => void perms.request(id)}
        onOpenSettings={(id) => void perms.openSettings(id)}
      />
    </Sect>
  );
}

/* ── 高级 ───────────────────────────────────────────────────────────────── */

function AdvancedTab({ status, busy, onSave }: { status: AppStatus; busy: boolean; onSave: (p: SettingsPatch) => Promise<boolean> }) {
  return (
    <>
      <Sect icon="folder" title="路径">
        <div className="opts">
          <PathOpt
            icon="folder"
            title="Cursor 数据目录"
            desc="登录态所在，切号读写它"
            saved={status.cursorUserDir}
            busy={busy}
            restart
            onSave={(v) => onSave({ cursorUserDir: v })}
          />
          {/* 安装目录和数据目录是两件事：前者放程序本体（启动 Cursor、Sand 补丁要它），
              后者放登录态（切号读写它）。Windows 上 Cursor 可以装在任意盘符，探测更容易
              落空，所以它要能单独指定。

              输入框里回填的是**用户填过的原文**而不是生效的那个：填错了也得留在那儿，
              否则一保存就被抹掉，用户连改都无从改起。填了没生效就照实说是哪种情况。 */}
          <PathOpt
            icon="box"
            title="Cursor 安装目录"
            desc="程序本体，启动 Cursor 与 Sand 补丁要它"
            saved={status.cursorAppDirSetting || status.cursorAppDir || ""}
            placeholder="留空则自动探测"
            busy={busy}
            restart
            state={
              status.cursorAppDir ? undefined : status.cursorAppDirSetting.trim() ? (
                <Health tone="warn">这个目录里没有 Cursor</Health>
              ) : (
                <Health tone="warn">未检测到</Health>
              )
            }
            onSave={(v) => onSave({ cursorAppDir: v })}
          />
        </div>
      </Sect>

      <LocalBackups />
    </>
  );
}

/**
 * 一条路径设置：标题一行，输入框铺在下面一整行。「保存」只在改过之后露出来。
 * 保存成功后在标题右边留一个「重启后生效」，直到真的重启 —— 而不是常驻一句小字。
 */
function PathOpt({
  icon,
  title,
  desc,
  saved,
  placeholder,
  busy,
  restart,
  state,
  onSave,
}: {
  icon: string;
  title: string;
  desc?: string;
  saved: string;
  placeholder?: string;
  busy: boolean;
  /** 改了要重启应用才生效。 */
  restart?: boolean;
  /** 标题右边的状态词（比如「未检测到」）。 */
  state?: ReactNode;
  onSave: (value: string) => Promise<boolean>;
}) {
  const [value, setValue] = useState(saved);
  const [touched, setTouched] = useState(false);
  // 别处（还原备份、重新探测）改了值，输入框跟上；正在编辑的不打断。
  useEffect(() => {
    setValue(saved);
  }, [saved]);
  const dirty = value !== saved;

  async function commit() {
    if (!dirty || busy) return;
    if (await onSave(value.trim())) setTouched(true);
  }

  function onKey(e: KeyboardEvent<HTMLInputElement>) {
    if (e.key === "Enter") void commit();
    if (e.key === "Escape") setValue(saved);
  }

  return (
    <Opt
      icon={icon}
      title={title}
      desc={desc}
      body={
        <>
          <input
            className="input mono"
            aria-label={title}
            value={value}
            placeholder={placeholder}
            spellCheck={false}
            onChange={(e) => setValue(e.target.value)}
            onKeyDown={onKey}
          />
          <button
            type="button"
            className={`btn btn-sm btn-icon btn-soft opt-save${dirty ? "" : " is-idle"}`}
            aria-label="保存"
            disabled={busy || !dirty}
            onClick={() => void commit()}
          >
            <Icon name="check" size={13} />
          </button>
        </>
      }
    >
      {state}
      {touched && restart && !dirty ? <Tag tone="warn">重启后生效</Tag> : null}
    </Opt>
  );
}

/* ── 日志 ───────────────────────────────────────────────────────────────── */

function LogTab() {
  const [log, setLog] = useState<ActivityEntry[] | null>(null);
  const [error, setError] = useState<unknown>(null);

  const reload = useCallback(async () => {
    try {
      setLog(await app.activity(200));
      setError(null);
    } catch (err) {
      setError(err);
    }
  }, []);

  useEffect(() => {
    void reload();
  }, [reload]);

  return (
    <Sect
      icon="list"
      title="活动日志"
      actions={
        <>
          {log && log.length > 0 ? <span className="faint tiny mono">{log.length} 条</span> : null}
          <button type="button" className="btn btn-sm btn-icon btn-soft" aria-label="刷新" onClick={() => void reload()}>
            <Icon name="refresh" size={13} />
          </button>
        </>
      }
    >
      <ErrorNote error={error} onRetry={() => void reload()} />
      {log === null ? (
        !error ? <div className="skeleton" style={{ height: 140, borderRadius: 12 }} /> : null
      ) : log.length === 0 ? (
        <Empty>还没有记录</Empty>
      ) : (
        <div className="opts">
          <div className="log is-tall">
            {log.map((e) => (
              <div className={`log-row is-${e.level}`} key={e.id}>
                <span className="log-time mono">{timeAgo(e.at)}</span>
                <span className="log-scope mono">{e.scope}</span>
                <span className="grow selectable truncate">{e.message}</span>
                {e.email ? <span className="faint truncate log-who">{e.email}</span> : null}
              </div>
            ))}
          </div>
        </div>
      )}
    </Sect>
  );
}

/* ── 关于 ───────────────────────────────────────────────────────────────── */

type UpdateState =
  | { kind: "idle" }
  | { kind: "checking" }
  | { kind: "latest" }
  | { kind: "found"; version: string }
  | { kind: "installing" }
  | { kind: "failed"; message: string };

/** 项目主页：源码、Issue、Release 都在这。 */
const REPO_URL = "https://github.com/roviix/nexus";

function AboutTab({ status, writable }: { status: AppStatus; writable: boolean }) {
  const [upd, setUpd] = useState<UpdateState>({ kind: "idle" });
  const found = useRef<Update | null>(null);

  async function lookForUpdate() {
    setUpd({ kind: "checking" });
    try {
      const u = await checkUpdate({ timeout: 15_000 });
      found.current = u;
      setUpd(u ? { kind: "found", version: u.version } : { kind: "latest" });
    } catch (err) {
      setUpd({ kind: "failed", message: err instanceof Error ? err.message : String(err) });
    }
  }

  async function install() {
    const u = found.current;
    if (!u) return;
    setUpd({ kind: "installing" });
    try {
      await u.downloadAndInstall();
      await relaunch();
    } catch (err) {
      setUpd({ kind: "failed", message: err instanceof Error ? err.message : String(err) });
    }
  }

  // 版本状态跟在版本号后面，不各自占一枚徽章。「检查失败」那句连原因一起给 ——
  // 之前它藏在 title 里，用户看到两个字却问不出为什么。
  const note =
    upd.kind === "checking"
      ? "正在检查更新"
      : upd.kind === "latest"
        ? "已是最新"
        : upd.kind === "found"
          ? `有新版本 v${upd.version}`
          : upd.kind === "failed"
            ? `检查失败：${upd.message}`
            : null;

  return (
    <>
      {/* 关于这一行和别的设置行长一个样：品牌标坐进 `.opt` 那个 30px 的图标框里。
          上一版是一块 56px 圆角底 + 40px 标 + 17px 大字的「英雄区」，它下面紧跟着一列
          54px 的普通行，两种版式摞在一起就是「别扭」的来源 —— 这一页只该有一种行。 */}
      <Sect icon="box" title="关于">
        <div className="opts">
          <Opt
            icon={<Mark size={17} tight />}
            title="Nexus"
            desc={note ? `v${status.version} · ${note}` : `v${status.version}`}
            hint={upd.kind === "failed" ? upd.message : undefined}
            tone={upd.kind === "failed" ? "warn" : undefined}
          >
            {upd.kind === "found" || upd.kind === "installing" ? (
              <button type="button" className="btn btn-sm btn-primary" disabled={upd.kind === "installing"} onClick={() => void install()}>
                {upd.kind === "installing" ? <Spinner /> : <Icon name="download" size={13} />}
                {upd.kind === "installing" ? "安装中…" : "立即更新"}
              </button>
            ) : (
              <button
                type="button"
                className="btn btn-sm btn-icon btn-soft tip-end"
                data-tip={upd.kind === "checking" ? "检查中…" : "检查更新"}
                aria-label="检查更新"
                disabled={upd.kind === "checking"}
                onClick={() => void lookForUpdate()}
              >
                {upd.kind === "checking" ? <Spinner /> : <Icon name="download" size={13} />}
              </button>
            )}
            <button
              type="button"
              className="btn btn-sm btn-icon btn-soft tip-end"
              data-tip="打开 GitHub 仓库"
              aria-label="打开 GitHub 仓库"
              onClick={() => void openUrl(REPO_URL)}
            >
              <Icon name="external" size={13} />
            </button>
          </Opt>
        </div>
      </Sect>

      <Sect icon="info" title="环境">
        <div className="opts">
          <Opt icon="window" title="Cursor" desc={status.cursorAppDir ?? status.cursorUserDir} tone={writable ? undefined : "bad"}>
            {status.cursorVersion ? <span className="faint mono tiny">v{status.cursorVersion}</span> : null}
            <Health tone={writable ? "ok" : "bad"}>{writable ? "可切号" : "只读"}</Health>
          </Opt>
        </div>
      </Sect>
    </>
  );
}
