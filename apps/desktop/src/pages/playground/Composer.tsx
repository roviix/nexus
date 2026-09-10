/**
 * 输入区 —— 这一页的控制中心。
 *
 * 一个带描边的盒子，从上到下三层：附件缩略图条、会长高的 textarea、底栏。底栏左边一串
 * 药丸（附件 / 模型，图片会话再加规格与张数），右边一枚圆形发送键。
 * 目标控件长在这里而不是顶上一条工具条：换模型、挑规格都是「发这一句之前」的
 * 决定，手不该在屏幕两端来回跑；条件不齐时那条横幅也就紧贴着输入框，看完就能改。
 *
 * Enter 发送、Shift+Enter 换行是对话框的通行约定。中文输入法组词途中的 Enter 是「选词」，
 * `isComposing` 不挡的话会把半截拼音发出去。停止键在生成时**原位替换**发送键——手指
 * 不用换地方。
 */
import { useEffect, useRef, useState, type DragEvent, type PointerEvent as ReactPointerEvent, type ReactNode, type RefObject } from "react";
import type { Kind } from "../../ipc/playground";
import { Banner, Icon } from "../../ui/primitives";
import {
  classify,
  IMAGE_ACCEPT,
  MAX_IMAGES,
  nameOf,
  newAttachmentId,
  payloadOf,
  reject,
  type ImageAttachment,
} from "./attachments";
import { PgIcon } from "./PgIcon";
import { PillMenu } from "./PillMenu";
import { DEFAULT_IMAGE_SIZE, fixedSizeOf, fmtSize, modelIds, sizeChip, sizeOptionsOf, type Catalogs, type Target } from "./target";
import "./composer.css";

export interface ImageOptions {
  /** null = 这条链路不认规格（走 Cursor 出图固定 1536×1024），不发。 */
  size: string | null;
  n: number;
}

/** 条件不齐：说清是什么、给一个去处。横幅就摆在输入框上面。 */
export interface Blocker {
  text: string;
  hint: string;
  fixLabel: string;
  fix: () => void;
}

/** 生视频的参数。首帧图走附件（最多一张）。 */
export interface VideoOptions {
  duration: number;
  aspectRatio: string;
  resolution: string;
}

export interface SendPayload {
  /** 已经把文本附件内联进去的那段话。 */
  prompt: string;
  /** 图片附件；字节随命令上去，缩略图交给 run。视频会话里就是首帧。 */
  images: ImageAttachment[];
  image?: ImageOptions;
  video?: VideoOptions;
}

const CHAT_SAMPLES = ["用一句话介绍你自己，并说出你是哪个模型。", "把下面这段话改写得更简洁：", "写一个 Python 函数，判断字符串是不是回文。"];
const IMAGE_SAMPLES = ["雨夜的东京街头，霓虹倒影，电影感，35mm", "一只戴着圆框眼镜的橘猫在看书，水彩插画", "极简主义海报：一颗孤独的行星，大面积留白"];
const VIDEO_SAMPLES = ["海浪拍在黑色礁石上，慢镜头，黄昏逆光", "一只柯基在雪地里奔跑，镜头跟随，电影感", "城市夜景延时，车流化成光带，无人机缓慢上升"];

/** 输入框跟着内容长的上限；再高就得用户自己从顶边拖。 */
const AUTO_MAX = 180;
const MIN_HEIGHT = 26;

/** xAI Imagine 认的时长 / 分辨率 / 画幅。1–15 秒；官方客户端默认 6 秒 480p。 */
const VIDEO_DURATIONS = [4, 6, 8, 10, 15];
const VIDEO_RESOLUTIONS = ["480p", "720p", "1080p"];
const VIDEO_RATIOS = ["16:9", "9:16", "1:1", "4:3", "3:4"];

