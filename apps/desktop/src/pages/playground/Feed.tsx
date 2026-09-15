/**
 * 主区中间那一大块：会话里的消息，从上到下。
 *
 * 用户的话靠右一个薄荷色气泡；回复是「标 + 正文」两栏——左边一枚 Nexus 标当身份，
 * 右边从上到下是：模型名（中途换模型对比时每条都对得上号）、思考、正文（按 Markdown 排），
 * 最后一行左边成绩单（首字 / 总耗时 / token / 吞吐）、右边动作键。
 *
 * **成绩单在末尾不在头上**：那些数是读完之后才回头看的，压在正文前面等于每读一条回复
 * 都要先跨过一串跟内容无关的数字。模型名留在头上——它是身份，不是成绩。
 *
 * 图片会话里回复是一格或几格图，点开看大图。正在生成的那一轮不在 `messages` 里 ——
 * 它来自 run store，画在末尾。
 *
 * 滚动只在本来就贴着底的时候跟随：流式每帧都在改内容，无条件滚底等于把正在上翻重读的
 * 用户按回去。
 */
import { useEffect, useMemo, useRef, useState } from "react";
import { imageSrc, isVideoRef, type Kind, type Message } from "../../ipc/playground";
import { Mark } from "../../ui/Mark";
import { Icon, Spinner } from "../../ui/primitives";
import { LIVE_ARTIFACT, type ArtifactRef } from "./artifacts";
import { Lightbox, type LightboxItem } from "./Lightbox";
import { Markdown } from "./Markdown";
import { PgIcon } from "./PgIcon";
import type { Run } from "./runs";
import { aspectOf, fmtBytes, fmtLatency, tokensPerSecond } from "./target";

/** 消息流与 artifact 预览区之间的通道：卡片报坐标，预览区亮卡片。 */
export interface ArtifactChannel {
  active: ArtifactRef | null;
  onOpen: (ref: ArtifactRef) => void;
}

