/**
 * 批量粘贴的那一页：一个大输入框 + 解析预览。
 *
 * **先预览再确认。** 清单来自各种地方、格式极乱，用户得先看清「会收哪 12 个、剩下
 * 3 个为什么不收」再按导入 —— 一步到位的导入出了偏差只能事后收拾。
 *
 * 解析在 Rust 侧做（`nexus_accounts::import`），前端只拿到「邮箱 + 收不收 + 原因」，
 * 明文凭证从没出过 Rust。同一个框也吃本应用导出的 `accounts.json` 全文。
 */
import { useState } from "react";
import type { ImportPreview } from "../../ipc/types";
import { Banner, Spinner, Tag } from "../../ui/primitives";

const SAMPLE = `支持这些写法，混在一起也行：

a@example.com----邮箱密码----Cursor密码
b@example.com----Cursor密码----2026-08-07 01:01
c@example.com----eyJhbGciOi…（refresh token）
d@example.com----crsr_…（User API Key，可查基础用量）

第1个：d@example.com
登录密码：xxx  邮箱密码：yyy

也可以把导出的 accounts.json 整份粘进来。`;

export function ImportPanel({
  text,
  parsing,
  preview,
  onChange,
}: {
  text: string;
  parsing: boolean;
  preview: ImportPreview | null;
  onChange: (next: string) => void;
}) {
  return (
    <>
      <textarea
        className="textarea imp-text"
        value={text}
        autoFocus
        spellCheck={false}
        placeholder={SAMPLE}
        onChange={(e) => onChange(e.target.value)}
      />

      {parsing ? (
        <div className="row faint tiny">
          <Spinner /> 正在解析…
        </div>
      ) : null}

      {preview ? <PreviewPanel preview={preview} /> : null}
    </>
  );
}

function PreviewPanel({ preview }: { preview: ImportPreview }) {
  const [onlyAccepted, setOnlyAccepted] = useState(false);
  const rows = onlyAccepted ? preview.rows.filter((r) => r.accepted) : preview.rows;

  if (preview.rows.length === 0 && preview.skipped.length === 0) {
    return <Banner tone="warn" title="没解析出账号" hint="每行至少要有一个邮箱地址。" />;
  }

  return (
    <div className="stack" style={{ gap: 8 }}>
      <div className="row-between">
        <div className="row" style={{ gap: 6 }}>
          <Tag tone="ok">收下 {preview.acceptedCount}</Tag>
          {preview.rejectedCount > 0 ? <Tag tone="warn">跳过 {preview.rejectedCount}</Tag> : null}
          {preview.skipped.length > 0 ? <Tag tone="bad">看不懂 {preview.skipped.length} 行</Tag> : null}
        </div>
        {preview.rejectedCount > 0 ? (
          <button type="button" className="linkish" onClick={() => setOnlyAccepted(!onlyAccepted)}>
            {onlyAccepted ? "显示全部" : "只看会收下的"}
          </button>
        ) : null}
      </div>

      <div className="imp-rows">
        {rows.map((r) => (
          <div key={r.email} className={r.accepted ? "imp-row" : "imp-row is-out"}>
            <span className="imp-mark">{r.accepted ? "✓" : "—"}</span>
            <span className="grow truncate selectable">{r.email}</span>
            <span className="row" style={{ gap: 4, flex: "none" }}>
              {r.hasRefresh ? <Tag tone="ok">token</Tag> : null}
              {!r.hasRefresh && r.hasAccess ? <Tag tone="warn">仅会话</Tag> : null}
              {r.hasApiKey ? <Tag>API Key</Tag> : null}
              {r.hasPassword ? <Tag>密码</Tag> : null}
              {r.hasEmailPassword ? <Tag>邮箱密码</Tag> : null}
            </span>
            {!r.accepted ? (
              <span className="imp-reason truncate" title={r.reason}>
                {r.reason}
              </span>
            ) : null}
          </div>
        ))}
      </div>

      {preview.skipped.length > 0 ? (
        <details className="imp-skipped">
          <summary>{preview.skipped.length} 行没认出来，原样列在这里</summary>
          <pre>{preview.skipped.join("\n")}</pre>
        </details>
      ) : null}
    </div>
  );
}