export function Composer({
  kind,
  target,
  cat,
  onTargetChange,
  localReady,
  blocker,
  disabled,
  busy,
  empty,
  onSend,
  onStop,
}: {
  kind: Kind;
  target: Target;
  cat: Catalogs;
  onTargetChange: (next: Target) => void;
  localReady: boolean;
  /** 就绪拦截；有它时发不出去。 */
  blocker: Blocker | null;
  /** 目标没就绪：输入框可以打字、可以改目标，但发不出去。 */
  disabled: boolean;
  /** 有一路在跑。 */
  busy: boolean;
  /** 会话里还没有内容：给几条示例提示词。 */
  empty: boolean;
  onSend: (payload: SendPayload) => void;
  onStop: () => void;
}) {
  const [text, setText] = useState("");
  const [size, setSize] = useState(DEFAULT_IMAGE_SIZE);
  const [n, setN] = useState(1);
  const [duration, setDuration] = useState(6);
  const [resolution, setResolution] = useState("720p");
  const [ratio, setRatio] = useState("16:9");
  // 只收图片。文本文件那条路去掉了 —— 见 `AttachButton`。
  const [attachments, setAttachments] = useState<ImageAttachment[]>([]);
  /** 附件被拒的原因；下一次成功添加就消失。 */
  const [notice, setNotice] = useState<string | null>(null);
  const [dropping, setDropping] = useState(false);
  const ta = useRef<HTMLTextAreaElement | null>(null);
  /**
   * 用户从顶边拖出来的高度；null = 跟着内容长（默认）。
   * 拖过之后就不再自动缩回：拉高是一个明确的意图（要写长东西），内容变少也该保持。
   * 双击顶边恢复自动。
   */
  const manualHeight = useRef<number | null>(null);
  const [resizing, setResizing] = useState(false);

  const sizes = sizeOptionsOf(kind);

  // 选中的规格不在可选档里就落到第一档。
  useEffect(() => {
    const first = sizes[0];
    if (first && !sizes.includes(size)) setSize(first);
  }, [sizes, size]);

  useEffect(() => {
    ta.current?.focus();
  }, [kind]);

  // 缩略图的 object URL：还没发出去就卸载了（切页、切会话）要自己收。
  const live = useRef<ImageAttachment[]>([]);
  live.current = attachments;
  useEffect(
    () => () => {
      for (const a of live.current) URL.revokeObjectURL(a.url);
    },
    [],
  );

  function autosize() {
    const el = ta.current;
    if (!el) return;
    if (manualHeight.current != null) {
      el.style.height = `${manualHeight.current}px`;
      return;
    }
    el.style.height = "0px";
    el.style.height = `${Math.min(el.scrollHeight, AUTO_MAX)}px`;
  }

  /** 顶边拖动：往上拉变高、往下推变矮。松手时低于自动高度就交回自动。 */
  function startResize(e: ReactPointerEvent<HTMLElement>) {
    const el = ta.current;
    if (!el) return;
    e.preventDefault();
    const handle = e.currentTarget;
    const startY = e.clientY;
    const startH = el.getBoundingClientRect().height;
    const max = Math.max(AUTO_MAX, Math.round(window.innerHeight * 0.6));
    handle.setPointerCapture(e.pointerId);
    setResizing(true);
    const onMove = (ev: PointerEvent) => {
      const next = Math.round(Math.min(max, Math.max(MIN_HEIGHT, startH + (startY - ev.clientY))));
      manualHeight.current = next;
      el.style.height = `${next}px`;
    };
    const onUp = () => {
      handle.removeEventListener("pointermove", onMove);
      handle.removeEventListener("pointerup", onUp);
      handle.removeEventListener("pointercancel", onUp);
      setResizing(false);
      // 拖到比内容自然高度还矮，说明是想「收回去」——交还给自动。
      const h = manualHeight.current;
      if (h != null) {
        el.style.height = "0px";
        const natural = Math.min(el.scrollHeight, AUTO_MAX);
        if (h <= natural) manualHeight.current = null;
        autosize();
      }
    };
    handle.addEventListener("pointermove", onMove);
    handle.addEventListener("pointerup", onUp);
    handle.addEventListener("pointercancel", onUp);
  }

  function resetResize() {
    manualHeight.current = null;
    autosize();
  }

  /** 图片会话走的是 images 接口，那条链路没有附件可带；视频会话能带一张首帧（图生视频）。 */
  const canAttach = kind === "chat" || kind === "video";
  const maxAttach = kind === "video" ? 1 : MAX_IMAGES;
  // 视频：有首帧图就可以没有提示词（纯图生视频）。
  const canSend = !disabled && !busy && (text.trim() !== "" || (kind === "video" && attachments.length > 0));
  /** 出图规格固定的链路：规格药丸换成一枚说明，也不把 size 发上去——发了只会被忽略。 */
  const fixedSize = fixedSizeOf(kind, target, cat);

  async function addFiles(files: File[]) {
    if (!canAttach || !files.length) return;
    const added: ImageAttachment[] = [];
    let problem: string | null = null;
    for (const f of files) {
      // 拖进来 / 粘进来的东西要在这儿拦一道：选择器只列图片，但这两条路绕得过它。
      if (classify(f) !== "image") {
        problem ??= `「${f.name}」带不了，这里只能带图片（png / jpg / webp / gif）。`;
        continue;
      }
      if (attachments.length + added.length >= maxAttach) {
        problem ??= kind === "video" ? "图生视频只认一张首帧。" : `最多带 ${maxAttach} 张。`;
        continue;
      }
      const why = reject(f, [...attachments, ...added]);
      if (why) {
        problem ??= why;
        continue;
      }
      try {
        const dataUrl = await read(f);
        added.push({ kind: "image", id: newAttachmentId(), name: nameOf(f), mime: f.type || "image/png", dataBase64: payloadOf(dataUrl), bytes: f.size, url: URL.createObjectURL(f) });
      } catch {
        problem ??= `「${f.name}」读不出来，换个文件试试。`;
      }
    }
    if (added.length) setAttachments((list) => [...list, ...added]);
    setNotice(problem);
    if (added.length) ta.current?.focus();
  }

  function drop(id: string) {
    setAttachments((list) => {
      const hit = list.find((a) => a.id === id);
      if (hit) URL.revokeObjectURL(hit.url);
      return list.filter((a) => a.id !== id);
    });
    setNotice(null);
  }

  function send() {
    if (!canSend) return;
    const images = attachments;
    const prompt = text.trim();
    setText("");
    // 缩略图的 URL 交给这一路 run，这里只松手，不 revoke。
    setAttachments([]);
    setNotice(null);
    requestAnimationFrame(autosize);
    onSend({
      prompt,
      images,
      image: kind === "image" ? { size: fixedSize ? null : size, n } : undefined,
      video: kind === "video" ? { duration, aspectRatio: ratio, resolution } : undefined,
    });
  }

  function pick(s: string) {
    setText(s);
    requestAnimationFrame(() => {
      autosize();
      ta.current?.focus();
    });
  }

  function onDrop(e: DragEvent) {
    e.preventDefault();
    setDropping(false);
    void addFiles([...e.dataTransfer.files]);
  }

  return (
    <div className="pg-compose">
      {empty && !text && !attachments.length ? (
        <div className="pg-samples">
          {(kind === "chat" ? CHAT_SAMPLES : kind === "video" ? VIDEO_SAMPLES : IMAGE_SAMPLES).map((s) => (
            <button key={s} type="button" className="tier" onClick={() => pick(s)}>
              {s}
            </button>
          ))}
        </div>
      ) : null}

      {blocker ? (
        <Banner
          tone="warn"
          title={blocker.text}
          hint={blocker.hint}
          action={
            <button type="button" className="btn btn-sm" onClick={blocker.fix}>
              {blocker.fixLabel}
            </button>
          }
        />
      ) : null}
      {notice ? (
        <Banner
          tone="bad"
          title={notice}
          action={
            <button type="button" className="btn btn-sm" onClick={() => setNotice(null)}>
              知道了
            </button>
          }
        />
      ) : null}

      <div
        className={`pg-box${dropping ? " is-drop" : ""}${resizing ? " is-resizing" : ""}`}
        onDragOver={(e) => {
          if (!canAttach) return;
          e.preventDefault();
          setDropping(true);
        }}
        onDragLeave={(e) => {
          if (!e.currentTarget.contains(e.relatedTarget as Node | null)) setDropping(false);
        }}
        onDrop={onDrop}
      >
        {/* 顶边的拖柄：整条边都能抓，中间一枚短横提示「这里能拖」。双击回到自动高度。 */}
        <div
          className="pg-grip"
          role="separator"
          aria-orientation="horizontal"
          aria-label="拖动调整输入框高度，双击恢复"
          title="拖动调整高度 · 双击恢复"
          onPointerDown={startResize}
          onDoubleClick={resetResize}
        >
          <i aria-hidden />
        </div>

        {attachments.length ? (
          <div className="pg-atts">
            {attachments.map((a) => (
              <figure key={a.id} className="pg-att is-img">
                <img src={a.url} alt={a.name} />
                <DropButton name={a.name} onClick={() => drop(a.id)} />
              </figure>
            ))}
          </div>
        ) : null}

        <textarea
          ref={ta}
          className="pg-input"
          rows={1}
          value={text}
          placeholder={kind === "chat" ? "说点什么… Enter 发送，Shift + Enter 换行" : kind === "video" ? "描述这段视频里发生什么… 可以带一张首帧图。Enter 生成" : "描述你想要的画面… Enter 生成"}
          onChange={(e) => {
            setText(e.target.value);
            autosize();
          }}
          onPaste={(e) => {
            const files = [...e.clipboardData.files];
            if (!canAttach || !files.length) return;
            e.preventDefault();
            void addFiles(files);
          }}
          onKeyDown={(e) => {
            if (e.key === "Enter" && !e.shiftKey && !e.nativeEvent.isComposing) {
              e.preventDefault();
              send();
            }
          }}
        />

        <div className="pg-cbar">
          {canAttach ? <AttachButton disabled={busy} max={maxAttach} hint={kind === "video" ? "首帧图（图生视频）" : undefined} onFiles={(files) => void addFiles(files)} /> : null}

          {/* 网关就绪与否的一枚小灯：亮着才发得出去。 */}
          <span className="pg-pill-btn pg-pill-static" title={localReady ? "本地网关就绪" : "本地网关未就绪"}>
            <span className={`pg-dot${localReady ? " is-ok" : ""}`} />
            <span className="pg-pill-v">本地网关</span>
          </span>

          <ModelMenu kind={kind} target={target} cat={cat} disabled={busy} onChange={(model) => onTargetChange({ ...target, model })} />

          {kind === "image" ? (
            <>
              {fixedSize ? (
                <span className="pg-pill-btn pg-pill-static num" title="这个模型走 Cursor 的出图协议，请求里没有尺寸参数，选什么都出这个规格">
                  <span className="pg-pill-k">规格</span>
                  <span className="pg-pill-v">{fmtSize(fixedSize)} · 固定</span>
                </span>
              ) : (
                <PillMenu<string>
                  label="规格"
                  value={size}
                  disabled={busy}
                  options={sizes.map((s) => {
                    const chip = sizeChip(s);
                    return { id: s, label: `${chip.alias} · ${chip.ratio}`, meta: fmtSize(s) };
                  })}
                  onChange={setSize}
                />
              )}
              <PillMenu<string>
                label="张数"
                value={String(n)}
                disabled={busy}
                options={[1, 2, 3, 4].map((k) => ({ id: String(k), label: `${k} 张` }))}
                onChange={(v) => setN(Number(v))}
              />
            </>
          ) : null}

          {kind === "video" ? (
            <>
              <PillMenu<string>
                label="时长"
                value={String(duration)}
                disabled={busy}
                options={VIDEO_DURATIONS.map((d) => ({ id: String(d), label: `${d} 秒` }))}
                onChange={(v) => setDuration(Number(v))}
              />
              <PillMenu<string>
                label="清晰度"
                value={resolution}
                disabled={busy}
                options={VIDEO_RESOLUTIONS.map((r) => ({ id: r, label: r, meta: r === "1080p" ? "仅 video-1.5" : undefined }))}
                onChange={setResolution}
              />
              <PillMenu<string> label="画幅" value={ratio} disabled={busy} options={VIDEO_RATIOS.map((r) => ({ id: r, label: r }))} onChange={setRatio} />
            </>
          ) : null}

          <span className="grow" />

          {busy ? (
            <button type="button" className="pg-send is-stop" onClick={onStop} title="停止" aria-label="停止">
              <PgIcon name="stop" size={15} />
            </button>
          ) : (
            <button type="button" className="pg-send" disabled={!canSend} onClick={send} title={blocker ? blocker.text : kind === "chat" ? "发送（Enter）" : "生成（Enter）"} aria-label={kind === "chat" ? "发送" : "生成"}>
              <PgIcon name={kind === "chat" ? "up" : "spark"} size={16} />
            </button>
          )}
        </div>
      </div>
    </div>
  );
}

