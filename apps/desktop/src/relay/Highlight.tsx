/**
 * 配置片段的轻量着色。
 *
 * 不引高亮库：这里只有五种形状的文本（JSON / TOML / shell / Python / JS），而且都是我们自己
 * 生成的，一个正则就能把「字串 / 键 / 数字 / 关键字 / 网址 / 命令行开关」分出来。着色的目的
 * 只有一个 —— 让人一眼找到自己要改的那几个值（地址、钥匙、模型），所以字串最亮，其余都退后。
 *
 * 钥匙的占位符单独一种颜色：它长得像钥匙，但**不是**，得让人看出「这里还没填」。
 */
import type { ReactNode } from "react";
import { KEY_PLACEHOLDER } from "./snippets";

const TOKEN =
  /("(?:[^"\\]|\\.)*"|'(?:[^'\\]|\\.)*')|(https?:\/\/[^\s"'\\)]+)|((?:^|(?<=\s))--?[A-Za-z][\w-]*)|([A-Za-z_$][\w.$-]*(?=\s*[=:]))|(\b\d+(?:\.\d+)?\b)|(\b(?:true|false|null|None|import|from|const|await|new|export|print|async|def|return|curl(?:\.exe)?|cursor-agent)\b)/g;

const CLASS = ["hl-str", "hl-url", "hl-flag", "hl-key", "hl-num", "hl-kw"] as const;

/** 一段文本里的钥匙占位符换成带样式的 span；其余原样。 */
function withPlaceholder(text: string, key: string): ReactNode {
  if (!text.includes(KEY_PLACEHOLDER)) return text;
  const parts = text.split(KEY_PLACEHOLDER);
  return parts.flatMap((p, i) =>
    i < parts.length - 1
      ? [
          p,
          <span key={`${key}-ph-${i}`} className="hl-ph">
            {KEY_PLACEHOLDER}
          </span>,
        ]
      : [p],
  );
}

function highlightLine(line: string, lineKey: string): ReactNode[] {
  const trimmed = line.trimStart();
  if (trimmed.startsWith("#") || trimmed.startsWith("//")) {
    return [
      <span key={lineKey} className="hl-cmt">
        {line}
      </span>,
    ];
  }
  // TOML 的 [section]
  if (/^\s*\[[^\]]+\]\s*$/.test(line)) {
    return [
      <span key={lineKey} className="hl-sec">
        {line}
      </span>,
    ];
  }
  const out: ReactNode[] = [];
  let last = 0;
  let n = 0;
  for (const m of line.matchAll(TOKEN)) {
    const start = m.index ?? 0;
    if (start > last) out.push(withPlaceholder(line.slice(last, start), `${lineKey}-t${n}`));
    const idx = CLASS.findIndex((_, i) => m[i + 1] !== undefined);
    const cls = CLASS[idx] ?? "";
    out.push(
      <span key={`${lineKey}-${n}`} className={cls}>
        {withPlaceholder(m[0], `${lineKey}-${n}`)}
      </span>,
    );
    last = start + m[0].length;
    n += 1;
  }
  if (last < line.length) out.push(withPlaceholder(line.slice(last), `${lineKey}-tail`));
  return out;
}

export function Highlight({ code }: { code: string }) {
  const lines = code.split("\n");
  return (
    <>
      {lines.map((line, i) => (
        <span key={i} className="hl-line">
          {highlightLine(line, `l${i}`)}
          {i < lines.length - 1 ? "\n" : null}
        </span>
      ))}
    </>
  );
}
