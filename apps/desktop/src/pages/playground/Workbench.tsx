/**
 * 一种会话的工作台：左栏会话列表，右边当前会话（或一个还没建的草稿）。对话与图片各挂一个。
 *
 * 三个约束：
 * 1. **真相在库里。** 每一轮由 Rust 落库；前端只交 thread_id + 新的一句话，历史不在前端拼。
 *    切会话、切页面、甚至重开应用，回来都还在。
 * 2. **进行中的一轮不跟着组件死。** 流式状态在 `runs.ts` 的模块级 store 里；切走再切回，
 *    半截回复还在，命令返回时也还有人负责刷新。
 * 3. **目标随会话记。** 模型挂在会话上，中途换模型对比时每条回复都标着
 *    自己是谁答的。新会话的默认目标取这一类最近一次的选择。
 */
import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { playground, type Kind, type ThreadDetail, type ThreadSummary } from "../../ipc/playground";
import type { RelayState } from "../../relay/useRelay";
import { go, type Route } from "../../shell/nav";
import { Mark } from "../../ui/Mark";
import { ErrorNote } from "../../ui/primitives";
import { ArtifactPanel } from "./ArtifactPanel";
import { collectArtifacts, LIVE_ARTIFACT, sameArtifact, type ArtifactRef } from "./artifacts";
import { Composer, type Blocker, type SendPayload } from "./Composer";
import { Feed } from "./Feed";
import { PgIcon } from "./PgIcon";
import { adoptActive, consumeRun, startChat, startImage, startVideo, stopRun, useRun } from "./runs";
import { defaultTarget, modelIds, retarget, threadTitle, type Catalogs, type Target } from "./target";
import { ThreadRail } from "./ThreadRail";

const SELECTED_KEY = (k: Kind) => `playground.selected.${k}`;

/** 从资产页「打开会话」：先记下要开哪个，再切到图片子项，工作台挂上来时读它。 */
export function rememberSelected(kind: Kind, id: string | null) {
  if (id) localStorage.setItem(SELECTED_KEY(kind), id);
  else localStorage.removeItem(SELECTED_KEY(kind));
}