function DropButton({ name, onClick }: { name: string; onClick: () => void }) {
  return (
    <button type="button" className="pg-att-x" title="移除" aria-label={`移除 ${name}`} onClick={onClick}>
      <Icon name="close" size={11} />
    </button>
  );
}

/**
 * 「＋」：直接开图片选择器。
 *
 * 上一版是个两条路的小菜单（上传图片 / 上传文本文件）。文本那条撤了 —— 这条链路带不了
 * 文件，摆一个用不了的入口比不摆更糟。只剩一条路之后，菜单本身也没有存在的理由了：
 * 一次点击就该到选择器，中间那一跳是白跳的。
 */
function AttachButton({ disabled, max, hint, onFiles }: { disabled: boolean; max: number; hint?: string; onFiles: (files: File[]) => void }) {
  const input = useRef<HTMLInputElement>(null);
  return (
    <span className="pg-pill">
      <button
        type="button"
        className="pg-pill-btn pg-attach-btn"
        disabled={disabled}
        aria-label="添加图片"
        title={hint ? `${hint} · 也可以直接粘贴或拖进输入框` : `添加图片 · 最多 ${max} 张，也可以直接粘贴截图或拖进输入框`}
        onClick={() => input.current?.click()}
      >
        <Icon name="plus" size={14} />
      </button>
      <input
        ref={input}
        type="file"
        hidden
        multiple={max > 1}
        accept={IMAGE_ACCEPT}
        onChange={() => {
          const el = input.current;
          if (!el) return;
          onFiles([...(el.files ?? [])]);
          // 同一个文件连选两次也要触发 change。
          el.value = "";
        }}
      />
    </span>
  );
}

