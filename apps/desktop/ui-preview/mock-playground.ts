/**
 * 游乐场的 fixtures：两个对话会话、一个图片会话，发一句会流式回字，出图会等两秒回两张
 * 占位图。全在内存里，刷新即还原。图片地址走 `convertFileSrc(id, "nexus-image")`，
 * 预览里把它换成一张按 id 上色的 SVG。
 */
import { PLAYGROUND_CHAT_EVENT, type Asset, type ChatAttachment, type ImageRef, type ImageRequest, type Kind, type Message, type Thread, type ThreadDetail, type ThreadSummary } from "../src/ipc/playground";
import type { TryEvent, TryFrame } from "../src/ipc/models";
import { emitMock } from "./mock-event";

const q = new URLSearchParams(location.search);
const EMPTY = q.get("empty") === "1";
const NOW = Date.now();
const iso = (msAgo: number) => new Date(NOW - msAgo).toISOString();
const delay = <T>(v: T, ms = 120) => new Promise<T>((r) => setTimeout(() => r(v), ms));

let seq = 0;
const nid = () => `pg-${(seq += 1).toString(36)}`;

const threads: Thread[] = [];
const messages = new Map<string, Message[]>();

function thread(kind: Kind, title: string, model: string, msAgo: number): Thread {
  const t: Thread = { id: nid(), kind, title, model, createdAt: iso(msAgo + 3_600_000), updatedAt: iso(msAgo) };
  threads.push(t);
  messages.set(t.id, []);
  return t;
}

function msg(t: Thread, role: Message["role"], content: string, extra: Partial<Message> = {}): Message {
  const list = messages.get(t.id)!;
  const m: Message = {
    id: nid(),
    threadId: t.id,
    seq: list.length + 1,
    role,
    content,
    thinking: null,
    model: role === "assistant" ? t.model : null,
    routed: null,
    usage: null,
    error: null,
    durationMs: null,
    ttftMs: null,
    createdAt: iso(60_000 * (20 - list.length)),
    images: [],
    ...extra,
  };
  list.push(m);
  return m;
}

function image(m: Message, size: string, bytes: number): ImageRef {
  const [w = 1024, h = 1024] = size.split("x").map(Number);
  const img: ImageRef = { id: nid(), messageId: m.id, mime: "image/png", width: w, height: h, bytes, size, createdAt: m.createdAt };
  m.images.push(img);
  return img;
}