export function Workbench({
  kind,
  relay,
  hint,
  onGo,
}: {
  kind: Kind;
  relay: RelayState;
  /** 从模型广场带进来的预选目标；用过一次就作废。 */
  hint?: { model?: string };
  onGo: (r: Route) => void;
}) {
  const [threads, setThreads] = useState<ThreadSummary[] | null>(null);
  const [selectedId, setSelectedId] = useState<string | null>(() => localStorage.getItem(SELECTED_KEY(kind)));
  const [detail, setDetail] = useState<ThreadDetail | null>(null);
  /** 新会话（还没建）的目标；null = 用默认。 */
  const [draft, setDraft] = useState<Target | null>(null);
  const [error, setError] = useState<unknown>(null);
  const run = useRun(selectedId);
  /** 预览区此刻打开的那块 artifact；null = 预览区收着。 */
  const [openArtifact, setOpenArtifact] = useState<ArtifactRef | null>(null);

  const cat = useMemo<Catalogs>(() => ({ local: relay.local }), [relay.local]);

  const reloadThreads = useCallback(async () => {
    const list = await playground.threads(kind);
    setThreads(list);
    return list;
  }, [kind]);

  useEffect(() => {
    reloadThreads().catch(setError);
  }, [reloadThreads]);

  function pickThread(id: string | null) {
    setSelectedId(id);
    setError(null);
    rememberSelected(kind, id);
  }

  // 选中的会话：拉全文；同时问一下 Rust 有没有一路还在跑（WebView 重载后模块状态是空的）。
  useEffect(() => {
    if (!selectedId) {
      setDetail(null);
      return;
    }
    let alive = true;
    playground
      .thread(selectedId)
      .then((d) => {
        if (alive) setDetail(d);
      })
      .catch(() => {
        // 会话没了（被删、库被还原）：退回新建态，别对着一个空壳。
        if (alive) pickThread(null);
      });
    void adoptActive(selectedId);
    return () => {
      alive = false;
    };
  }, [selectedId]);

  // 这一路跑完（回复已落库）：刷新会话与列表，然后把 run 收掉。
  useEffect(() => {
    if (!selectedId || !run?.done) return;
    const id = selectedId;
    let alive = true;
    Promise.all([playground.thread(id), reloadThreads()])
      .then(([d]) => {
        if (!alive) return;
        setDetail(d);
        // 流式途中打开着的 artifact 坐标还指着「live」：落库后换成真实消息 id，
        // 预览区不跟着关 —— 用户正看着呢。
        setOpenArtifact((ref) => {
          if (!ref || ref.messageId !== LIVE_ARTIFACT) return ref;
          const last = [...d.messages].reverse().find((m) => m.role === "assistant");
          return last ? { ...ref, messageId: last.id } : ref;
        });
      })
      .catch(setError)
      .finally(() => consumeRun(id));
    return () => {
      alive = false;
    };
  }, [run?.done, selectedId, reloadThreads]);

  // 从模型广场「试一下」带进来的模型：开一个新会话草稿，目标按它定。
  const consumed = useRef("");
  useEffect(() => {
    if (!hint?.model || relay.loading) return;
    if (consumed.current === hint.model) return;
    consumed.current = hint.model;
    setSelectedId(null);
    setDraft(defaultTarget(kind, threads ?? [], cat, hint));
    onGo(go("playground", { view: kind }));
  }, [hint, relay.loading, cat, threads, kind, onGo]);

  const target: Target = useMemo(() => {
    if (detail && selectedId) return { model: detail.thread.model };
    return draft ?? defaultTarget(kind, threads ?? [], cat);
  }, [detail, selectedId, draft, kind, threads, cat]);

  const busy = Boolean(run && !run.done);
  const ids = modelIds(cat, kind);

  // 本地网关能不能出图 / 出视频，看它的目录里有没有那一类模型（Cursor 生图协议、Grok 通道接上后就有），不写死。
  const localCanDo = kind === "chat" || ids.length > 0;
  const ready = Boolean(relay.gateway?.running) && localCanDo;

  const blocker: Blocker | null = useMemo(() => {
    if (!localCanDo && !relay.loading) {
      return kind === "video"
        ? { text: "本地网关此刻出不了视频", hint: "生视频走 Grok 通道：在「账号 → Grok Build」加一个付费档订阅号或 xAI API Key。", fixLabel: "去加账号", fix: () => onGo(go("accounts", { platform: "grok" })) }
        : { text: "本地网关此刻不出图", hint: "这台网关的目录里没有生图模型：接上能出图的账号（Cursor / ChatGPT）之后就有。", fixLabel: "去加账号", fix: () => onGo(go("accounts")) };
    }
    if (!relay.gateway?.running) return { text: "本地网关没开", hint: "游乐场走的就是网关自己的地址与口令，得先把它开起来。", fixLabel: "去开启", fix: () => onGo(go("gateway")) };
    if (!relay.gateway.lane.candidates.length) return { text: "网关号池里没有可用的号", hint: "开着但没号可接力，请求会被拒。", fixLabel: "去看看", fix: () => onGo(go("gateway")) };
    if (!ids.length && !relay.loading) return { text: kind === "chat" ? "没有可用的对话模型" : kind === "video" ? "没有可用的生视频模型" : "没有可用的生图模型", hint: "到模型广场看看网关此刻能映射哪些模型。", fixLabel: "模型广场", fix: () => onGo(go("models")) };
    if (!target.model.trim()) return { text: "还没选模型", hint: "在下面的模型药丸里挑一个，或直接输入模型 id。", fixLabel: "模型广场", fix: () => onGo(go("models")) };
    return null;
  }, [target, kind, localCanDo, relay.gateway, relay.loading, ids.length, onGo]);

  // 已有会话的目标改动：本地先改、稍后落库。模型选择器支持手输，一个字一次 IPC 太吵。
  const persist = useRef<number | undefined>(undefined);
  useEffect(() => () => window.clearTimeout(persist.current), []);
  function changeTarget(next: Target) {
    setError(null);
    if (detail && selectedId) {
      const id = selectedId;
      setDetail({ ...detail, thread: { ...detail.thread, model: next.model } });
      window.clearTimeout(persist.current);
      persist.current = window.setTimeout(() => {
        playground
          .setTarget(id, next.model)
          .then((t) => setThreads((list) => list?.map((s) => (s.id === t.id ? { ...s, ...t } : s)) ?? list))
          .catch(setError);
      }, 350);
    } else {
      setDraft(next);
    }
  }

  function onTargetChange(next: Target) {
    changeTarget(next);
  }

  // 目录刷新后（换了账号、网关重启），会话上记着的模型可能已经不在货架上：落回默认。
  // 只纠正草稿，不动已有会话——那上面的模型名是用户当时明确选的，而且输入框本来就允许
  // 敲目录里没有的 id。
  useEffect(() => {
    if (!draft || selectedId) return;
    const fixed = retarget(draft, kind, cat);
    if (fixed !== draft) setDraft(fixed);
  }, [draft, selectedId, kind, cat]);

  /** 上一批图要的规格；「再来一批」照它发。 */
  const lastImageSize = useRef<string | null>(null);

  async function send({ prompt, images, image, video }: SendPayload) {
    setError(null);
    try {
      let id = selectedId;
      if (!id) {
        const t = await playground.createThread(kind, target.model);
        id = t.id;
        setDraft(null);
        pickThread(t.id);
        await reloadThreads();
      }
      if (kind === "chat") void startChat(id, prompt, images);
      else if (kind === "video") {
        const frame = images[0];
        void startVideo(
          id,
          {
            prompt,
            imageBase64: frame?.dataBase64 ?? null,
            imageMime: frame?.mime ?? null,
            duration: video?.duration ?? null,
            aspectRatio: video?.aspectRatio ?? null,
            resolution: video?.resolution ?? null,
          },
          images,
        );
      } else {
        lastImageSize.current = image?.size ?? null;
        void startImage(id, { prompt, size: image?.size ?? null, n: image?.n ?? 1 });
      }
      // 标题由 Rust 在第一句话落库时定（首行、40 字）；列表先按同一规则补上，别让它顶着「新对话」等流走完。
      const tid = id;
      setThreads((list) => list?.map((s) => (s.id === tid && !s.title ? { ...s, title: (prompt.split("\n").find((l) => l.trim()) ?? prompt).trim().slice(0, 40), updatedAt: new Date().toISOString() } : s)) ?? list);
    } catch (e) {
      setError(e);
    }
  }

  async function removeThread(id: string) {
    try {
      await playground.deleteThread(id);
      if (id === selectedId) pickThread(null);
      await reloadThreads();
    } catch (e) {
      setError(e);
      throw e;
    }
  }

  async function refreshDetail() {
    if (!selectedId) return;
    const [d] = await Promise.all([playground.thread(selectedId), reloadThreads()]);
    setDetail(d);
  }

  async function removeMessage(id: string) {
    try {
      await playground.deleteMessage(id);
      await refreshDetail();
    } catch (e) {
      setError(e);
    }
  }

  async function removeImage(id: string) {
    try {
      await playground.deleteImage(id);
      await refreshDetail();
    } catch (e) {
      setError(e);
      throw e;
    }
  }

  const messages = detail && detail.thread.id === selectedId ? detail.messages : [];
  const title = detail && selectedId ? threadTitle(detail.thread.title, detail.thread.kind) : threadTitle("", kind);
  const imageCount = messages.reduce((n, m) => n + m.images.length, 0);

  /* ── artifact 预览区 ────────────────────────────────────────────────────
     列表分两半算：落库的那部分只在 messages 变时重算；流式那半跟着 run.text 逐帧走，
     打开着的面板因此能看到代码一边写一边长。 */

  // 切会话、切对话/图片，预览区都收起来 —— 它属于「刚才那条会话」。
  useEffect(() => setOpenArtifact(null), [selectedId, kind]);

  // run 对象在 done 之后、consumeRun 之前还在：这段窗口里 live 坐标仍然有效，
  // 面板不会因为「done 了但还没刷新出消息」闪一下关掉。
  const liveText = run && run.kind === "chat" && run.text ? run.text : null;
  const artifacts = useMemo(
    () => (kind === "chat" ? collectArtifacts(messages, liveText) : []),
    [kind, messages, liveText],
  );
  const currentArtifact = openArtifact ? (artifacts.find((a) => sameArtifact(a.ref, openArtifact)) ?? null) : null;
  const artifactIndex = currentArtifact ? artifacts.indexOf(currentArtifact) : -1;

  // 指着的那块没了（那条回复被删了）：把预览区收掉，别留一个空壳。
  // detail 还没回来的时候不算 —— 那会儿 messages 是空的，什么都找不到。
  useEffect(() => {
    if (openArtifact && detail && !currentArtifact) setOpenArtifact(null);
  }, [openArtifact, detail, currentArtifact]);

  const toggleArtifact = useCallback((ref: ArtifactRef) => {
    setOpenArtifact((cur) => (sameArtifact(cur, ref) ? null : ref));
  }, []);

  const artifactChannel = useMemo(
    () => ({ active: openArtifact, onOpen: toggleArtifact }),
    [openArtifact, toggleArtifact],
  );

  return (
    <div className={`pg is-${kind}${currentArtifact ? " has-art" : ""}`}>
      <ThreadRail kind={kind} threads={threads} selectedId={selectedId} onSelect={pickThread} onNew={() => pickThread(null)} onDelete={removeThread} />

      <section className="pg-main" aria-label={title}>
        <header className="pg-head">
          <TitleEditor
            key={selectedId ?? "draft"}
            title={title}
            editable={Boolean(detail && selectedId)}
            onRename={async (t) => {
              if (!selectedId || !detail) return;
              const thread = await playground.renameThread(selectedId, t);
              setDetail({ ...detail, thread });
              setThreads((list) => list?.map((s) => (s.id === thread.id ? { ...s, ...thread } : s)) ?? list);
            }}
          />
          <span className="grow" />
          {detail && selectedId ? (
            <span className="pg-head-meta num">
              {messages.length} 条{detail.thread.kind === "image" ? ` · ${imageCount} 张图` : detail.thread.kind === "video" ? ` · ${imageCount} 段视频` : ""}
            </span>
          ) : null}
        </header>

        {error ? (
          <div className="pg-note">
            <ErrorNote error={error} />
          </div>
        ) : null}

        {selectedId && !detail ? (
          <div className="pg-feed">
            <div className="pg-feed-in">
              <div className="skeleton" style={{ height: 48, width: "55%", marginLeft: "auto" }} />
              <div className="skeleton" style={{ height: 120 }} />
            </div>
          </div>
        ) : messages.length === 0 && !busy ? (
          <EmptyFeed kind={kind} />
        ) : (
          <Feed
            kind={kind}
            messages={messages}
            run={run}
            model={target.model}
            artifact={kind === "chat" ? artifactChannel : undefined}
            onRegenerate={() => selectedId && void startChat(selectedId, null)}
            onRetryImage={kind === "image" ? (prompt) => selectedId && void startImage(selectedId, { prompt, size: lastImageSize.current, n: 1 }) : undefined}
            onReveal={(id) => void playground.revealImage(id).catch(setError)}
            onDeleteMessage={(id) => void removeMessage(id)}
            onDeleteImage={removeImage}
          />
        )}

        <Composer
          kind={kind}
          target={target}
          cat={cat}
          onTargetChange={onTargetChange}
          localReady={Boolean(relay.gateway?.running) && localCanDo}
          blocker={blocker}
          disabled={!ready || Boolean(blocker)}
          busy={busy}
          empty={messages.length === 0 && !busy}
          onSend={(payload) => void send(payload)}
          onStop={() => selectedId && void stopRun(selectedId)}
        />
      </section>

      {currentArtifact ? (
        <ArtifactPanel
          artifact={currentArtifact}
          index={artifactIndex}
          total={artifacts.length}
          onJump={(delta) => {
            const next = artifacts[artifactIndex + delta];
            if (next) setOpenArtifact(next.ref);
          }}
          onClose={() => setOpenArtifact(null)}
        />
      ) : null}
    </div>
  );
}