export function Feed({
  kind,
  messages,
  run,
  model,
  artifact,
  onRegenerate,
  onRetryImage,
  onReveal,
  onDeleteMessage,
  onDeleteImage,
}: {
  kind: Kind;
  messages: Message[];
  run: Run | undefined;
  /** 这一会话选的模型。4.7 等思考模型首字前会空等，文案要跟普通模型分开。 */
  model?: string;
  /** 对话会话才有：可预览的代码块画成卡片，点开进右侧预览区。 */
  artifact?: ArtifactChannel;
  onRegenerate: () => void;
  /** 图片会话：用同一句提示词再出一批。 */
  onRetryImage?: (prompt: string) => void;
  onReveal: (imageId: string) => void;
  onDeleteMessage: (id: string) => void;
  onDeleteImage: (id: string) => Promise<void>;
}) {
  const box = useRef<HTMLDivElement | null>(null);
  const stick = useRef(true);
  const streaming = Boolean(run && !run.done);
  const [light, setLight] = useState<{ items: LightboxItem[]; index: number } | null>(null);

  useEffect(() => {
    if (stick.current) box.current?.scrollTo({ top: box.current.scrollHeight });
  }, [messages, run?.text, run?.thinking, run?.prompt, streaming]);

  function onScroll() {
    const el = box.current;
    if (!el) return;
    stick.current = el.scrollHeight - el.scrollTop - el.clientHeight < 64;
  }

  const last = messages[messages.length - 1];
  // 末尾是一条回复、而且此刻没在跑，才给「重新生成」。
  const canRegenerate = !streaming && kind === "chat" && last?.role === "assistant";
  // 这一轮的那句话：Rust 在流开始前就把它落库了，会话若在流的中途被重新拉过，
  // 它已经在 `messages` 末尾 —— 那就别再画一遍。
  const pendingPrompt = run && !run.done && run.prompt != null && !(last?.role === "user" && last.content === run.prompt) ? run.prompt : null;

  /**
   * 一批图连同出它们的那句提示词，交给看大图。回复上的图，提示词是它前面那条 user 消息；
   * user 消息自己带的图（附件），提示词就是这条消息本身。
   */
  function openImage(m: Message, imageId: string) {
    const i = messages.indexOf(m);
    const prompt = m.role === "user" ? m.content || null : ([...messages.slice(0, i)].reverse().find((x) => x.role === "user")?.content ?? null);
    const items: LightboxItem[] = m.images.map((img) => ({ id: img.id, mime: img.mime, prompt, model: m.role === "user" ? null : m.model, width: img.width, height: img.height, size: img.size, bytes: img.bytes, createdAt: img.createdAt }));
    setLight({ items, index: Math.max(0, items.findIndex((x) => x.id === imageId)) });
  }

  return (
    <div className="pg-feed" ref={box} onScroll={onScroll}>
      <div className="pg-feed-in">
        {messages.map((m, i) =>
          m.role === "user" ? (
            <UserBubble key={m.id} content={m.content} images={m.images.map((img) => ({ id: img.id, src: imageSrc(img.id), alt: m.content, onOpen: () => openImage(m, img.id) }))} />
          ) : kind === "chat" ? (
            <Reply key={m.id} m={m} artifact={artifact} regenerate={canRegenerate && i === messages.length - 1 ? onRegenerate : undefined} onDelete={() => onDeleteMessage(m.id)} />
          ) : (
            <ImageReply
              key={m.id}
              m={m}
              onOpen={(id) => openImage(m, id)}
              onDelete={() => onDeleteMessage(m.id)}
              onRetry={
                onRetryImage && !streaming
                  ? () => {
                      const prompt = [...messages.slice(0, i)].reverse().find((x) => x.role === "user")?.content;
                      if (prompt) onRetryImage(prompt);
                    }
                  : undefined
              }
            />
          ),
        )}
        {run && !run.done ? (
          <>
            {pendingPrompt != null ? <UserBubble content={pendingPrompt} images={run.attachments.map((a) => ({ id: a.id, src: a.url, alt: a.name }))} /> : null}
            {run.kind === "chat" ? <StreamingReply run={run} model={model} artifact={artifact} /> : <GeneratingCard run={run} />}
          </>
        ) : null}
      </div>
      {light ? (
        <Lightbox
          items={light.items}
          index={light.index}
          onIndex={(index) => setLight({ ...light, index })}
          onClose={() => setLight(null)}
          onReveal={onReveal}
          onDelete={async (id) => {
            await onDeleteImage(id);
            const items = light.items.filter((x) => x.id !== id);
            if (!items.length) setLight(null);
            else setLight({ items, index: Math.min(light.index, items.length - 1) });
          }}
        />
      ) : null}
    </div>
  );
}

/** 用户的话。随它发上去的图排在文字上面一排小图；点开看大图（还没落库的待定气泡点不开）。 */
function UserBubble({ content, images = [] }: { content: string; images?: { id: string; src: string; alt: string; onOpen?: () => void }[] }) {
  return (
    <div className="pg-msg is-user">
      <div className="pg-user">
        {images.length ? (
          <div className="pg-user-imgs">
            {images.map((img) =>
              img.onOpen ? (
                <button key={img.id} type="button" className="pg-user-img" onClick={img.onOpen} aria-label="看大图">
                  <img src={img.src} alt={img.alt} loading="lazy" decoding="async" />
                </button>
              ) : (
                <span key={img.id} className="pg-user-img">
                  <img src={img.src} alt={img.alt} />
                </span>
              ),
            )}
          </div>
        ) : null}
        {content ? <div className="pg-bubble selectable">{content}</div> : null}
      </div>
    </div>
  );
}

/** 回复左边那枚标：身份，不是头像。 */
function Avatar({ live }: { live?: boolean }) {
  return (
    <span className={`pg-avatar${live ? " is-live" : ""}`}>
      <Mark size={16} />
    </span>
  );
}

/** 回复头上那一行：模型 → 实际路由。它回答「这是谁答的」，读正文之前就得知道。 */
function ModelLine({ m }: { m: Pick<Message, "model" | "routed"> }) {
  if (!m.model && !m.routed) return null;
  return (
    <div className="pg-meta">
      {m.model ? <span className="mono pg-meta-model">{m.model}</span> : null}
      {m.routed && m.routed !== m.model ? (
        <span className="pg-meta-routed">
          → <span className="mono">{m.routed}</span>
        </span>
      ) : null}
    </div>
  );
}

