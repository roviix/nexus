/**
 * 资产：所有生成过的图，一张不漏地摊在一面墙上。
 *
 * 会话是「按时间线看」，这里是「按结果看」——想找上周那张海报、想清掉一批没用的、想知道
 * 图片一共占了多少磁盘，都不该去一个个会话里翻。方格是等大的正方形、图按 cover 裁：一面墙
 * 要整齐才扫得快，完整画幅留给点开之后的大图。
 */
import { useCallback, useEffect, useMemo, useState } from "react";
import { imageSrc, isVideoRef, playground, type Asset } from "../../ipc/playground";
import { Icon, ErrorNote } from "../../ui/primitives";
import { Lightbox } from "./Lightbox";
import { PgIcon } from "./PgIcon";
import { PillMenu } from "./PillMenu";
import { fmtBytes } from "./target";

export function AssetsView({ onOpenThread, onGoImages }: { onOpenThread: (threadId: string, kind: "image" | "video") => void; onGoImages: () => void }) {
  const [assets, setAssets] = useState<Asset[] | null>(null);
  const [error, setError] = useState<unknown>(null);
  const [q, setQ] = useState("");
  const [model, setModel] = useState<string | null>(null);
  const [kind, setKind] = useState<AssetKind>("all");
  const [open, setOpen] = useState<number | null>(null);

  const reload = useCallback(async () => {
    try {
      setAssets(await playground.assets());
      setError(null);
    } catch (e) {
      setError(e);
    }
  }, []);

  useEffect(() => {
    void reload();
  }, [reload]);

  /** 图 / 视频各有多少。只有一种时不摆类型筛选——单选项的分段控件是噱头。 */
  const counts = useMemo(() => {
    let image = 0;
    let video = 0;
    for (const a of assets ?? []) {
      if (isVideoRef(a)) video += 1;
      else image += 1;
    }
    return { image, video };
  }, [assets]);

  // 模型列表跟着类型筛选走：看视频时不该列出一串出图模型。
  const inKind = useMemo(() => (assets ?? []).filter((a) => kind === "all" || (kind === "video") === isVideoRef(a)), [assets, kind]);

  const models = useMemo(() => {
    const n = new Map<string, number>();
    for (const a of inKind) n.set(a.model ?? "—", (n.get(a.model ?? "—") ?? 0) + 1);
    return [...n.entries()].sort((a, b) => b[1] - a[1]);
  }, [inKind]);

  // 切类型后原来选的模型可能已经不在候选里，放开它而不是让墙空着。
  useEffect(() => {
    if (model && !models.some(([m]) => m === model)) setModel(null);
  }, [model, models]);

  const shown = useMemo(() => {
    const kw = q.trim().toLowerCase();
    return inKind.filter((a) => (!model || (a.model ?? "—") === model) && (!kw || (a.prompt ?? "").toLowerCase().includes(kw) || a.threadTitle.toLowerCase().includes(kw) || (a.model ?? "").toLowerCase().includes(kw)));
  }, [inKind, q, model]);

  const totalBytes = (assets ?? []).reduce((n, a) => n + a.bytes, 0);

  async function remove(id: string) {
    await playground.deleteImage(id);
    setAssets((list) => list?.filter((a) => a.id !== id) ?? list);
  }

  return (
    <div className="pg pg-assets">
      {/* 标题一行到底：标题 + 等宽计数在左，筛选与搜索在右。图标和第二行副标题都撤掉了——
          侧栏已经标着「资产」，这一行只需要回答「一共多少、怎么筛」。 */}
      <header className="pg-assets-head">
        <h1 className="pg-title">资产</h1>
        <p className="pg-assets-sub">{assets == null ? "…" : assets.length ? `${assetCountLabel(counts)} · ${fmtBytes(totalBytes)}` : "还没有内容"}</p>
        <span className="grow" />
        {counts.image > 0 && counts.video > 0 ? (
          <div className="pg-seg" role="tablist" aria-label="类型">
            {(
              [
                ["all", "全部", counts.image + counts.video],
                ["image", "图片", counts.image],
                ["video", "视频", counts.video],
              ] as Array<[AssetKind, string, number]>
            ).map(([id, label, n]) => (
              <button key={id} type="button" role="tab" className="pg-seg-btn" aria-selected={kind === id} onClick={() => setKind(id)}>
                {id !== "all" ? <PgIcon name={id === "video" ? "play" : "image"} size={12} /> : null}
                {label}
                <span className="pg-seg-n num">{n}</span>
              </button>
            ))}
          </div>
        ) : null}
        {models.length > 1 ? <PillMenu label="模型" align="right" value={model} options={[{ id: null, label: "全部", meta: String(inKind.length) }, ...models.map(([m, n]) => ({ id: m as string | null, label: m, meta: String(n) }))]} onChange={setModel} /> : null}
        {/* 一张图都没有时不摆筛选和搜索：对着空墙给一排筛选器只是噪音。 */}
        {assets?.length ? (
          <label className="search pg-search">
            <Icon name="search" size={13} />
            <input value={q} placeholder="搜提示词 / 会话 / 模型" onChange={(e) => setQ(e.target.value)} />
            {q ? (
              <button type="button" className="search-clear" aria-label="清空" onClick={() => setQ("")}>
                <Icon name="close" size={12} />
              </button>
            ) : null}
          </label>
        ) : null}
      </header>

      {error ? (
        <div className="pg-note">
          <ErrorNote error={error} onRetry={() => void reload()} />
        </div>
      ) : null}

      <div className="pg-assets-body">
        {assets == null ? (
          <div className="pg-wall">
            {Array.from({ length: 8 }, (_, i) => (
              <div key={i} className="pg-tile skeleton" />
            ))}
          </div>
        ) : assets.length === 0 ? (
          <div className="pg-empty">
            <span className="pg-empty-mark">
              <PgIcon name="gallery" size={22} />
            </span>
            <p className="pg-empty-title">这里会摆下你生成的每一张图</p>
            <p className="pg-empty-sub">在「图片」里描述一个画面，出来的图会自动收进这面墙，连同提示词、模型和规格。</p>
            <button type="button" className="btn btn-primary" style={{ marginTop: 10 }} onClick={onGoImages}>
              <PgIcon name="spark" size={14} />
              去出第一张图
            </button>
          </div>
        ) : shown.length === 0 ? (
          <div className="pg-empty">
            <p className="pg-empty-title">{kind === "video" ? "没有匹配的视频" : kind === "image" ? "没有匹配的图" : "没有匹配的内容"}</p>
            <p className="pg-empty-sub">换个词，或把类型 / 模型筛选放开。</p>
          </div>
        ) : (
          <div className="pg-wall">
            {shown.map((a, i) => (
              <Tile key={a.id} a={a} onOpen={() => setOpen(i)} />
            ))}
          </div>
        )}
      </div>

      {open != null && shown[open] ? (
        <Lightbox
          items={shown.map((a) => ({ id: a.id, mime: a.mime, prompt: a.prompt, model: a.model, width: a.width, height: a.height, size: a.size, bytes: a.bytes, createdAt: a.createdAt, threadId: a.threadId, threadTitle: a.threadTitle || (a.prompt ? a.prompt.slice(0, 40) : isVideoRef(a) ? "新视频" : "新图片") }))}
          index={open}
          onIndex={setOpen}
          onClose={() => setOpen(null)}
          onOpenThread={(tid) => {
            const hit = shown.find((a) => a.threadId === tid);
            onOpenThread(tid, hit && isVideoRef(hit) ? "video" : "image");
          }}
          onReveal={(id) => void playground.revealImage(id).catch(setError)}
          onDelete={async (id) => {
            await remove(id);
            const left = shown.length - 1;
            if (left <= 0) setOpen(null);
            else setOpen(Math.min(open, left - 1));
          }}
        />
      ) : null}
    </div>
  );
}

