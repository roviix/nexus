/**
 * 看大图。整屏压暗，图占左边，右边一栏是它的来历：提示词、模型、规格、体积、时间、出自哪个会话，
 * 底下一排动作。左右键 / 两侧箭头翻同一批里的上一张下一张，Esc 关。
 *
 * 消息流里点图和资产页点图用的是同一个组件——同一张图在两处看到的信息与能做的事该完全一样。
 */
import { useEffect, useState } from "react";
import { imageSrc } from "../../ipc/playground";
import { shortDateTime } from "../../ui/usage";
import { Icon, Spinner, useEscapeToClose } from "../../ui/primitives";
import { PgIcon } from "./PgIcon";
import { fmtBytes } from "./target";

export interface LightboxItem {
  id: string;
  /** 缺省按图片；`video/*` 用 `<video>` 放。 */
  mime?: string;
  prompt: string | null;
  model: string | null;
  width: number | null;
  height: number | null;
  size: string | null;
  bytes: number;
  createdAt: string;
  threadId?: string;
  threadTitle?: string;
}

export function Lightbox({
  items,
  index,
  onIndex,
  onClose,
  onOpenThread,
  onReveal,
  onDelete,
}: {
  items: LightboxItem[];
  index: number;
  onIndex: (i: number) => void;
  onClose: () => void;
  /** 给了才显示「打开会话」（消息流里本来就在那个会话，不给）。 */
  onOpenThread?: (threadId: string) => void;
  onReveal: (id: string) => void;
  /** 删掉这一张。resolve 后由调用方决定列表怎么变；这里只负责问一句「确定？」。 */
  onDelete?: (id: string) => Promise<void>;
}) {
  useEscapeToClose(onClose);
  const item = items[index];
  const [armed, setArmed] = useState(false);
  const [busy, setBusy] = useState(false);
  const [copied, setCopied] = useState(false);

  useEffect(() => {
    setArmed(false);
  }, [index]);

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "ArrowLeft" && index > 0) onIndex(index - 1);
      if (e.key === "ArrowRight" && index < items.length - 1) onIndex(index + 1);
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [index, items.length, onIndex]);

  if (!item) return null;

  const dims = item.width && item.height ? `${item.width} × ${item.height}` : item.size?.replace("x", " × ") ?? "—";
  const at = Date.parse(item.createdAt);

  function copyPrompt() {
    if (!item?.prompt) return;
    void navigator.clipboard.writeText(item.prompt).catch(() => {});
    setCopied(true);
    window.setTimeout(() => setCopied(false), 1400);
  }

  return (
    <div
      className="pg-light"
      role="dialog"
      aria-modal="true"
      aria-label="查看图片"
      onMouseDown={(e) => {
        if (e.target === e.currentTarget) onClose();
      }}
    >
      <div className="pg-light-stage" onMouseDown={(e) => e.target === e.currentTarget && onClose()}>
        {index > 0 ? (
          <button type="button" className="pg-light-nav is-prev" aria-label="上一张" onClick={() => onIndex(index - 1)}>
            <PgIcon name="prev" size={18} />
          </button>
        ) : null}
        {item.mime?.startsWith("video/") ? (
          <video key={item.id} className="pg-light-img" src={imageSrc(item.id)} controls autoPlay loop playsInline />
        ) : (
          <img key={item.id} className="pg-light-img" src={imageSrc(item.id)} alt={item.prompt ?? ""} />
        )}
        {index < items.length - 1 ? (
          <button type="button" className="pg-light-nav is-next" aria-label="下一张" onClick={() => onIndex(index + 1)}>
            <PgIcon name="next" size={18} />
          </button>
        ) : null}
        {items.length > 1 ? (
          <span className="pg-light-count num">
            {index + 1} / {items.length}
          </span>
        ) : null}
      </div>

      <aside className="pg-light-side">
        <div className="pg-light-side-head">
          <span className="eyebrow" style={{ margin: 0 }}>
            提示词
          </span>
          <button type="button" className="btn btn-icon btn-quiet" onClick={onClose} aria-label="关闭">
            <Icon name="close" />
          </button>
        </div>
        <p className="pg-light-prompt selectable">{item.prompt || <span className="faint">（没有记下提示词）</span>}</p>

        <dl className="pg-light-meta">
          <dt>模型</dt>
          <dd className="mono truncate">{item.model ?? "—"}</dd>
          <dt>尺寸</dt>
          <dd className="mono">{dims}</dd>
          <dt>体积</dt>
          <dd className="mono">{fmtBytes(item.bytes)}</dd>
          <dt>时间</dt>
          <dd className="mono">{Number.isFinite(at) ? shortDateTime(at) : "—"}</dd>
          {item.threadTitle ? (
            <>
              <dt>会话</dt>
              <dd className="truncate">{item.threadTitle}</dd>
            </>
          ) : null}
        </dl>

        <div className="pg-light-acts">
          {onOpenThread && item.threadId ? (
            <button type="button" className="btn btn-block" onClick={() => onOpenThread(item.threadId!)}>
              <PgIcon name="image" size={14} />
              打开会话
            </button>
          ) : null}
          <div className="row" style={{ gap: 6 }}>
            <button type="button" className="btn btn-sm grow" onClick={() => onReveal(item.id)}>
              <PgIcon name="folder" size={13} />
              在文件夹中显示
            </button>
            <button type="button" className="btn btn-sm grow" disabled={!item.prompt} onClick={copyPrompt}>
              <Icon name={copied ? "check" : "copy"} size={13} />
              {copied ? "已复制" : "复制提示词"}
            </button>
          </div>
          {onDelete ? (
            armed ? (
              <div className="confirm">
                <span className="confirm-text">文件也会一起删掉，不可恢复。</span>
                <button type="button" className="btn btn-sm" disabled={busy} onClick={() => setArmed(false)}>
                  取消
                </button>
                <button
                  type="button"
                  className="btn btn-sm btn-danger is-armed"
                  disabled={busy}
                  onClick={() =>
                    void (async () => {
                      setBusy(true);
                      try {
                        await onDelete(item.id);
                      } finally {
                        setBusy(false);
                        setArmed(false);
                      }
                    })()
                  }
                >
                  {busy ? <Spinner /> : "确认删除"}
                </button>
              </div>
            ) : (
              <button type="button" className="btn btn-sm btn-quiet btn-danger" onClick={() => setArmed(true)}>
                <Icon name="trash" size={13} />
                删除这张图
              </button>
            )
          ) : null}
        </div>
      </aside>
    </div>
  );
}