if (!EMPTY) {
  const a = thread("chat", "用 Rust 写一个 LRU cache", "claude-sonnet-5", 12 * 60_000);
  msg(a, "user", "用 Rust 写一个线程安全的 LRU cache，说明取舍。");
  msg(
    a,
    "assistant",
    [
      "下面是一个最小可用的实现，核心是 `HashMap` + 双向链表；线程安全靠外层 `Mutex`：",
      "",
      "```rust",
      "use std::collections::HashMap;",
      "use std::sync::Mutex;",
      "",
      "pub struct Lru<K, V> {",
      "    cap: usize,",
      "    map: Mutex<HashMap<K, V>>,",
      "}",
      "```",
      "",
      "取舍：",
      "- **一把大锁**最简单，读多写少时瓶颈明显；",
      "- 分片（sharding）能把锁竞争摊开，代价是 LRU 变成「每片各自 LRU」。",
      "",
      "> 生产里建议直接用 `lru` crate，除非你要教学。",
    ].join("\n"),
    { thinking: "用户要的是取舍说明，先给最小实现再讲锁的粒度。", routed: "claude-sonnet-5", usage: { promptTokens: 412, completionTokens: 286 }, durationMs: 6_820, ttftMs: 640 },
  );
  // 一条把 Markdown 渲染器的花样摆全的回复：表格对齐、任务列表、嵌套引用、转义、
  // 几种语言的高亮，外加能切到「预览」页签的 html / svg 围栏。
  msg(
    a,
    "assistant",
    [
      "再补几样能直接看的东西。",
      "",
      "## 分片数怎么选",
      "",
      "| 场景 | 分片数 | 命中率 | 说明 |",
      "|:-----|-------:|:------:|:-----|",
      "| 单线程 | `1` | 92% | 没有锁竞争，链表也最好写 |",
      "| 八核读多写少 | `16` | 89% | 每片各自 LRU，边缘键会被提前淘汰 |",
      "| 写密集 | `64` | 81% | 分片收益递减，瓶颈转到分配器 |",
      "",
      "落地清单：",
      "",
      "- [x] `HashMap` + 侵入式双向链表",
      "- [x] 外面先包一把 `Mutex`，跑通再说",
      "- [ ] 分片：`shards[hash(k) % N]`",
      "- [ ] 用 `criterion` 压一遍，跟 `lru` crate 对比",
      "",
      "> 上面「命中率」那一栏：",
      "> > 是拿我们自己的线上负载重放出来的。换一种 key 分布数字会差很多，别当结论用。",
      "",
      "取值路径长这样（`\\*self` 那行是唯一会动堆的地方）：",
      "",
      "```ts",
      "type Shard<K, V> = { map: Map<K, V>; cap: number };",
      "",
      "/** 命中就把这个键挪到队尾——Map 的插入顺序就是我们的 LRU 顺序。 */",
      "export function get<K, V>(s: Shard<K, V>, key: K): V | undefined {",
      "  const hit = s.map.get(key); // 0x1 次哈希",
      "  if (hit === undefined) return undefined;",
      "  s.map.delete(key);",
      "  s.map.set(key, hit);",
      "  return hit;",
      "}",
      "```",
      "",
      "```python",
      "from dataclasses import dataclass",
      "",
      "@dataclass",
      "class Shard:",
      "    cap: int = 1_024",
      "    hits: float = 0.0",
      "",
      "    def ratio(self) -> str:",
      '        """只在压测报告里用，别放进热路径。"""',
      '        return f"{self.hits * 100:.1f}%"',
      "```",
      "",
      "分片占用画出来更好看，下面这页和这张图都收成了卡片，点开在右侧预览：",
      "",
      "```html",
      "<!DOCTYPE html>",
      "<html>",
      "<head>",
      "  <meta charset=\"utf-8\">",
      "  <title>分片占用一览</title>",
      "  <style>",
      "    body { margin: 0; padding: 18px; background: #0a0c0d; color: #e9eef0;",
      "           font: 13px/1.6 system-ui, sans-serif; }",
      "    .row { display: flex; gap: 8px; }",
      "    .cell { flex: 1; padding: 10px 12px; border-radius: 8px;",
      "            background: #171b1e; border: 1px solid #232a2e; }",
      "    .cell b { color: #2dd4a0; font-variant-numeric: tabular-nums; }",
      "    button { margin-top: 12px; padding: 5px 12px; border-radius: 7px;",
      "             border: 1px solid #232a2e; background: #111517; color: #e9eef0; }",
      "  </style>",
      "</head>",
      "<body>",
      '  <div class="row">',
      '    <div class="cell">shard 0 <b>128</b> / 256</div>',
      '    <div class="cell">shard 1 <b>241</b> / 256</div>',
      '    <div class="cell">shard 2 <b>57</b> / 256</div>',
      "  </div>",
      "  <button onclick=\"document.querySelector('.row').style.opacity = 0.35\">淘汰一轮</button>",
      "</body>",
      "</html>",
      "```",
      "",
      "```svg",
      '<svg viewBox="0 0 320 110" xmlns="http://www.w3.org/2000/svg">',
      '  <rect width="320" height="110" rx="10" fill="#111517" stroke="#232a2e"/>',
      '  <g fill="#2dd4a0">',
      '    <rect x="28" y="46" width="42" height="40" rx="4"/>',
      '    <rect x="94" y="22" width="42" height="64" rx="4"/>',
      '    <rect x="160" y="66" width="42" height="20" rx="4" opacity="0.55"/>',
      '    <rect x="226" y="34" width="42" height="52" rx="4" opacity="0.8"/>',
      "  </g>",
      '  <g fill="#8a9499" font-family="ui-monospace, monospace" font-size="10">',
      '    <text x="40" y="100">s0</text><text x="106" y="100">s1</text>',
      '    <text x="172" y="100">s2</text><text x="238" y="100">s3</text>',
      "  </g>",
      "</svg>",
      "```",
    ].join("\n"),
    { routed: "claude-sonnet-5", usage: { promptTokens: 704, completionTokens: 512 }, durationMs: 9_140, ttftMs: 580 },
  );
  msg(a, "user", "分片版本怎么写？");
  msg(a, "assistant", "", { error: "ERROR_RATE_LIMITED: 上游限流，稍后再试。", durationMs: 1_240 });

  const b = thread("chat", "把这段话改写得更简洁", "gpt-5.6-sol", 3 * 3_600_000);
  msg(b, "user", "把下面这段话改写得更简洁：我们公司在过去的一年当中，一直都在持续不断地努力……");
  msg(b, "assistant", "过去一年，我们一直在努力。", { routed: "gpt-5.6-sol", usage: { promptTokens: 58, completionTokens: 12 }, durationMs: 1_930, ttftMs: 410 });

  const c = thread("image", "雨夜的东京街头，霓虹倒影", "gpt-image-1", 40 * 60_000);
  msg(c, "user", "雨夜的东京街头，霓虹倒影，电影感，35mm");
  const r1 = msg(c, "assistant", "", { durationMs: 18_400 });
  image(r1, "1024x1024", 1_482_113);
  image(r1, "1024x1024", 1_390_004);
  msg(c, "user", "同样的场景，换成清晨，雾气");
  const r2 = msg(c, "assistant", "", { durationMs: 21_100 });
  image(r2, "1536x1024", 1_902_331);

  const d = thread("image", "极简主义海报：一颗孤独的行星", "seedream-5", 26 * 3_600_000);
  msg(d, "user", "极简主义海报：一颗孤独的行星，大面积留白");
  const r3 = msg(d, "assistant", "", { durationMs: 34_000 });
  image(r3, "2048x2048", 3_412_990);
  image(r3, "2048x2048", 3_280_114);
  image(r3, "2048x2048", 3_501_770);
  msg(d, "user", "换成竖版，加一行很小的标题");
  const r4 = msg(d, "assistant", "", { durationMs: 29_700 });
  image(r4, "1728x2304", 2_988_002);
  image(r4, "1728x2304", 3_104_556);
  msg(d, "user", "再来一张夜景版");
  msg(d, "assistant", "", { error: "没有权限（403）：这个 Cursor 号没有生图权限（上游只对 Developer 或 Sand 通道放行，号还得有 Bot 额度）。换一个有 Bot 额度的号，或切到云端中转。", durationMs: 1_100 });
}