type AssetKind = "all" | "image" | "video";

/** 「12 张 · 3 段」——只有一种时不带另一种的零。 */
function assetCountLabel(c: { image: number; video: number }): string {
  const parts: string[] = [];
  if (c.image) parts.push(`${c.image} 张图`);
  if (c.video) parts.push(`${c.video} 段视频`);
  return parts.join(" · ");
}

function Tile({ a, onOpen }: { a: Asset; onOpen: () => void }) {
  const [gone, setGone] = useState(false);
  return (
    <button type="button" className="pg-tile" onClick={onOpen} title={a.prompt ?? undefined} aria-label={a.prompt ?? a.threadTitle}>
      {gone ? (
        <span className="pg-fig-gone">
          <PgIcon name="image" size={18} />
          <span>文件已不在本机</span>
        </span>
      ) : isVideoRef(a) ? (
        <video className="pg-tile-img" src={imageSrc(a.id)} muted playsInline preload="metadata" onError={() => setGone(true)} />
      ) : (
        <img className="pg-tile-img" src={imageSrc(a.id)} alt={a.prompt ?? ""} loading="lazy" decoding="async" onError={() => setGone(true)} />
      )}
      <span className="pg-tile-cap">
        <span className="pg-tile-prompt">{a.prompt ?? a.threadTitle}</span>
        <span className="pg-tile-meta num">
          <span className="mono truncate">{a.model ?? "—"}</span>
          <span className="pg-sep">·</span>
          {a.width && a.height ? `${a.width}×${a.height}` : a.size ?? "—"}
        </span>
      </span>
    </button>
  );
}
