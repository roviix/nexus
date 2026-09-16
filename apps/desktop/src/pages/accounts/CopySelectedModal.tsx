/**
 * 多选 → 复制：选一种凭证格式，再勾要附带的说明。
 *
 * 两组选项分两层：格式是必选的一项（单选），说明是可以不要的几项（多选）。预览用的是选中的
 * 第一个号的**真实用量**、假的凭证 —— 让人在按下复制前看到自己会拿到什么形状的东西，
 * 又不把明文密码摆在弹窗里。
 */

import { useMemo, useState } from "react";
import {
  COPY_EXTRAS,
  COPY_FORMATS,
  copyInfoLine,
  type CopyChoice,
  type CopyExtra,
  type CopyFormat,
} from "../../accounts/copy";
import type { Account } from "../../ipc/types";
import { maskEmail } from "../../ui/format";
import { Icon, Modal } from "../../ui/primitives";

export function CopySelectedModal({
  accounts,
  initial,
  busy,
  masked,
  onClose,
  onCopy,
}: {
  /** 选中的号，按列表顺序。预览拿第一个。 */
  accounts: Account[];
  initial: CopyChoice;
  busy: boolean;
  /** 页头小眼睛开着时，预览里的邮箱也打码。 */
  masked: boolean;
  onClose: () => void;
  onCopy: (choice: CopyChoice) => void;
}) {
  const [format, setFormat] = useState<CopyFormat>(initial.format);
  const [extras, setExtras] = useState<Set<CopyExtra>>(() => new Set(initial.extras));

  const chosenExtras = useMemo(() => COPY_EXTRAS.map((e) => e.id).filter((id) => extras.has(id)), [extras]);
  const sample = accounts[0];
  const preview = useMemo(() => {
    if (!sample) return "";
    const email = masked ? maskEmail(sample.email) : sample.email;
    const info = copyInfoLine(sample, chosenExtras);
    if (format === "json") {
      const entry: Record<string, string> = { email, refreshToken: "…" };
      if (info) entry.info = info;
      return JSON.stringify({ accounts: [entry] }, null, 2);
    }
    const tail =
      format === "email_password"
        ? "----••••••••"
        : format === "email_refresh" || format === "email_session"
          ? "----eyJhbGci…"
          : "";
    return info ? `${email}${tail}\n${info}` : `${email}${tail}`;
  }, [sample, masked, format, chosenExtras]);

  function toggle(id: CopyExtra) {
    setExtras((prev) => {
      const next = new Set(prev);
      if (next.has(id)) next.delete(id);
      else next.add(id);
      return next;
    });
  }

  return (
    <Modal
      compact
      title={`复制 ${accounts.length} 个账号`}
      subtitle="第一行是凭证，给脚本用；勾了说明就另起一行跟在后面。"
      onClose={onClose}
      footer={
        <>
          <button type="button" className="btn" onClick={onClose} disabled={busy}>
            取消
          </button>
          <button
            type="button"
            className="btn btn-primary"
            disabled={busy || accounts.length === 0}
            onClick={() => onCopy({ format, extras: chosenExtras })}
          >
            <Icon name="copy" size={13} />
            复制
          </button>
        </>
      }
    >
      <div className="copy-sect">
        <div className="copy-sect-k">格式</div>
        <div className="choice-list" role="radiogroup" aria-label="复制格式">
          {COPY_FORMATS.map((f) => {
            const on = f.id === format;
            return (
              <button
                key={f.id}
                type="button"
                role="radio"
                aria-checked={on}
                className={`choice${on ? " is-on" : ""}`}
                onClick={() => setFormat(f.id)}
              >
                <span className="choice-dot" aria-hidden />
                <span className="choice-copy">
                  <span className="choice-title">{f.label}</span>
                  <span className="choice-sample mono">{f.sample}</span>
                </span>
              </button>
            );
          })}
        </div>
      </div>

      <div className="copy-sect">
        <div className="copy-sect-k">
          附带说明
          <span className="faint"> · 可不选</span>
        </div>
        <div className="chips" role="group" aria-label="附带说明">
          {COPY_EXTRAS.map((e) => {
            const on = extras.has(e.id);
            return (
              <button
                key={e.id}
                type="button"
                className={`chip${on ? " is-on" : ""}`}
                aria-pressed={on}
                title={e.hint}
                onClick={() => toggle(e.id)}
              >
                {on ? <Icon name="check" size={11} /> : null}
                {e.label}
              </button>
            );
          })}
        </div>
      </div>

      {preview ? (
        <div className="copy-sect">
          <div className="copy-sect-k">预览</div>
          <pre className="copy-preview mono">{preview}</pre>
        </div>
      ) : null}
    </Modal>
  );
}