/**
 * 一条回复的成绩单：首字 / 总计 / token / 吞吐。
 *
 * 摆在**消息末尾**，不在头上。这些数是读完之后才会回头去看的「刚才那次跑得怎么样」；
 * 压在正文前面，等于每读一条回复都要先跨过一串跟内容无关的数字。模型名留在头上 ——
 * 那个是身份，不是成绩。
 */
function Stats({ m }: { m: Pick<Message, "ttftMs" | "durationMs" | "usage"> }) {
  const tps = tokensPerSecond(m.usage?.completionTokens, m.durationMs, m.ttftMs);
  const facts = [
    m.ttftMs != null ? `首字 ${fmtLatency(m.ttftMs)}` : "",
    m.durationMs != null ? `总计 ${fmtLatency(m.durationMs)}` : "",
    m.usage ? `${m.usage.promptTokens} → ${m.usage.completionTokens} tokens` : "",
    tps ? `${tps >= 10 ? Math.round(tps) : tps} tok/s` : "",
  ].filter(Boolean);
  if (!facts.length) return null;
  return (
    <div className="pg-stats num">
      {facts.map((f) => (
        <span key={f} className="pg-stat">
          {f}
        </span>
      ))}
    </div>
  );
}

/**
 * 思考过程。默认收起，展开是一段带左界线的旁白。
 *
 * 上一版是裸的 `<details>` + `<pre>`：浏览器自带的那个三角标、一块方底、等宽字，
 * 读起来像在正文中间贴了一段日志。现在标题行自己画折叠标（展开转 90°），
 * 正文靠一条左界线说明「这是旁白不是回答」，并且**给了高度上限** —— 带思考的模型
 * 能吐几千字，一展开就把正文顶出屏幕，人再也找不回刚才读到哪。
 */
function Thinking({ text, live, open }: { text: string; live?: boolean; open?: boolean }) {
  return (
    <details className="pg-think" open={open}>
      <summary className="pg-think-head">
        <Icon name="chevron" size={11} className="pg-think-chev" />
        <span className="pg-think-k">思考</span>
        {live ? <span className="pg-think-live">进行中</span> : null}
        <span className="pg-think-n num">{text.length} 字</span>
      </summary>
      <div className="pg-think-body selectable">{text}</div>
    </details>
  );
}

/**
 * 消息底下那排动作：图标 + 悬停提示，不带字。
 *
 * 上一版是三枚带字的 `.btn-quiet`（复制 / 重新生成 / 🗑），一排字压在正文下面比正文还抢眼。
 * 复制、重来、删除这三个图标是通用语汇，收成图标键正合 §14.33 那条「动作尽量收成
 * 图标键 + 悬停提示，留字的只有需要一个词才敢按的」。
 */
function ActButton({
  icon,
  label,
  onClick,
  danger,
  on,
}: {
  icon: string;
  label: string;
  onClick: () => void;
  danger?: boolean;
  /** 刚复制完的那一秒。 */
  on?: boolean;
}) {
  return (
    <button
      type="button"
      className={`pg-act${danger ? " is-danger" : ""}${on ? " is-on" : ""}`}
      onClick={onClick}
      title={label}
      aria-label={label}
    >
      <Icon name={icon} size={13} />
    </button>
  );
}

