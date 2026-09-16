/**
 * 批量查找：粘一份邮箱清单，把它们在库里找出来。
 *
 * 弹窗只做一件事 —— 收清单。数几个邮箱实时显示在按钮上，让人在按下前就知道粘对了没有；
 * 找到之后怎么呈现（列表只剩这几个、哪些没找到）是账号页那条结果栏的事。
 */

import { useEffect, useMemo, useRef, useState } from "react";
import { parseLookup } from "../../accounts/lookup";
import { Icon, Modal } from "../../ui/primitives";

export function LookupModal({
  initialText = "",
  onClose,
  onLookup,
}: {
  /** 从搜索框粘过来的那段文本，直接填进去。 */
  initialText?: string;
  onClose: () => void;
  onLookup: (emails: string[], refresh: boolean) => void;
}) {
  const [text, setText] = useState(initialText);
  const [refresh, setRefresh] = useState(false);
  const emails = useMemo(() => parseLookup(text), [text]);
  const ref = useRef<HTMLTextAreaElement>(null);

  useEffect(() => {
    ref.current?.focus();
  }, []);

  function submit() {
    if (emails.length === 0) return;
    onLookup(emails, refresh);
  }

  return (
    <Modal
      compact
      title="批量查找"
      subtitle="粘一份邮箱清单，列表只留这几个号。一行一个、逗号隔开、邮箱----密码 那种清单都行，只认邮箱。"
      onClose={onClose}
      footer={
        <>
          <label className="lookup-refresh">
            <input type="checkbox" checked={refresh} onChange={(e) => setRefresh(e.target.checked)} />
            <span>找到后顺手刷新用量</span>
          </label>
          <span className="grow" />
          <button type="button" className="btn" onClick={onClose}>
            取消
          </button>
          <button type="button" className="btn btn-primary" disabled={emails.length === 0} onClick={submit}>
            <Icon name="search" size={13} />
            {emails.length > 0 ? `查找 ${emails.length} 个` : "查找"}
          </button>
        </>
      }
    >
      <textarea
        ref={ref}
        className="input lookup-text mono"
        value={text}
        onChange={(e) => setText(e.target.value)}
        onKeyDown={(e) => {
          // ⌘/Ctrl + Enter 提交：清单里本来就要回车换行，光 Enter 不能抢。
          if (e.key === "Enter" && (e.metaKey || e.ctrlKey)) {
            e.preventDefault();
            submit();
          }
        }}
        placeholder={"a@example.com\nb@example.com----P@ssw0rd\nc@example.com"}
        spellCheck={false}
        rows={8}
      />
    </Modal>
  );
}