function summary(t: Thread): ThreadSummary {
  const list = messages.get(t.id) ?? [];
  const last = [...list].reverse().find((m) => m.content);
  const cover = [...list].reverse().find((m) => m.images.length)?.images.at(-1)?.id ?? null;
  return { ...t, messageCount: list.length, preview: last ? last.content.slice(0, 80) : null, coverImageId: cover };
}

function detail(id: string): ThreadDetail {
  const t = threads.find((x) => x.id === id);
  if (!t) throw { code: "invalid_input", message: "这个会话已经不存在了。", hint: "刷新列表再试。" };
  return { thread: t, messages: messages.get(t.id) ?? [] };
}

function touch(t: Thread, title?: string) {
  if (title && !t.title) t.title = (title.split("\n")[0] ?? title).slice(0, 40);
  t.updatedAt = new Date().toISOString();
}

async function fakeChat(requestId: string, t: Thread, prompt: string | null, attachments: ChatAttachment[] = []): Promise<Message> {
  const emit = (f: TryEvent) => emitMock(PLAYGROUND_CHAT_EVENT, { id: requestId, ...f } satisfies TryFrame);
  const list = messages.get(t.id)!;
  if (prompt != null) {
    const u = msg(t, "user", prompt);
    // 附件在真实链路里由 Rust 落盘后挂到 user 消息上；预览里只造几条记录，图按 id 上色。
    for (const a of attachments) image(u, "1024x1024", Math.floor((a.dataBase64.length * 3) / 4));
    touch(t, prompt);
  } else if (list.at(-1)?.role === "assistant") {
    list.pop();
  }
  const lastUser = [...list].reverse().find((m) => m.role === "user")?.content ?? "";
  const routed = t.model === "auto" ? "composer-2.5-fast" : t.model;
  const started = Date.now();
  await delay(null, 400);
  emit({ kind: "routed", model: routed });
  for (const s of ["用户在问：", lastUser.slice(0, 12), "。先想清楚再答。"]) {
    await delay(null, 150);
    emit({ kind: "thinking", text: s });
  }
  // 提到「页面 / html」就流式吐一个 artifact：卡片、右侧预览区那条链路也能在预览里走一遍。
  const wantsArtifact = /html|页面|artifact/i.test(lastUser);
  const answer = wantsArtifact
    ? [
        "好，一个最小的心跳页面，收成了下面这张卡片 —— 点开在右侧看效果：",
        "",
        "```html",
        "<!DOCTYPE html>",
        "<html>",
        "<head>",
        '  <meta charset="utf-8">',
        "  <title>心跳监测</title>",
        "  <style>",
        "    body { margin: 0; min-height: 100vh; display: grid; place-items: center;",
        "           background: #0a0c0d; color: #e9eef0; font: 14px/1.7 system-ui, sans-serif; }",
        "    .beat { width: 14px; height: 14px; border-radius: 999px; background: #2dd4a0;",
        "            animation: b 1.2s ease-in-out infinite; }",
        "    @keyframes b { 50% { transform: scale(1.6); opacity: 0.5; } }",
        "  </style>",
        "</head>",
        "<body>",
        '  <div class="beat"></div>',
        "</body>",
        "</html>",
        "```",
      ].join("\n")
    : `这是 **${routed}** 在预览里的回答。你问的是「${lastUser.slice(0, 30)}」。\n\n- 第一点：预览数据全是假的；\n- 第二点：流式、思考、用量与耗时都是真实链路会给的东西。\n\n\`\`\`ts\nconst ok = true;\n\`\`\``;
  let first = 0;
  for (const piece of answer.match(/.{1,5}/gs) ?? []) {
    await delay(null, 40);
    if (!first) first = Date.now();
    emit({ kind: "delta", text: piece });
  }
  await delay(null, 120);
  emit({ kind: "done", finish: "stop", usage: { promptTokens: 61, completionTokens: 88 } });
  const reply = msg(t, "assistant", answer, {
    thinking: `用户在问：${lastUser.slice(0, 12)}。先想清楚再答。`,
    routed,
    usage: { promptTokens: 61, completionTokens: 88 },
    durationMs: Date.now() - started,
    ttftMs: first - started,
  });
  touch(t);
  return reply;
}