function Reply({ m, artifact, regenerate, onDelete }: { m: Message; artifact?: ArtifactChannel; regenerate?: () => void; onDelete: () => void }) {
  const [copied, setCopied] = useState(false);
  // 每条消息一个稳定的 link 对象：active 不变时 Markdown 不必重渲染（它是 memo 的）。
  const link = useMemo(
    () => (artifact ? { messageId: m.id, active: artifact.active, onOpen: artifact.onOpen } : undefined),
    [artifact, m.id],
  );
  function copy() {
    void navigator.clipboard.writeText(m.content).catch(() => {});
    setCopied(true);
    window.setTimeout(() => setCopied(false), 1400);
  }
  const failedOutright = !m.content && m.error;
  return (
    <div className={`pg-msg is-reply${m.error ? " has-error" : ""}`}>
      <Avatar />
      <div className="pg-reply">
        <ModelLine m={m} />
        {m.thinking ? <Thinking text={m.thinking} /> : null}
        {m.content ? <Markdown text={m.content} artifactLink={link} /> : null}
        {m.error ? <ErrorLine text={failedOutright ? m.error : `流在这里断了：${m.error}`} onRetry={regenerate} /> : null}
        <div className="pg-foot">
          <Stats m={m} />
          <div className="pg-msg-acts">
            {m.content ? <ActButton icon={copied ? "check" : "copy"} label={copied ? "已复制" : "复制"} on={copied} onClick={copy} /> : null}
            {regenerate && !m.error ? <ActButton icon="refresh" label="重新生成" onClick={regenerate} /> : null}
            <ActButton icon="trash" label="删除这条回复" danger onClick={onDelete} />
          </div>
        </div>
      </div>
    </div>
  );
}

/** 从某一刻起过了多少整秒，每半秒刷一次。等待态的秒表与「等太久了」的提示都靠它。 */
function useElapsed(since: number): number {
  const [n, setN] = useState(() => Math.floor((Date.now() - since) / 1000));
  useEffect(() => {
    const t = window.setInterval(() => setN(Math.floor((Date.now() - since) / 1000)), 500);
    return () => window.clearInterval(t);
  }, [since]);
  return n;
}

/**
 * 首字之前的等待。三颗起伏的点是「对方在打字」的通行语言，比一个转圈更像有人在那头；
 * 超过几秒把秒数亮出来，再久一点说一句为什么——上游首字动辄两三秒、带思考的模型更久，
 * 用户要知道这不是卡死。
 */
function isSilentThinkModel(model: string | null | undefined): boolean {
  const k = (model || "").toLowerCase();
  return k.includes("grok-4.7") || k.includes("grok-4-7") || k.includes("4-7-0910") || k.includes("sand-cua");
}

function waitingLabel(selected?: string, routed?: string | null): string {
  return isSilentThinkModel(routed) || isSilentThinkModel(selected) ? "模型在思考" : "正在连上游";
}

function waitingHint(selected?: string, routed?: string | null): string {
  if (isSilentThinkModel(routed) || isSilentThinkModel(selected)) {
    return "4.7 会先在上游想完再吐字，思考过程经常不流下来，所以这里会空等 20–40 秒。不是又卡死了。";
  }
  return "上游还没吐第一个字。带思考的模型首字常要十几秒；一直没动静可以停掉重发。";
}

function Waiting({ since, what, hint }: { since: number; what: string; hint: string }) {
  const s = useElapsed(since);
  return (
    <div className="pg-waiting">
      <span className="pg-typing" aria-hidden>
        <i />
        <i />
        <i />
      </span>
      <span className="pg-waiting-text">
        {what}
        {s >= 4 ? <span className="num pg-waiting-s"> · {s} s</span> : null}
      </span>
      {s >= 8 ? <span className="pg-waiting-hint">{hint}</span> : null}
    </div>
  );
}

function StreamingReply({ run, model, artifact }: { run: Run; model?: string; artifact?: ArtifactChannel }) {
  const waiting = !run.text && !run.thinking;
  const thinkingOnly = !run.text && Boolean(run.thinking);
  const silent = isSilentThinkModel(model) || isSilentThinkModel(run.routed);
  // 流式中的那一轮还没落库：卡片坐标里的消息 id 用 LIVE_ARTIFACT 顶替，落库后工作台换回来。
  const link = useMemo(
    () => (artifact ? { messageId: LIVE_ARTIFACT, active: artifact.active, onOpen: artifact.onOpen } : undefined),
    [artifact],
  );
  return (
    <div className="pg-msg is-reply is-streaming">
      <Avatar live />
      <div className="pg-reply">
        <div className="pg-meta num">
          <span className="pg-meta-live">
            <Spinner /> {waiting ? (silent ? "思考中" : "等首字") : thinkingOnly ? "思考中" : "生成中"}
          </span>
          {run.routed ? <span className="mono pg-meta-model">{run.routed}</span> : null}
        </div>
        {waiting ? <Waiting since={run.startedAt} what={waitingLabel(model, run.routed)} hint={waitingHint(model, run.routed)} /> : null}
        {/* 还没开始出正文时思考区默认展开：那会儿它是屏幕上唯一在动的东西。 */}
        {run.thinking ? <Thinking text={run.thinking} live={!run.text} open={!run.text} /> : null}
        {run.text ? (
          <div className="pg-stream">
            <Markdown text={run.text} artifactLink={link} />
            <span className="pg-caret">▍</span>
          </div>
        ) : null}
        {run.error ? (
          <p className="pg-error">
            <span className="pg-error-dot" />
            {run.error}
          </p>
        ) : null}
      </div>
    </div>
  );
}

