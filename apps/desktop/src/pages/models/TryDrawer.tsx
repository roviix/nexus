/**
 * 「试一下」抽屉：选一个模型、说一句话，经网关流式回字。
 *
 * 它验证的是客户端会走的那条路——地址、钥匙、模型映射、接力——所以要求网关正在运行；
 * 条件不齐就明说并给一个去处理的门。
 */
import { useState } from "react";
import { go, type Route } from "../../shell/nav";
import { ShellIcon } from "../../shell/ShellIcon";
import { TryResult } from "../../relay/TryResult";
import { useTryRun } from "../../relay/useTryRun";
import { Banner, Drawer } from "../../ui/primitives";

const DEFAULT_PROMPT = "用一句话介绍你自己，并说出你是哪个模型。";

export interface TryLane {
  /** 现在能不能试。 */
  ready: boolean;
  /** 不能试时说为什么、去哪修。 */
  blocker?: { text: string; hint: string; fix: () => void; fixLabel: string };
  /** 网关认的模型 id。 */
  modelIds: string[];
  /** 脚注：地址 / 当前号 / 通道。 */
  foot: string;
}

export function TryDrawer({
  lane,
  initialModel,
  onClose,
  onGo,
}: {
  lane: TryLane;
  initialModel: string;
  onClose: () => void;
  onGo: (r: Route) => void;
}) {
  const [model, setModel] = useState(initialModel);
  const [prompt, setPrompt] = useState(DEFAULT_PROMPT);
  const { run, busy, start, stats } = useTryRun();

  const canStart = lane.ready && !busy && prompt.trim() !== "" && model.trim() !== "";

  return (
    <Drawer
      label="试一下"
      onClose={onClose}
      head={
        <h2 style={{ margin: 0, fontSize: 17 }}>试一下</h2>
      }
      footer={
        <div className="row-between" style={{ width: "100%" }}>
          <span className="muted truncate" style={{ fontSize: 12 }}>
            {lane.foot}
          </span>
          <button type="button" className="btn btn-primary" disabled={!canStart} onClick={() => void start(model, prompt)}>
            <ShellIcon name="play" size={13} />
            {busy ? "生成中…" : "发送"}
          </button>
        </div>
      }
    >
      {!lane.ready && lane.blocker ? (
        <Banner
          tone="warn"
          title={lane.blocker.text}
          hint={lane.blocker.hint}
          action={
            <button type="button" className="btn btn-sm" onClick={lane.blocker.fix}>
              {lane.blocker.fixLabel}
            </button>
          }
        />
      ) : null}

      <label className="field" style={{ marginTop: lane.ready ? 0 : 14 }}>
        <span className="muted" style={{ fontSize: 12 }}>
          模型
        </span>
        <select className="select mono" value={model} onChange={(e) => setModel(e.target.value)} disabled={busy}>
          {lane.modelIds.map((id) => (
            <option key={id} value={id}>
              {id}
            </option>
          ))}
          {!lane.modelIds.includes(model) ? <option value={model}>{model}</option> : null}
        </select>
      </label>

      <label className="field" style={{ marginTop: 12 }}>
        <span className="muted" style={{ fontSize: 12 }}>
          说点什么
        </span>
        <textarea
          className="textarea fw-try-prompt"
          rows={3}
          value={prompt}
          disabled={busy}
          onChange={(e) => setPrompt(e.target.value)}
          onKeyDown={(e) => {
            if ((e.metaKey || e.ctrlKey) && e.key === "Enter" && canStart) void start(model, prompt);
          }}
        />
        <span className="muted" style={{ fontSize: 11.5 }}>
          ⌘ / Ctrl + Enter 发送
        </span>
      </label>

      {run ? (
        <div style={{ marginTop: 16 }}>
          <TryResult run={run} stats={stats} busy={busy} />
        </div>
      ) : null}

      <div className="row" style={{ marginTop: 16, justifyContent: "flex-end", gap: 8 }}>
        {/* 这里只是一问一答的通不通；想多聊几轮、留下记录，去游乐场。 */}
        <button type="button" className="btn btn-sm btn-soft" onClick={() => onGo(go("playground", { view: "chat", model }))}>
          <ShellIcon name="flask" size={12} />
          到游乐场继续
        </button>
        <button type="button" className="btn btn-sm btn-soft" onClick={() => onGo(go("connect", { model }))}>
          用它接入
          <ShellIcon name="arrow" size={12} />
        </button>
      </div>
    </Drawer>
  );
}
