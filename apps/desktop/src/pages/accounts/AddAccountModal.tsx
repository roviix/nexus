/**
 * 添加账号：一个弹窗，两种填法 —— 单个填表 / 整段粘贴。
 *
 * 合成一个入口而不是页头摆两个按钮：两者做的是同一件事（把号收进来），差别只在
 * 手里拿的是一条还是一堆。两个模式各自记着自己的草稿，切过去再切回来不会丢。
 *
 * 托管门槛（ARCHITECTURE §5.1）：邮箱 + (refresh_token | Cursor 密码 | session token | crsr_ API Key)。只有邮箱密码的
 * 不收 —— 那登不进 Cursor。表单把这条规则直接摆在「凭证」一节的标题旁，而不是等按下保存才报错。
 * 只填 session token 的号是「仅会话」：有效期内能查用量、能切进 Cursor，到期得重新粘；标题旁会直接说出来。
 * 只填 crsr_ 的号能查基础用量，不能切号。
 */
import { useCallback, useEffect, useRef, useState, type ReactNode } from "react";
import { accounts } from "../../ipc/api";
import type { ImportPreview } from "../../ipc/types";
import { ErrorNote, Icon, Modal, Spinner } from "../../ui/primitives";
import { ImportPanel } from "./ImportPanel";

type Mode = "single" | "bulk";

interface Pane {
  body: ReactNode;
  submit: ReactNode;
}

export function AddAccountModal({
  onClose,
  onAdded,
  onImported,
}: {
  onClose: () => void;
  /** 单个添加成功。 */
  onAdded: () => Promise<void>;
  /** 批量导入成功，参数是导入的条数。 */
  onImported: (count: number) => Promise<void>;
}) {
  const [mode, setMode] = useState<Mode>("single");
  const single = useSinglePane(onAdded);
  const bulk = useBulkPane(onImported);
  const pane = mode === "single" ? single : bulk;

  return (
    <Modal
      title="添加账号"
      onClose={onClose}
      footer={
        <>
          <button type="button" className="btn" onClick={onClose}>
            取消
          </button>
          {pane.submit}
        </>
      }
    >
      <div className="stack" style={{ gap: 16 }}>
        <div className="tabs tabs-block">
          <button type="button" className="tab" aria-selected={mode === "single"} onClick={() => setMode("single")}>
            单个填写
          </button>
          <button type="button" className="tab" aria-selected={mode === "bulk"} onClick={() => setMode("bulk")}>
            批量粘贴
          </button>
        </div>
        {pane.body}
      </div>
    </Modal>
  );
}

/* ── 单个 ─────────────────────────────────────────────────────────────────── */

const EMPTY_FORM = {
  email: "",
  refreshToken: "",
  sessionToken: "",
  apiKey: "",
  cursorPassword: "",
  emailPassword: "",
  recoveryEmail: "",
  note: "",
};

function useSinglePane(onDone: () => Promise<void>): Pane {
  const [form, setForm] = useState(EMPTY_FORM);
  const [error, setError] = useState<unknown>(null);
  const [saving, setSaving] = useState(false);

  const set = (k: keyof typeof form) => (value: string) => setForm((f) => ({ ...f, [k]: value }));

  const emailOk = /\S+@\S+\.\S+/.test(form.email.trim());
  const longLived = Boolean(form.refreshToken.trim() || form.cursorPassword.trim());
  const sessionOnly = !longLived && Boolean(form.sessionToken.trim());
  const apiKeyRaw = form.apiKey.trim();
  const apiKeyOnly = !longLived && !sessionOnly && apiKeyRaw.toLowerCase().startsWith("crsr_");
  const qualified = longLived || sessionOnly || apiKeyOnly;
  const credHint = longLived
    ? "已满足"
    : sessionOnly
      ? "仅会话 · 到期需重新粘"
      : apiKeyOnly
        ? "仅 API Key · 可查基础用量，不能切号"
        : apiKeyRaw
          ? "API Key 须 crsr_ 开头"
          : "至少填一项";

  async function save() {
    setSaving(true);
    setError(null);
    try {
      await accounts.add({
        email: form.email.trim(),
        refreshToken: form.refreshToken.trim() || undefined,
        accessToken: form.sessionToken.trim() || undefined,
        apiKey: form.apiKey.trim() || undefined,
        cursorPassword: form.cursorPassword || undefined,
        emailPassword: form.emailPassword || undefined,
        recoveryEmail: form.recoveryEmail.trim() || undefined,
        note: form.note.trim() || undefined,
      });
      await onDone();
    } catch (err) {
      setError(err);
    } finally {
      setSaving(false);
    }
  }

  const body = (
    <form
      className="stack"
      style={{ gap: 18 }}
      onSubmit={(e) => {
        e.preventDefault();
        if (emailOk && qualified && !saving) void save();
      }}
    >
      <ErrorNote error={error} />

      <div className="fsect">
        <Field id="acc-email" label="邮箱">
          <input
            id="acc-email"
            className="input"
            autoFocus
            autoComplete="off"
            value={form.email}
            onChange={(e) => set("email")(e.target.value)}
            placeholder="someone@example.com"
          />
        </Field>
      </div>

      <div className="fsect">
        <div className="fsect-cap">
          <span>凭证</span>
          <span className={longLived ? "fsect-state is-ok" : "fsect-state"}>
            {credHint}
          </span>
        </div>
        <Field id="acc-rt" label="refresh_token">
          <input
            id="acc-rt"
            className="input mono"
            autoComplete="off"
            spellCheck={false}
            value={form.refreshToken}
            onChange={(e) => set("refreshToken")(e.target.value)}
            placeholder="有就填：能直接查用量、能切进 Cursor、能进网关接力"
          />
        </Field>
        <Field id="acc-pw" label="Cursor 密码">
          <SecretInput id="acc-pw" value={form.cursorPassword} onChange={set("cursorPassword")} />
        </Field>
        <Field id="acc-st" label="session token">
          <input
            id="acc-st"
            className="input mono"
            autoComplete="off"
            spellCheck={false}
            value={form.sessionToken}
            onChange={(e) => set("sessionToken")(e.target.value)}
            placeholder="user_xxx::eyJ… 或裸 JWT；没有上面两项时靠它，几小时到几天过期"
          />
        </Field>
        <Field id="acc-key" label="crsr_ API Key">
          <input
            id="acc-key"
            className="input mono"
            autoComplete="off"
            spellCheck={false}
            value={form.apiKey}
            onChange={(e) => set("apiKey")(e.target.value)}
            placeholder="crsr_…；session 过期后还能查基础花费，不能切进 Cursor"
          />
        </Field>
      </div>

      <div className="fsect">
        <div className="fsect-cap">
          <span>可选</span>
        </div>
        <div className="grid-2">
          <Field id="acc-epw" label="邮箱密码">
            <SecretInput id="acc-epw" value={form.emailPassword} onChange={set("emailPassword")} />
          </Field>
          <Field id="acc-rec" label="辅助邮箱">
            <input
              id="acc-rec"
              className="input"
              autoComplete="off"
              value={form.recoveryEmail}
              onChange={(e) => set("recoveryEmail")(e.target.value)}
            />
          </Field>
        </div>
        <Field id="acc-note" label="备注">
          <input
            id="acc-note"
            className="input"
            value={form.note}
            onChange={(e) => set("note")(e.target.value)}
            placeholder="这个号是谁的、用来干什么"
          />
        </Field>
      </div>
      {/* 让回车能提交。 */}
      <button type="submit" hidden />
    </form>
  );

  const submit = (
    <button
      type="button"
      className="btn btn-primary"
      disabled={saving || !emailOk || !qualified}
      onClick={() => void save()}
    >
      {saving ? <Spinner /> : "保存"}
    </button>
  );

  return { body, submit };
}