/** 空会话的主区：一句话说清这是干什么的。示例提示词在输入区上方，离手更近。 */
function EmptyFeed({ kind }: { kind: Kind }) {
  return (
    <div className="pg-feed">
      <div className="pg-empty">
        <span className="pg-empty-mark">{kind === "chat" ? <Mark size={26} /> : <PgIcon name="spark" size={22} />}</span>
        <p className="pg-empty-title">{kind === "chat" ? "开始一段对话" : kind === "video" ? "描述这段视频" : "描述你想要的画面"}</p>
        <p className="pg-empty-sub">
          {kind === "chat"
            ? "走的是和客户端相同的地址与钥匙。多轮历史、思考过程、首字与耗时都会留在本机。"
            : kind === "video"
              ? "走本地网关的 Grok 通道（xAI Imagine）出视频：文生视频，或带一张首帧图。成片落在本机、汇入「资产」。一段要一到三分钟。"
              : "走本地网关出图，图片落在本机、汇入「资产」。同一个会话里可以反复改提示词对比效果。"}
        </p>
      </div>
    </div>
  );
}

/** 标题：点一下就地改名。空会话（草稿）不能改——它还没有名字可改。 */
function TitleEditor({ title, editable, onRename }: { title: string; editable: boolean; onRename: (t: string) => Promise<void> }) {
  const [editing, setEditing] = useState(false);
  const [value, setValue] = useState(title);
  const [err, setErr] = useState<unknown>(null);

  useEffect(() => setValue(title), [title]);

  if (!editable) return <h1 className="pg-title truncate">{title}</h1>;
  if (!editing) {
    return (
      <button type="button" className="pg-title-btn" title="改名" onClick={() => setEditing(true)}>
        <h1 className="pg-title truncate">{title}</h1>
      </button>
    );
  }
  const commit = async () => {
    const t = value.trim();
    setEditing(false);
    if (!t || t === title) {
      setValue(title);
      return;
    }
    try {
      await onRename(t);
    } catch (e) {
      setErr(e);
      setValue(title);
    }
  };
  return (
    <>
      <input
        className="input pg-title-input"
        autoFocus
        value={value}
        maxLength={80}
        onChange={(e) => setValue(e.target.value)}
        onBlur={() => void commit()}
        onKeyDown={(e) => {
          if (e.key === "Enter") void commit();
          if (e.key === "Escape") {
            setValue(title);
            setEditing(false);
          }
        }}
      />
      {err ? <ErrorNote error={err} /> : null}
    </>
  );
}