async function fakeImage(t: Thread, req: ImageRequest): Promise<Message> {
  msg(t, "user", req.prompt);
  touch(t, req.prompt);
  const started = Date.now();
  await delay(null, 2_200);
  const reply = msg(t, "assistant", /seedream/i.test(t.model) ? "" : `改写后的提示词：${req.prompt}，高细节，电影感光线。`, { durationMs: Date.now() - started });
  for (let i = 0; i < Math.max(1, req.n); i += 1) image(reply, req.size ?? "1024x1024", 1_200_000 + i * 90_000);
  touch(t);
  return reply;
}

/** 预览里的图：按 id 取色的一张 SVG，画幅跟着记录里的宽高走。 */
export function fakeImageSrc(id: string): string {
  const all = [...messages.values()].flat().flatMap((m) => m.images);
  const img = all.find((i) => i.id === id);
  const w = img?.width ?? 1024;
  const h = img?.height ?? 1024;
  let hash = 0;
  for (const ch of id) hash = (hash * 31 + ch.charCodeAt(0)) >>> 0;
  const hue = hash % 360;
  const svg = `<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 ${w} ${h}"><defs><linearGradient id="g" x1="0" y1="0" x2="1" y2="1"><stop offset="0" stop-color="hsl(${hue},55%,28%)"/><stop offset="1" stop-color="hsl(${(hue + 50) % 360},60%,12%)"/></linearGradient></defs><rect width="${w}" height="${h}" fill="url(#g)"/><circle cx="${w * 0.7}" cy="${h * 0.3}" r="${Math.min(w, h) * 0.12}" fill="hsl(${(hue + 120) % 360},70%,70%)" opacity="0.8"/></svg>`;
  return `data:image/svg+xml;utf8,${encodeURIComponent(svg)}`;
}