function Field({ id, label, children }: { id: string; label: string; children: ReactNode }) {
  return (
    <div className="field">
      <label htmlFor={id}>{label}</label>
      {children}
    </div>
  );
}

/** 密码框带一个「看一眼」。粘进来的密码看不见的话，多一个空格都查不出来。 */
function SecretInput({ id, value, onChange }: { id: string; value: string; onChange: (v: string) => void }) {
  const [shown, setShown] = useState(false);
  return (
    <div className="input-wrap">
      <input
        id={id}
        className="input"
        type={shown ? "text" : "password"}
        autoComplete="new-password"
        spellCheck={false}
        value={value}
        onChange={(e) => onChange(e.target.value)}
      />
      <button
        type="button"
        className="input-eye"
        onClick={() => setShown(!shown)}
        aria-label={shown ? "隐藏" : "显示"}
        tabIndex={-1}
      >
        <Icon name={shown ? "eyeOff" : "eye"} size={14} />
      </button>
    </div>
  );
}

/* ── 批量 ─────────────────────────────────────────────────────────────────── */

function useBulkPane(onDone: (count: number) => Promise<void>): Pane {
  const [text, setText] = useState("");
  const [preview, setPreview] = useState<ImportPreview | null>(null);
  const [parsing, setParsing] = useState(false);
  const [importing, setImporting] = useState(false);
  const [error, setError] = useState<unknown>(null);
  const timer = useRef<number | undefined>(undefined);

  // 边打边解析，但要防抖：粘一份两百行的清单会连着触发一串解析。
  const schedule = useCallback((next: string) => {
    window.clearTimeout(timer.current);
    if (!next.trim()) {
      setPreview(null);
      setParsing(false);
      return;
    }
    setParsing(true);
    timer.current = window.setTimeout(() => {
      accounts
        .parseDump(next)
        .then((p) => {
          setPreview(p);
          setError(null);
        })
        .catch(setError)
        .finally(() => setParsing(false));
    }, 260);
  }, []);

  useEffect(() => () => window.clearTimeout(timer.current), []);

  async function submit() {
    setImporting(true);
    setError(null);
    try {
      const outcome = await accounts.importDump(text);
      if (outcome.failures.length) {
        setError(new Error(`部分失败：${outcome.failures.join("；")}`));
        setImporting(false);
        return;
      }
      await onDone(outcome.imported);
    } catch (err) {
      setError(err);
      setImporting(false);
    }
  }

  const accepted = preview?.acceptedCount ?? 0;

  const body = (
    <div className="stack" style={{ gap: 12 }}>
      <ErrorNote error={error} />
      <ImportPanel
        text={text}
        parsing={parsing}
        preview={preview}
        onChange={(next) => {
          setText(next);
          schedule(next);
        }}
      />
    </div>
  );

  const submit_ = (
    <button
      type="button"
      className="btn btn-primary"
      disabled={importing || parsing || accepted === 0}
      onClick={() => void submit()}
    >
      {importing ? <Spinner /> : accepted > 0 ? `导入 ${accepted} 个账号` : "导入"}
    </button>
  );

  return { body, submit: submit_ };
}
