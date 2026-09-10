/**
 * 左栏：这一类的会话列表（新的在上），头上一个「新建」和一个搜索框。
 *
 * 种类不在这里切——对话 / 图片是侧栏里游乐场下面的两个子项，这一栏只管一种。
 * 列表行两行：标题（没有就按种类给占位）和相对时间在一行，第二行是最后一条消息的开头；
 * 图片会话左边多一枚封面缩略图 —— 一排「画只猫」「画只狗」光靠文字认不出来。
 * 删除藏在 hover 里、两步确认：列表不该常驻一排红叉。
 */
import { useMemo, useState } from "react";
import { imageSrc, type Kind, type ThreadSummary } from "../../ipc/playground";
import { timeAgo } from "../../ui/format";
import { Icon, Spinner } from "../../ui/primitives";
import { PgIcon } from "./PgIcon";
import { useRun } from "./runs";
import { plainPreview, threadTitle } from "./target";

export function ThreadRail({
  kind,
  threads,
  selectedId,
  onSelect,
  onNew,
  onDelete,
}: {
  kind: Kind;
  /** null = 还没拉回来。 */
  threads: ThreadSummary[] | null;
  selectedId: string | null;
  onSelect: (id: string) => void;
  onNew: () => void;
  onDelete: (id: string) => Promise<void>;
}) {
  const [q, setQ] = useState("");
  const shown = useMemo(() => {
    const kw = q.trim().toLowerCase();
    return (threads ?? []).filter((t) => !kw || threadTitle(t.title, t.kind).toLowerCase().includes(kw) || (t.preview ?? "").toLowerCase().includes(kw) || t.model.toLowerCase().includes(kw));
  }, [threads, q]);

  return (
    <aside className="pg-rail">
      {/* 标题行带上「新建」那枚键，搜索紧挨着列表 —— 它是列表的筛子，该贴着被筛的东西。 */}
      <div className="pg-rail-head">
        <div className="pg-rail-cap">
          <span className="eyebrow" style={{ margin: 0 }}>
            {kind === "chat" ? "对话" : "图片"}
          </span>
          {threads?.length ? <span className="pg-rail-n num">{threads.length}</span> : null}
          <span className="grow" />
          <button
            type="button"
            className={`pg-new${selectedId == null ? " is-on" : ""}`}
            onClick={onNew}
            title={kind === "chat" ? "新对话" : "新图片"}
            aria-label={kind === "chat" ? "新对话" : "新图片"}
          >
            <Icon name="plus" size={14} />
          </button>
        </div>
        <label className="pg-search">
          <Icon name="search" size={13} className="pg-search-ico" />
          <input value={q} placeholder={kind === "chat" ? "搜索对话" : "搜索图片会话"} spellCheck={false} onChange={(e) => setQ(e.target.value)} />
          {q ? (
            <button type="button" className="pg-search-x" aria-label="清空" onClick={() => setQ("")}>
              <Icon name="close" size={11} />
            </button>
          ) : null}
        </label>
      </div>

      <div className="pg-rail-list" role="listbox" aria-label={kind === "chat" ? "对话列表" : "图片会话列表"}>
        {threads == null ? (
          <>
            <div className="skeleton pg-row-skeleton" />
            <div className="skeleton pg-row-skeleton" />
            <div className="skeleton pg-row-skeleton" />
          </>
        ) : shown.length === 0 ? (
          <p className="pg-rail-empty">{q ? "没有匹配的会话" : kind === "chat" ? "还没有对话，右边说一句就开始了。" : "还没有出过图，右边描述一下画面。"}</p>
        ) : (
          shown.map((t) => <Row key={t.id} t={t} on={t.id === selectedId} onSelect={() => onSelect(t.id)} onDelete={() => onDelete(t.id)} />)
        )}
      </div>
    </aside>
  );
}

function Row({ t, on, onSelect, onDelete }: { t: ThreadSummary; on: boolean; onSelect: () => void; onDelete: () => Promise<void> }) {
  const [armed, setArmed] = useState(false);
  const [busy, setBusy] = useState(false);
  const run = useRun(t.id);
  const title = threadTitle(t.title, t.kind);
  const live = run && !run.done;
  const sub = live ? (t.kind === "chat" ? "生成中…" : t.kind === "video" ? "生成视频中…" : "出图中…") : t.kind === "chat" ? (t.preview ? plainPreview(t.preview) : `${t.messageCount} 条`) : t.model;

  return (
    <div className={`pg-row${on ? " is-on" : ""}`} role="option" aria-selected={on}>
      <button type="button" className="pg-row-main" onClick={onSelect} title={title} aria-label={title}>
        {t.kind === "image" ? (
          <span className="pg-row-cover">{t.coverImageId ? <img src={imageSrc(t.coverImageId)} alt="" loading="lazy" decoding="async" /> : <PgIcon name="image" size={14} />}</span>
        ) : t.kind === "video" ? (
          <span className="pg-row-cover">{t.coverImageId ? <video src={imageSrc(t.coverImageId)} muted playsInline preload="metadata" /> : <PgIcon name="play" size={14} />}</span>
        ) : null}
        <span className="pg-row-text">
          <span className="pg-row-line">
            <span className="pg-row-title truncate">{title}</span>
            <span className="pg-row-time num">{timeAgo(t.updatedAt)}</span>
          </span>
          <span className={`pg-row-sub truncate${live ? " is-live" : ""}`}>{sub}</span>
        </span>
      </button>
      {armed ? (
        <span className="pg-row-confirm">
          <button type="button" className="btn btn-sm btn-quiet" disabled={busy} onClick={() => setArmed(false)}>
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
                  await onDelete();
                } finally {
                  setBusy(false);
                  setArmed(false);
                }
              })()
            }
          >
            {busy ? <Spinner /> : "删除"}
          </button>
        </span>
      ) : (
        <button type="button" className="ibtn pg-row-del" aria-label="删除这个会话" onClick={() => setArmed(true)}>
          <Icon name="trash" size={13} />
        </button>
      )}
    </div>
  );
}