/**
 * 模型：药丸打开一个带搜索框的弹层。
 *
 * 和接入页那个 `ModelPicker` 是同一套能力（可搜、可手输目录外的 id——网关不校验模型名），
 * 但外观是药丸不是裸输入框：这一排控件要长成一家人，而且不打字的时候框里那个完整 id
 * 会把候选筛得只剩它自己。
 */
function ModelMenu({ kind, target, cat, disabled, onChange }: { kind: Kind; target: Target; cat: Catalogs; disabled: boolean; onChange: (model: string) => void }) {
  const [open, setOpen] = useState(false);
  const [kw, setKw] = useState("");
  const box = useRef<HTMLDivElement>(null);
  useDismiss(open, box, () => setOpen(false));

  const ids = modelIds(cat, kind);
  const typed = kw.trim();
  const hit = ids.filter((id) => id.toLowerCase().includes(typed.toLowerCase()));
  const custom = typed && !ids.includes(typed) ? typed : "";

  function commit(model: string) {
    onChange(model);
    setKw("");
    setOpen(false);
  }

  return (
    <div ref={box} className={`pg-pill pg-mmenu${open ? " is-open" : ""}`}>
      <button
        type="button"
        className="pg-pill-btn"
        disabled={disabled}
        aria-haspopup="listbox"
        aria-expanded={open}
        title={target.model || "选一个模型"}
        onClick={() => {
          setKw("");
          setOpen((v) => !v);
        }}
      >
        <span className={`pg-pill-v truncate mono${target.model ? "" : " faint"}`}>{target.model || "选模型"}</span>
        <Icon name="chevron" size={12} className="pg-pill-chev" />
      </button>
      {open ? (
        <div className="mpick-pop pg-pill-pop pg-mmenu-pop" role="listbox">
          <div className="pg-mmenu-search">
            <Icon name="search" size={13} />
            <input
              className="pg-mmenu-input mono"
              value={kw}
              autoFocus
              spellCheck={false}
              placeholder={ids.length ? "搜模型，或直接输入 id" : "网关还没给目录，直接输入 id"}
              onChange={(e) => setKw(e.target.value)}
              onKeyDown={(e) => {
                if (e.key === "Enter") {
                  e.preventDefault();
                  const first = hit[0];
                  if (custom) commit(custom);
                  else if (first) commit(first);
                }
              }}
            />
          </div>
          <div className="pg-mmenu-list">
            {custom ? (
              <button type="button" role="option" aria-selected={false} className="mpick-opt" onClick={() => commit(custom)}>
                <span className="mono truncate">{custom}</span>
                <span className="mpick-meta">目录外，直接用</span>
              </button>
            ) : null}
            {hit.map((id) => (
              <Option key={id} id={id} on={id === target.model} meta={null} onClick={() => commit(id)} />
            ))}
            {!hit.length && !custom ? <p className="pg-mmenu-none">{ids.length ? "没有匹配的模型" : "网关上没有可用模型"}</p> : null}
          </div>
        </div>
      ) : null}
    </div>
  );
}