/** 一句错误 + 该给的出口：至少能重试。 */
function ErrorLine({ text, onRetry }: { text: string; onRetry?: () => void }) {
  return (
    <div className="pg-error">
      <span className="pg-error-dot" />
      <span className="pg-error-text">{text}</span>
      {onRetry ? (
        <button type="button" className="btn btn-sm btn-quiet pg-error-act" onClick={onRetry}>
          <Icon name="refresh" size={12} />
          重试
        </button>
      ) : null}
    </div>
  );
}

/** 请求要的规格和实际出的不一样时说一句（本地出图固定 1536×1024，所选规格会被忽略）。 */
function sizeMismatch(img: { size: string | null; width: number | null; height: number | null }): string | null {
  const m = /^(\d+)\s*[x×]\s*(\d+)$/i.exec(img.size ?? "");
  if (!m || !img.width || !img.height) return null;
  if (Number(m[1]) === img.width && Number(m[2]) === img.height) return null;
  return `要的是 ${m[1]} × ${m[2]}，实际出了 ${img.width} × ${img.height}——这条链路的规格是固定的`;
}

function ImageReply({ m, onOpen, onDelete, onRetry }: { m: Message; onOpen: (imageId: string) => void; onDelete: () => void; onRetry?: () => void }) {
  const mismatch = m.images.map(sizeMismatch).find(Boolean) ?? null;
  return (
    <div className={`pg-msg is-reply${m.error ? " has-error" : ""}`}>
      <Avatar />
      <div className="pg-reply">
        <ModelLine m={{ model: m.model, routed: null }} />
        {m.images.length ? (
          <div className={`pg-grid n${Math.min(m.images.length, 4)}`}>
            {m.images.map((img) => (
              <figure key={img.id} className="pg-fig" style={{ aspectRatio: img.width && img.height ? `${img.width} / ${img.height}` : isVideoRef(img) ? "16 / 9" : String(aspectOf(img.size)) }}>
                {isVideoRef(img) ? (
                  <Picture id={img.id} alt={m.content} mime={img.mime} />
                ) : (
                  <button type="button" className="pg-fig-btn" onClick={() => onOpen(img.id)} aria-label="看大图">
                    <Picture id={img.id} alt={m.content} mime={img.mime} />
                  </button>
                )}
                <figcaption className="pg-fig-cap num">
                  <span className="mono">
                    {img.width && img.height ? `${img.width} × ${img.height}` : isVideoRef(img) ? (img.size ?? "视频") : (img.size?.replace("x", " × ") ?? "—")}
                    <span className="pg-sep">·</span>
                    {fmtBytes(img.bytes)}
                  </span>
                  <span className="grow" />
                  <button type="button" className="ibtn" aria-label="看大图" onClick={() => onOpen(img.id)}>
                    <PgIcon name="expand" size={13} />
                  </button>
                </figcaption>
              </figure>
            ))}
          </div>
        ) : null}
        {m.content ? (
          <p className="pg-revised selectable">{m.images.some(isVideoRef) || m.content.startsWith("任务 ") ? m.content : `上游改写后的提示词：${m.content}`}</p>
        ) : null}
        {mismatch ? <p className="pg-revised">{mismatch}</p> : null}
        {m.error ? <ErrorLine text={m.error} onRetry={onRetry} /> : null}
        <div className="pg-foot">
          <Stats m={{ ttftMs: null, durationMs: m.durationMs, usage: null }} />
          <div className="pg-msg-acts">
            {onRetry && !m.error ? <ActButton icon="refresh" label="用同一句提示词再出一批" onClick={onRetry} /> : null}
            <ActButton icon="trash" label="删除这一批图" danger onClick={onDelete} />
          </div>
        </div>
      </div>
    </div>
  );
}

