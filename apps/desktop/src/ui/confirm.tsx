/**
 * 应用内的确认框，替代 `window.confirm`。
 *
 * **为什么不能用 `window.confirm`**：Tauri 在 macOS 上用的 WKWebView 没挂 JS 对话框的
 * `WKUIDelegate` 回调（wry 没实现），`confirm()` 不弹窗、直接返回 `false`。任何
 * `if (!window.confirm(...)) return;` 在 mac 上就是一个静默的 no-op ——「移出切号池」点了
 * 没反应就是这么来的。Windows 的 WebView2 会弹系统框，所以这个坑只在 mac 上现形。
 *
 * 用法和 `window.confirm` 一样，只是异步的：
 *
 * ```ts
 * if (!(await confirm("把它移出切号池？"))) return;
 * ```
 *
 * `<ConfirmHost />` 挂在 `App` 根部一次；`confirm()` 往它那儿排队，一次只显示一个。
 * 没挂 host（比如单测、预览脚手架里漏挂了）时直接回 `false` —— 和「用户点了取消」同义，
 * 宁可不做也别在没人确认的情况下把事做了。
 */
import { useEffect, useState, type ReactNode } from "react";
import { Modal } from "./primitives";

export interface ConfirmOptions {
  /** 标题。不传就用一句通用的「请确认」。 */
  title?: ReactNode;
  /** 确认键文案。默认「确认」。 */
  okLabel?: string;
  /** 取消键文案。默认「取消」。 */
  cancelLabel?: string;
  /** 确认的是件有破坏性的事（删除 / 覆盖 / 打断别人）时把确认键标红。 */
  danger?: boolean;
}

interface Pending {
  message: ReactNode;
  options: ConfirmOptions;
  resolve: (ok: boolean) => void;
}

/** 当前挂着的 host 收请求的口。没挂时是 null。 */
let enqueue: ((p: Pending) => void) | null = null;

/** 问用户一个是 / 否的问题。resolve 成 `true` 表示点了确认。 */
export function confirm(message: ReactNode, options: ConfirmOptions = {}): Promise<boolean> {
  if (!enqueue) return Promise.resolve(false);
  const push = enqueue;
  return new Promise((resolve) => push({ message, options, resolve }));
}

/**
 * 只在应用根部挂一个。第二个实例会接管收请求的口 —— 这里不做「只许一个」的断言，
 * 因为预览脚手架会按页单独渲染。
 */
export function ConfirmHost() {
  const [queue, setQueue] = useState<Pending[]>([]);

  useEffect(() => {
    enqueue = (p) => setQueue((q) => [...q, p]);
    return () => {
      enqueue = null;
    };
  }, []);

  const current = queue[0];
  if (!current) return null;

  const settle = (ok: boolean) => {
    current.resolve(ok);
    setQueue((q) => q.slice(1));
  };

  const { title, okLabel, cancelLabel, danger } = current.options;
  return (
    <Modal
      compact
      title={title ?? "请确认"}
      onClose={() => settle(false)}
      footer={
        <>
          <button type="button" className="btn" onClick={() => settle(false)}>
            {cancelLabel ?? "取消"}
          </button>
          <button type="button" className={`btn ${danger ? "btn-danger is-armed" : "btn-primary"}`} autoFocus onClick={() => settle(true)}>
            {okLabel ?? "确认"}
          </button>
        </>
      }
    >
      <p className="confirm-message">{current.message}</p>
    </Modal>
  );
}
