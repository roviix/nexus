/**
 * Cursor 面板 —— 一个问题、三个答案：**IDE 里的 Agent 面板由谁付账？**
 *
 *  - 原生：不改 Cursor。面板用它自己登着的号、按那个号的额度扣。要换号用「切号」。
 *  - CRSR：只把请求的 Bearer 换成某个账号 `crsr_` API Key 兑出来的票据。协议、路由、client-type
 *    一个字节不动，走的是 Cursor 的 API 计费口径。
 *  - Sand：把面板改道到 Cursor 内部的 sand 通道，用 Grok Bot 的额度付账。改得多、追着版本跑。
 *
 * 两条补丁改的是同一处 `applyAuthorization`，天然互斥；以前是侧栏里两个平级页面各自防着对方，
 * 用户要在两页之间来回对照才知道「现在到底装着哪个」。这里把它做成一个选择器：盘上装着什么
 * 一眼看到，选哪一档就只看那一档的东西。本地网关**不参与**这一页——面板走 `agent.v1` 到 api5，
 * 客户端没有任何指向 127.0.0.1 的口子，网关只服务标准方言的客户端。
 */
import { useCallback, useEffect, useState } from "react";
import { crsr, sand } from "../ipc/api";
import type { CrsrStatus, SandStatus } from "../ipc/types";
import { go, type PanelMode, type Route } from "../shell/nav";
import { Banner, ErrorNote, Icon, Opt, Tag } from "../ui/primitives";
import { CrsrPage } from "./CrsrPage";
import { SandPage } from "./SandPage";

export const MODES: Array<{ id: PanelMode; label: string; desc: string; icon: string }> = [
  {
    id: "native",
    label: "原生",
    desc: "不改 Cursor。面板用 IDE 里登着的号、扣它自己的额度；换号用「切号」。",
    icon: "shield",
  },
  {
    id: "crsr",
    label: "CRSR",
    desc: "只换 Bearer：用某个账号的 crsr_ API Key 付账，协议与路由不动。需要一把 crsr_ Key。",
    icon: "crsr",
  },
  {
    id: "sand",
    label: "Sand",
    desc: "改道到 Cursor 内部 sand 通道，用 Grok Bot 额度付账。改动多、跟 Cursor 版本硬绑。",
    icon: "sand",
  },
];

export const MODE_LABEL: Record<PanelMode, string> = {
  native: "原生",
  crsr: "CRSR",
  sand: "Sand",
};

/**
 * 盘上此刻装着哪一档。任一状态读不到时按「未知」处理（`null`），不猜——猜成「原生」会让
 * 装着补丁的机器看到一句「Cursor 是原版」。两条都装着在 Rust 侧是被拒绝的形态，真出现了
 * 优先报 CRSR（它是后装的那条，先卸它）。
 */
export function installedMode(c: CrsrStatus | null, s: SandStatus | null): PanelMode | null {
  if (!c || !s) return null;
  if (c.installed) return "crsr";
  if (s.installed) return "sand";
  return "native";
}

export function CursorPanelPage({ route, onGo }: { route: Route; onGo: (r: Route) => void }) {
  const [crsrStatus, setCrsrStatus] = useState<CrsrStatus | null>(null);
  const [sandStatus, setSandStatus] = useState<SandStatus | null>(null);
  const [error, setError] = useState<unknown>(null);

  const reload = useCallback(async () => {
    setError(null);
    const results = await Promise.allSettled([
      crsr.status().then(setCrsrStatus),
      sand.status().then(setSandStatus),
    ]);
    for (const result of results) {
      if (result.status === "rejected") {
        setError(result.reason);
        break;
      }
    }
  }, []);

  useEffect(() => {
    void reload();
  }, [reload]);

  // 子页面装 / 卸之后盘上状态变了，选择器上的「盘上」标签要跟着走。子页面自己有刷新，这里只在
  // 档位切换时重读一次——切换本身是用户刚做完一件事回来的时刻。
  useEffect(() => {
    void reload();
  }, [route.mode, reload]);

  const installed = installedMode(crsrStatus, sandStatus);
  const mode: PanelMode | null = route.mode ?? installed;

  return (
    <div>
      <div className="page-head">
        <div className="page-title-line">
          <h1>Cursor 面板</h1>
          <span className="page-title-meta">Agent 面板由谁付账</span>
        </div>
        <button type="button" className="btn btn-sm btn-icon btn-soft" onClick={() => void reload()} title="刷新" aria-label="刷新">
          <Icon name="refresh" size={13} />
        </button>
      </div>

      <ErrorNote error={error} onRetry={() => void reload()} />

      {mode === null ? (
        <div className="skeleton" style={{ height: 160 }} />
      ) : (
        <>
          <div className="opts">
            {MODES.map((m) => {
              const active = mode === m.id;
              const onDisk = installed === m.id;
              return (
                <Opt key={m.id} icon={m.icon} title={m.label} desc={m.desc} tone={active ? "on" : undefined}>
                  {onDisk ? <Tag tone="ok">盘上</Tag> : null}
                  <button
                    type="button"
                    className={active ? "btn btn-sm btn-primary" : "btn btn-sm"}
                    aria-pressed={active}
                    onClick={() => onGo(go("panel", { mode: m.id }))}
                  >
                    {active ? "查看中" : "查看"}
                  </button>
                </Opt>
              );
            })}
          </div>

          <div style={{ marginTop: 20 }}>
            {mode === "native" ? (
              <NativeView installed={installed} onGo={onGo} />
            ) : mode === "crsr" ? (
              <CrsrPage embedded onGo={onGo} />
            ) : (
              <SandPage embedded onGo={onGo} />
            )}
          </div>
        </>
      )}
    </div>
  );
}

/** 「原生」这一档没有东西可装；它回答的是「现在是不是原生」，不是的话指路去卸。 */
function NativeView({ installed, onGo }: { installed: PanelMode | null; onGo: (r: Route) => void }) {
  if (installed === "native") {
    return (
      <Banner
        tone="ok"
        title="Cursor 是原版：Agent 面板用 IDE 里登着的号，按它自己的额度扣。"
        hint="要换一个号付账，去「切号」把另一个号写进 IDE；要让面板用别的额度，选上面的 CRSR 或 Sand。"
        action={
          <button type="button" className="btn btn-sm" onClick={() => onGo(go("switcher"))}>
            去切号
          </button>
        }
      />
    );
  }
  if (installed === null) {
    return <Banner tone="default" title="读不到 Cursor 的补丁状态。" hint="确认 Cursor 已安装，或在「设置 → 高级」里指定安装目录。" />;
  }
  return (
    <Banner
      tone="warn"
      title={`盘上装着 ${MODE_LABEL[installed]} 补丁，Cursor 现在不是原版。`}
      hint={`要回到原生，到 ${MODE_LABEL[installed]} 那一档点「卸载」，会按备份逐字节写回。`}
      action={
        <button type="button" className="btn btn-sm btn-primary" onClick={() => onGo(go("panel", { mode: installed }))}>
          去 {MODE_LABEL[installed]} 卸载
        </button>
      }
    />
  );
}