/** 认得的命令就处理并返回 Promise；不是游乐场的命令返回 undefined。 */
export function handlePlayground(cmd: string, args?: Record<string, unknown>): Promise<unknown> | undefined {
  switch (cmd) {
    case "playground_threads": {
      const kind = args?.kind as Kind | null | undefined;
      const list = threads.filter((t) => !kind || t.kind === kind).sort((a, b) => b.updatedAt.localeCompare(a.updatedAt)).map(summary);
      return delay(list, 160);
    }
    case "playground_thread":
      return delay(detail(String(args?.id)), 140);
    case "playground_thread_create": {
      const t: Thread = { id: nid(), kind: args?.kind as Kind, title: "", model: String(args?.model), createdAt: new Date().toISOString(), updatedAt: new Date().toISOString() };
      threads.push(t);
      messages.set(t.id, []);
      return delay(t, 80);
    }
    case "playground_thread_rename": {
      const t = detail(String(args?.id)).thread;
      t.title = String(args?.title ?? "").trim();
      t.updatedAt = new Date().toISOString();
      return delay(t, 80);
    }
    case "playground_thread_set_target": {
      const t = detail(String(args?.id)).thread;
      t.model = String(args?.model);
      return delay(t, 80);
    }
    case "playground_thread_delete": {
      const i = threads.findIndex((t) => t.id === args?.id);
      if (i >= 0) {
        messages.delete(String(args?.id));
        threads.splice(i, 1);
      }
      return delay(undefined, 120);
    }
    case "playground_message_delete": {
      for (const list of messages.values()) {
        const i = list.findIndex((m) => m.id === args?.id);
        if (i >= 0) list.splice(i, 1);
      }
      return delay(undefined, 80);
    }
    case "playground_active":
      return delay(null, 40);
    case "playground_chat_send": {
      const t = detail(String(args?.threadId)).thread;
      return fakeChat(String(args?.requestId), t, (args?.prompt as string | null) ?? null, (args?.attachments as ChatAttachment[] | undefined) ?? []);
    }
    case "playground_stop":
      return delay(true, 40);
    case "playground_image_generate": {
      const t = detail(String(args?.threadId)).thread;
      return fakeImage(t, args?.request as ImageRequest);
    }
    case "playground_image_reveal":
      return delay(undefined, 40);
    case "playground_assets": {
      const out: Asset[] = [];
      for (const t of threads) {
        const list = messages.get(t.id) ?? [];
        list.forEach((m, i) => {
          const prompt = [...list.slice(0, i)].reverse().find((x) => x.role === "user")?.content ?? null;
          for (const img of m.images) out.push({ ...img, threadId: t.id, threadTitle: t.title, model: m.model, prompt });
        });
      }
      out.sort((a, b) => b.createdAt.localeCompare(a.createdAt));
      return delay(out, 180);
    }
    case "playground_image_delete": {
      for (const list of messages.values()) {
        for (const m of list) {
          const i = m.images.findIndex((img) => img.id === args?.id);
          if (i >= 0) m.images.splice(i, 1);
        }
      }
      return delay(undefined, 80);
    }
    default:
      return undefined;
  }
}