/**
 * 出图中的占位：按请求的规格画 n 个同比例的闪烁格子，秒表放在第一格里。
 * 出图没有中间态可报，能给的只有时间：先说通常要多久，等过了那个区间再说「还在画」——
 * 一个静止的「生成中」在四十秒后就会被当成卡死。
 */
function GeneratingCard({ run }: { run: Run }) {
  const video = run.kind === "video";
  const n = video ? 1 : Math.min(Math.max(run.image?.n ?? 1, 1), 4);
  const ratio = video ? aspectOfRatio(run.video?.aspectRatio) : aspectOf(run.image?.size);
  const elapsed = useElapsed(run.startedAt);
  const stage = video
    ? elapsed < 10
      ? "任务已提交给上游"
      : elapsed < 90
        ? "上游正在生成，我们每 4 秒查一次"
        : elapsed < 240
          ? "还在生成，长视频 / 高分辨率要几分钟"
          : "等得有点久了，最多七分钟就会放弃"
    : elapsed < 8
      ? "已发给上游"
      : elapsed < 45
        ? `正在画${n > 1 ? `，${n} 张是一张一张出的` : ""}`
        : elapsed < 120
          ? "还在画，出图偶尔要一两分钟"
          : "等得有点久了，最多再等两分钟就会放弃";
  const spec = video
    ? [run.video?.duration ? `${run.video.duration}s` : null, run.video?.resolution ?? null, run.video?.aspectRatio ?? null].filter(Boolean).join(" · ")
    : run.image?.size?.replace("x", " × ");
  return (
    <div className="pg-msg is-reply is-streaming">
      <Avatar live />
      <div className="pg-reply">
        <div className="pg-meta num">
          <span className="pg-meta-live">
            <Spinner /> {video ? "生成视频中" : "出图中"}
          </span>
          {spec ? <span className="mono pg-meta-model">{spec}</span> : null}
          <span className="pg-meta-fact">{stage}</span>
        </div>
        <div className={`pg-grid n${n}`}>
          {Array.from({ length: n }, (_, i) => (
            <div key={i} className="pg-fig skeleton" style={{ aspectRatio: String(ratio) }}>
              {i === 0 ? (
                <div className="pg-fig-wait num">
                  <Spinner />
                  <span>{elapsed} s</span>
                  <span className="pg-fig-wait-hint">{video ? "通常 1–3 分钟" : "通常 15–40 秒"}</span>
                </div>
              ) : null}
            </div>
          ))}
        </div>
        {run.error ? (
          <p className="pg-error">
            <span className="pg-error-dot" />
            {run.error}
          </p>
        ) : null}
      </div>
    </div>
  );
}

/** `16:9` → 1.777。认不出按 16:9——视频的常见画幅。 */
function aspectOfRatio(ratio: string | null | undefined): number {
  const m = /^(\d+)\s*:\s*(\d+)$/.exec(ratio ?? "");
  if (!m) return 16 / 9;
  const w = Number(m[1]);
  const h = Number(m[2]);
  return w > 0 && h > 0 ? w / h : 16 / 9;
}

/** 图片 / 视频本体。文件不在了（手动删过、或记录来自别的机器的备份）画一个说明，不是破图标。 */
function Picture({ id, alt, mime }: { id: string; alt: string; mime?: string }) {
  const [gone, setGone] = useState(false);
  if (gone) {
    return (
      <div className="pg-fig-gone">
        <PgIcon name="image" size={18} />
        <span>文件已不在本机</span>
      </div>
    );
  }
  if (mime?.startsWith("video/")) {
    return <video className="pg-img" src={imageSrc(id)} controls loop playsInline preload="metadata" onError={() => setGone(true)} />;
  }
  return <img className="pg-img" src={imageSrc(id)} alt={alt} loading="lazy" decoding="async" onError={() => setGone(true)} />;
}
