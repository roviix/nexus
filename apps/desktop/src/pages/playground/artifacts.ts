/**
 * 对话里的 artifact：一段可预览的代码（HTML 页面 / SVG 图）。
 *
 * 它在消息流里不再内嵌预览，而是一张卡片；点开之后，右侧预览区看效果或原始代码。
 * 一块 artifact 的身份是 `(messageId, codeIndex)` —— 哪条消息里的第几个围栏代码块。
 * 序号把不可预览的块也算上，流式途中新增一个普通代码块才不会把后面 artifact 的坐标
 * 全部顶错。流式中的那一轮还没落库、没有消息 id，用 `LIVE_ARTIFACT` 顶替。
 */
import type { Message } from "../../ipc/playground";
import { artifactTitle, parseBlocks, previewKind } from "./Markdown";

/** 流式中的那一轮用的临时消息 id；落库后由工作台换成真实 id。 */
export const LIVE_ARTIFACT = "live";

export interface ArtifactRef {
  messageId: string;
  codeIndex: number;
}

export interface Artifact {
  ref: ArtifactRef;
  kind: "html" | "svg";
  lang: string;
  title: string;
  body: string;
  /** 围栏还没收尾（流式途中）：预览没意义，预览区先给代码。 */
  open: boolean;
  lines: number;
  bytes: number;
}

export function sameArtifact(a: ArtifactRef | null, b: ArtifactRef | null): boolean {
  return Boolean(a && b && a.messageId === b.messageId && a.codeIndex === b.codeIndex);
}

const encoder = new TextEncoder();

/** 一条消息里的所有 artifact，按出现顺序。 */
export function artifactsInText(messageId: string, text: string): Artifact[] {
  const out: Artifact[] = [];
  let codeIndex = 0;
  for (const b of parseBlocks(text)) {
    if (b.kind !== "code") continue;
    const kind = previewKind(b.lang, b.body);
    if (kind) {
      out.push({
        ref: { messageId, codeIndex },
        kind,
        lang: b.lang,
        title: artifactTitle(kind, b.body),
        body: b.body,
        open: Boolean(b.open),
        lines: b.body ? b.body.split("\n").length : 0,
        bytes: encoder.encode(b.body).length,
      });
    }
    codeIndex += 1;
  }
  return out;
}

/**
 * 整个会话的 artifact，按对话顺序 —— 预览区的「上一个 / 下一个」按它翻。
 * 只有回复会渲染 Markdown（用户的话是气泡纯文本），所以只从回复里收。
 * 流式中的那一轮挂在最后。
 */
export function collectArtifacts(messages: Message[], liveText?: string | null): Artifact[] {
  const out: Artifact[] = [];
  for (const m of messages) {
    if (m.role !== "assistant" || !m.content) continue;
    out.push(...artifactsInText(m.id, m.content));
  }
  if (liveText) out.push(...artifactsInText(LIVE_ARTIFACT, liveText));
  return out;
}
