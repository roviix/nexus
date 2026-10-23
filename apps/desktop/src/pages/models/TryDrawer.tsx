/**
 * 「试一下」抽屉：选一个模型、说一句话，经网关流式回字。
 *
 * 它验证的是客户端会走的那条路——地址、钥匙、模型映射、接力——所以要求网关正在运行；
 * 条件不齐就明说并给一个去处理的门。
 */
import { useEffect, useMemo, useRef, useState } from "react";
import { go, type Route } from "../../shell/nav";
import { ShellIcon } from "../../shell/ShellIcon";
import { TryResult } from "../../relay/TryResult";
import { useTryRun } from "../../relay/useTryRun";
import { Banner, Drawer, Icon } from "../../ui/primitives";
import { gateway as gatewayApi } from "../../ipc/api";

const DEFAULT_PROMPT = "用一句话介绍你自己，并说出你是哪个模型。";

function ModelDropdown({
  value,
  options,
  disabled,
  onChange,
}: {
  value: string;
  options: string[];
  disabled?: boolean;
  onChange: (val: string) => void;
}) {
  const [open, setOpen] = useState(false);
  const [query, setQuery] = useState("");
  const containerRef = useRef<HTMLDivElement>(null);
  const searchInputRef = useRef<HTMLInputElement>(null);

  useEffect(() => {
    if (!open) return;
    const handleOutside = (e: MouseEvent) => {
      if (containerRef.current && !containerRef.current.contains(e.target as Node)) {
        setOpen(false);
      }
    };
    document.addEventListener("mousedown", handleOutside);
    return () => document.removeEventListener("mousedown", handleOutside);
  }, [open]);

  useEffect(() => {
    if (open) {
      setQuery("");
      setTimeout(() => searchInputRef.current?.focus(), 50);
    }
  }, [open]);

  const onKeyDown = (e: React.KeyboardEvent) => {
    if (e.key === "Escape") {
      setOpen(false);
    }
  };

  const allOpts = useMemo(() => {
    if (!options.includes(value)) return [value, ...options];
    return options;
  }, [options, value]);

  const filtered = useMemo(() => {
    const q = query.trim().toLowerCase();
    if (!q) return allOpts;
    return allOpts.filter((id) => id.toLowerCase().includes(q));
  }, [allOpts, query]);

  const channelOf = (id: string) => {
    const parts = id.split("/");
    return parts.length > 1 ? parts[0] : "local";
  };

  const nameOf = (id: string) => {
    const parts = id.split("/");
    return parts.length > 1 ? parts.slice(1).join("/") : id;
  };

  const curChannel = channelOf(value);
  const curName = nameOf(value);

  return (
    <div className="try-model-picker-wrap" ref={containerRef} onKeyDown={onKeyDown}>
      <button
        type="button"
        className={`try-model-trigger${open ? " is-open" : ""}`}
        onClick={() => !disabled && setOpen((v) => !v)}
        disabled={disabled}
      >
        <div className="try-model-trigger-left">
          <span className="try-model-prefix-tag">{curChannel}</span>
          <span className="try-model-trigger-name mono">{curName}</span>
        </div>
        <Icon
          name="chevron"
          size={12}
          className={`try-model-chevron${open ? " is-up" : ""}`}
        />
      </button>

      {open && (
        <div className="try-model-popover">
          <div className="try-model-search-bar">
            <Icon name="search" size={13} className="try-model-search-ico" />
            <input
              ref={searchInputRef}
              type="text"
              className="try-model-search-input"
              placeholder="搜索模型名称或通道前缀..."
              value={query}
              onChange={(e) => setQuery(e.target.value)}
            />
            {query ? (
              <button
                type="button"
                className="try-model-clear-btn"
                onClick={() => setQuery("")}
              >
                <Icon name="x" size={11} />
              </button>
            ) : null}
          </div>

          <div className="try-model-list">
            {filtered.length === 0 ? (
              <div className="try-model-empty">无匹配模型</div>
            ) : (
              filtered.map((id) => {
                const ch = channelOf(id);
                const n = nameOf(id);
                const isSelected = id === value;
                return (
                  <div
                    key={id}
                    className={`try-model-item${isSelected ? " is-selected" : ""}`}
                    onClick={() => {
                      onChange(id);
                      setOpen(false);
                    }}
                  >
                    <span className="try-model-prefix-tag">{ch}</span>
                    <span className="try-model-item-name mono truncate">{n}</span>
                    {isSelected && (
                      <Icon name="check" size={13} className="try-model-check-icon" />
                    )}
                  </div>
                );
              })
            )}
          </div>
        </div>
      )}
    </div>
  );
}

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
  const [startingGateway, setStartingGateway] = useState(false);
  const { run, busy, start, stats } = useTryRun();

  async function handleStartGateway() {
    setStartingGateway(true);
    try {
      await gatewayApi.start();
      lane.ready = true;
    } catch {} finally {
      setStartingGateway(false);
    }
  }

  const canStart = (lane.ready || startingGateway) && !busy && prompt.trim() !== "" && model.trim() !== "";

  const head = (
    <div className="try-drawer-head">
      <div className="try-head-icon">
        <ShellIcon name="flask" size={16} />
      </div>
      <div>
        <h2 className="try-head-title">试一下 · 模型实时探测</h2>
        <span className="try-head-sub">直连本地网关，验证流式回字与首字延迟</span>
      </div>
    </div>
  );

  const footer = (
    <div className="try-drawer-footer">
      <button
        type="button"
        className="btn btn-sm btn-soft try-playground-btn"
        onClick={() => onGo(go("playground", { view: "chat", model }))}
        title="在游乐场中进行多轮对话与参数微调"
      >
        <ShellIcon name="flask" size={13} />
        <span>游乐场多轮调试</span>
      </button>

      <button
        type="button"
        className="btn btn-sm btn-primary try-submit-btn"
        disabled={!canStart}
        onClick={async () => {
          if (!lane.ready) {
            await handleStartGateway();
          }
          void start(model, prompt);
        }}
      >
        <ShellIcon name="play" size={13} />
        <span>{busy ? "正在探测…" : !lane.ready ? "一键开启网关并探测" : "发送探测"}</span>
      </button>
    </div>
  );

  return (
    <Drawer
      label="试一下"
      onClose={onClose}
      head={head}
      footer={footer}
    >
      {!lane.ready ? (
        <Banner
          tone="warn"
          title="本地网关未开启"
          hint="试用需经过本地网关 (127.0.0.1)。点击下方按钮可一键开启网关并直接发送测试。"
          action={
            <button
              type="button"
              className="btn btn-sm btn-primary"
              disabled={startingGateway}
              onClick={handleStartGateway}
            >
              {startingGateway ? "启动中…" : "一键开启网关"}
            </button>
          }
        />
      ) : null}

      <div className="try-field-group" style={{ marginTop: lane.ready ? 0 : 14 }}>
        <div className="try-field-label-row">
          <label className="try-field-label">探测目标模型</label>
        </div>
        <ModelDropdown
          value={model}
          options={lane.modelIds}
          disabled={busy}
          onChange={setModel}
        />
      </div>

      <div className="try-field-group" style={{ marginTop: 14 }}>
        <div className="try-field-label-row">
          <label className="try-field-label">输入测试内容</label>
          <button
            type="button"
            className="try-preset-chip"
            onClick={() => setPrompt(DEFAULT_PROMPT)}
            title="重置为默认自我介绍提示词"
          >
            重置默认
          </button>
        </div>
        <div className="try-prompt-card">
          <textarea
            className="try-prompt-textarea"
            rows={3}
            value={prompt}
            disabled={busy}
            placeholder="输入测试问题或指令…"
            onChange={(e) => setPrompt(e.target.value)}
            onKeyDown={(e) => {
              if ((e.metaKey || e.ctrlKey) && e.key === "Enter" && canStart) {
                if (!lane.ready) void handleStartGateway();
                void start(model, prompt);
              }
            }}
          />
          <div className="try-prompt-card-foot">
            <span className="try-shortcut-pill mono">
              <kbd>⌘ / Ctrl + ↵</kbd> 发送
            </span>
            {prompt ? (
              <span className="try-prompt-len mono faint">{prompt.length} 字符</span>
            ) : null}
          </div>
        </div>
      </div>

      {run ? (
        <div className="try-result-section" style={{ marginTop: 16 }}>
          <TryResult run={run} stats={stats} busy={busy} />
        </div>
      ) : null}
    </Drawer>
  );
}