function Option({ id, on, meta, onClick }: { id: string; on: boolean; meta: ReactNode; onClick: () => void }) {
  return (
    <button type="button" role="option" aria-selected={on} className={`mpick-opt${on ? " is-on" : ""}`} onClick={onClick}>
      <span className="mono truncate">{id}</span>
      {meta ? <span className="mpick-meta">{meta}</span> : null}
    </button>
  );
}

/** 点外面或按 Esc 就收起来。 */
function useDismiss(open: boolean, box: RefObject<HTMLElement | null>, close: () => void) {
  // 关的动作多半是内联箭头函数，每次渲染都换；拿它当依赖会让监听器每帧重挂一遍。
  const fn = useRef(close);
  fn.current = close;
  useEffect(() => {
    if (!open) return;
    const onDown = (e: MouseEvent) => {
      if (!box.current?.contains(e.target as Node)) fn.current();
    };
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") fn.current();
    };
    // 捕获阶段，不是冒泡：Tauri 的拖拽脚本在 document 上先挂了一个 mousedown，落在
    // `data-tauri-drag-region` 上时它会 stopImmediatePropagation。挂在冒泡阶段的话，
    // 「开着弹层去拖窗口」这一下收不到事件，窗口移完弹层还开着。
    document.addEventListener("mousedown", onDown, true);
    document.addEventListener("keydown", onKey);
    return () => {
      document.removeEventListener("mousedown", onDown, true);
      document.removeEventListener("keydown", onKey);
    };
  }, [open, box]);
}

function read(f: File): Promise<string> {
  return new Promise((resolve, fail) => {
    const r = new FileReader();
    r.onerror = () => fail(new Error("读不出来"));
    r.onload = () => resolve(String(r.result ?? ""));
    r.readAsDataURL(f);
  });
}
