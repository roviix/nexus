/**
 * 右侧的 artifact 预览区：对话里的卡片点开之后，效果与原始代码都在这里看。
 *
 * 它不是弹层，是对话旁边并排的一根柱子 —— 对话留在原地，点另一张卡片这里就换一块；
 * 读完代码关掉，眼睛还落在刚才那条消息上。头部两行：第一行回答「这是哪一块」
 * （图标 + 标题 + 第几块 / 共几块），第二行是视角开关（预览 / 代码）与复制。
 */
import { useEffect, useRef, useState } from "react";
import { CopyButton, Icon, Spinner, useEscapeToClose } from "../../ui/primitives";
import { artifactKindLabel, CodeBody, usePreviewUrl } from "./Markdown";
import { PgIcon } from "./PgIcon";
import { fmtBytes } from "./target";
import type { Artifact } from "./artifacts";

export function ArtifactPanel({
  artifact,
  index,
  total,
  onJump,
  onClose,
}: {
  artifact: Artifact;
  /** 在整条会话的 artifact 里排第几（0 起）。 */
  index: number;
  total: number;
  onJump: (delta: -1 | 1) => void;
  onClose: () => void;
}) {
  useEscapeToClose(onClose);

  const [tab, setTab] = useState<"preview" | "code">(artifact.open ? "code" : "preview");
  /** 用户亲手切过 tab 之后，流式收尾那一刻不再替他切 —— 他正在看的就是他要的。 */
  const touched = useRef(false);
  const refKey = `${artifact.ref.messageId}:${artifact.ref.codeIndex}`;

  // 换了一块：回到默认视角（写完的看效果，没写完的看代码）。
  // 只在「换了一块」这个时刻重置；open 的后续翻转交给下面那个 effect。
  useEffect(() => {
    touched.current = false;
    setTab(artifact.open ? "code" : "preview");
  }, [refKey]);

  // 流式写完的那一刻正是「能看了」的时刻：没碰过 tab 的话，从代码切到预览。
  useEffect(() => {
    if (!artifact.open && !touched.current) setTab("preview");
  }, [artifact.open]);

  const url = usePreviewUrl(artifact.kind, artifact.body, tab === "preview" && !artifact.open);

  // 看代码 + 还在生成：跟着写往下滚，像看日志一样。
  const codeRef = useRef<HTMLDivElement | null>(null);
  useEffect(() => {
    const el = codeRef.current;
    if (el && artifact.open) el.scrollTop = el.scrollHeight;
  }, [artifact.body, artifact.open, tab]);

  return (
    <aside className="pg-art" aria-label={`预览 ${artifact.title}`}>
      <header className="pg-art-head">
        <div className="pg-art-top">
          <span className={`pg-art-ico is-${artifact.kind}`} aria-hidden>
            <PgIcon name={artifact.kind === "svg" ? "image" : "window"} size={15} />
          </span>
          <div className="pg-art-names">
            <div className="pg-art-title truncate" title={artifact.title}>
              {artifact.title}
            </div>
            <div className="pg-art-meta num">
              {/* 标题就是类别名（没挖到 <title>）时不再重复它。 */}
              {[
                artifact.title !== artifactKindLabel(artifact.kind) ? artifactKindLabel(artifact.kind) : "",
                `${artifact.lines} 行`,
                fmtBytes(artifact.bytes),
              ]
                .filter(Boolean)
                .join(" · ")}
              {artifact.open ? (
                <>
                  {" · "}
                  <em className="pg-art-live">生成中</em>
                </>
              ) : null}
            </div>
          </div>
          {total > 1 ? (
            <span className="pg-art-nav">
              <button
                type="button"
                className="pg-act"
                disabled={index <= 0}
                onClick={() => onJump(-1)}
                aria-label="上一个 artifact"
                data-tip="上一个"
              >
                <PgIcon name="prev" size={13} />
              </button>
              <span className="pg-art-n num">
                {index + 1} / {total}
              </span>
              <button
                type="button"
                className="pg-act"
                disabled={index >= total - 1}
                onClick={() => onJump(1)}
                aria-label="下一个 artifact"
                data-tip="下一个"
              >
                <PgIcon name="next" size={13} />
              </button>
            </span>
          ) : null}
          <button
            type="button"
            className="btn btn-icon btn-quiet"
            onClick={onClose}
            aria-label="关闭预览区"
            data-tip="关闭"
          >
            <Icon name="close" />
          </button>
        </div>
        <div className="pg-art-bar">
          <div className="pg-art-tabs" role="tablist" aria-label="预览或代码">
            <button
              type="button"
              role="tab"
              aria-selected={tab === "preview"}
              className="pg-art-tab"
              title="在隔离沙箱里渲染：脚本跑得起来，但够不着这个应用"
              onClick={() => {
                touched.current = true;
                setTab("preview");
              }}
            >
              预览
            </button>
            <button
              type="button"
              role="tab"
              aria-selected={tab === "code"}
              className="pg-art-tab"
              onClick={() => {
                touched.current = true;
                setTab("code");
              }}
            >
              代码
            </button>
          </div>
          <span className="grow" />
          <CopyButton icon value={artifact.body} label="复制代码" />
        </div>
      </header>

      <div className="pg-art-body">
        {tab === "preview" ? (
          artifact.open ? (
            <div className="pg-art-waiting">
              <Spinner />
              <p>
                还在生成，写完整篇就能预览。
                <br />
                <span className="pg-art-waiting-sub">等不及可以先看代码，正跟着写往下滚。</span>
              </p>
            </div>
          ) : url ? (
            <iframe
              title={`预览 ${artifact.title}`}
              src={url}
              // 与内嵌预览同一条纪律：给脚本，不给 same-origin。
              sandbox="allow-scripts"
              className="pg-art-frame"
            />
          ) : null
        ) : (
          <div className="pg-art-code" ref={codeRef}>
            <CodeBody lang={artifact.lang} body={artifact.body} />
            {artifact.open ? <span className="pg-caret pg-art-caret">▍</span> : null}
          </div>
        )}
      </div>
    </aside>
  );
}
