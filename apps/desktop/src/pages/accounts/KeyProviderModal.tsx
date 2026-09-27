/**
 * 添加 / 编辑一家 API Key 供应商。
 *
 * 一把钥匙、一个兼容地址、一张模型清单——和别的通道加一个号是同一件事。模型清单是这家能跑的
 * 上游 id：先「获取模型列表」从它那里拉，勾要用的；拉不到就手填。客户端怎么叫它们在「接入」
 * 里按客户端配。钥匙在点保存时才交给 Rust，列表里之后只有尾号；编辑时钥匙留空表示不换。
 */
import { useMemo, useState } from "react";
import { errorText, keyProviders } from "../../ipc/api";
import type { ApiFormat, AuthField, KeyProvider } from "../../ipc/types";
import { Banner, Icon, Modal, Spinner } from "../../ui/primitives";
import { PRESETS, baseUrlHint, cleanModels, splitModels, type Preset } from "./keyModel";

interface Draft {
  name: string;
  website: string;
  baseUrl: string;
  apiKey: string;
  format: ApiFormat;
  auth: AuthField;
  models: string[];
}

function fromProvider(p: KeyProvider | null): Draft {
  if (!p) {
    return { name: "", website: "", baseUrl: "", apiKey: "", format: "openai_chat", auth: "auth_token", models: [] };
  }
  return {
    name: p.name,
    website: p.website ?? "",
    baseUrl: p.baseUrl,
    apiKey: "",
    format: p.apiFormat,
    auth: p.authField,
    models: [...p.models],
  };
}

export function KeyProviderModal({
  initial,
  onClose,
  onSaved,
}: {
  initial: KeyProvider | null;
  onClose: () => void;
  onSaved: (provider: KeyProvider) => void;
}) {
  const editing = initial != null;
  const [draft, setDraft] = useState<Draft>(() => fromProvider(initial));
  const [preset, setPreset] = useState("custom");
  const [catalog, setCatalog] = useState<string[] | null>(null);
  const [filter, setFilter] = useState("");
  const [typed, setTyped] = useState("");
  const [fetching, setFetching] = useState(false);
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [showKey, setShowKey] = useState(false);

  function patch(partial: Partial<Draft>) {
    setDraft((d) => ({ ...d, ...partial }));
    setError(null);
  }

  function applyPreset(p: Preset) {
    setPreset(p.id);
    setCatalog(null);
    if (p.id === "custom") return;
    setDraft((d) => {
      const namedByPreset = PRESETS.some((x) => x.id !== "custom" && x.label === d.name.trim());
      return {
        ...d,
        name: !d.name.trim() || namedByPreset ? p.label : d.name,
        website: p.website,
        baseUrl: p.baseUrl,
        format: p.format,
        auth: p.auth,
      };
    });
  }

  const has = (id: string) => draft.models.some((m) => m.toLowerCase() === id.toLowerCase());
  const toggle = (id: string) => patch({ models: has(id) ? draft.models.filter((m) => m.toLowerCase() !== id.toLowerCase()) : [...draft.models, id] });
  const addTyped = () => {
    const more = splitModels(typed);
    if (!more.length) return;
    patch({ models: cleanModels([...draft.models, ...more]) });
    setTyped("");
  };

  const shown = useMemo(() => {
    const kw = filter.trim().toLowerCase();
    return (catalog ?? []).filter((id) => !kw || id.toLowerCase().includes(kw));
  }, [catalog, filter]);

  const keyMissing = !editing && !draft.apiKey.trim();
  const blocked = !draft.name.trim() || !draft.baseUrl.trim() || keyMissing || draft.models.length === 0;

  async function fetchModels() {
    setFetching(true);
    setError(null);
    try {
      const ids = await keyProviders.models({
        baseUrl: draft.baseUrl,
        apiFormat: draft.format,
        authField: draft.auth,
        apiKey: draft.apiKey.trim() || null,
        providerId: initial?.id ?? null,
      });
      setCatalog(ids);
      setFilter("");
    } catch (e) {
      setError(errorText(e));
    } finally {
      setFetching(false);
    }
  }

  async function save() {
    setSaving(true);
    setError(null);
    try {
      const saved = await keyProviders.save({
        id: initial?.id ?? null,
        name: draft.name,
        website: draft.website.trim() || null,
        baseUrl: draft.baseUrl,
        apiFormat: draft.format,
        authField: draft.auth,
        models: draft.models,
        apiKey: draft.apiKey.trim() || null,
      });
      onSaved(saved);
    } catch (e) {
      setError(errorText(e));
      setSaving(false);
    }
  }

  return (
    <Modal
      wide
      title={editing ? `编辑 ${initial.name}` : "添加供应商"}
      subtitle="一把钥匙、一个地址、一张模型清单。钥匙只存本机。"
      onClose={onClose}
      footer={
        <>
          <span className="faint tiny grow">{blocked ? (draft.models.length === 0 ? "至少要有一个模型" : keyMissing ? "还差 API Key" : "还差名称或地址") : `${draft.models.length} 个模型`}</span>
          <button type="button" className="btn" onClick={onClose} disabled={saving}>
            取消
          </button>
          <button type="button" className="btn btn-primary" disabled={saving || blocked} onClick={() => void save()}>
            {saving ? <Spinner /> : null}
            {editing ? "保存" : "添加"}
          </button>
        </>
      }
    >
      <div className="stack" style={{ gap: 14 }}>
        {!editing ? (
          <div className="connect-chans-bar" role="group" aria-label="常用供应商">
            {PRESETS.map((p) => (
              <button key={p.id} type="button" className={`chan-chip${preset === p.id ? " is-active" : ""}`} onClick={() => applyPreset(p)}>
                {p.label}
              </button>
            ))}
          </div>
        ) : null}

        {error ? <Banner tone="bad" title="出了点问题" hint={error} /> : null}

        <div className="fsect">
          <div className="pform-grid">
            <Field label="名称" id="prov-name">
              <input id="prov-name" className="input" value={draft.name} onChange={(e) => patch({ name: e.target.value })} placeholder="例如 DeepSeek" autoFocus={!editing} />
            </Field>
            <Field label="API 格式" id="prov-fmt">
              <select id="prov-fmt" className="select" value={draft.format} onChange={(e) => patch({ format: e.target.value as ApiFormat })}>
                <option value="openai_chat">OpenAI Chat（/chat/completions）</option>
                <option value="anthropic">Anthropic Messages（/v1/messages）</option>
                <option value="openai_responses">OpenAI Responses（/responses）</option>
              </select>
            </Field>
          </div>
          <Field label="请求地址" id="prov-url">
            <input id="prov-url" className="input mono" value={draft.baseUrl} onChange={(e) => patch({ baseUrl: e.target.value })} placeholder={draft.format === "anthropic" ? "https://api.example.com/anthropic" : "https://api.example.com/v1"} spellCheck={false} />
            <p className="subtitle" style={{ margin: 0, maxWidth: "none" }}>
              {baseUrlHint(draft.format)}
            </p>
          </Field>
          <div className="pform-grid">
            <Field label="API Key" id="prov-key">
              <div className="input-wrap">
                <input
                  id="prov-key"
                  className="input mono"
                  type={showKey ? "text" : "password"}
                  value={draft.apiKey}
                  autoComplete="off"
                  spellCheck={false}
                  onChange={(e) => patch({ apiKey: e.target.value })}
                  placeholder={editing ? (initial.keyTail ? `留空不换（····${initial.keyTail}）` : "留空不换") : "sk-…"}
                />
                <button type="button" className="input-eye" onClick={() => setShowKey((v) => !v)} aria-label={showKey ? "隐藏" : "显示"} tabIndex={-1}>
                  <Icon name={showKey ? "eyeOff" : "eye"} size={14} />
                </button>
              </div>
            </Field>
            {draft.format === "anthropic" ? (
              <Field label="钥匙放在" id="prov-auth">
                <select id="prov-auth" className="select" value={draft.auth} onChange={(e) => patch({ auth: e.target.value as AuthField })}>
                  <option value="auth_token">Authorization: Bearer（中转常用）</option>
                  <option value="api_key">x-api-key（Anthropic 官方）</option>
                </select>
              </Field>
            ) : (
              <Field label="网站" id="prov-site">
                <input id="prov-site" className="input" value={draft.website} onChange={(e) => patch({ website: e.target.value })} placeholder="可选，给自己看" spellCheck={false} />
              </Field>
            )}
          </div>
        </div>

        <div className="fsect">
          <div className="fsect-cap">
            <span>模型</span>
            <button type="button" className="btn btn-sm btn-quiet" disabled={fetching || !draft.baseUrl.trim() || (!draft.apiKey.trim() && !initial)} onClick={() => void fetchModels()}>
              {fetching ? <Spinner /> : <Icon name="refresh" size={12} />}
              获取模型列表
            </button>
          </div>
          <p className="subtitle" style={{ margin: 0 }}>
            网关里写成 <code className="mono">provider/模型</code>。同一个模型有几家都声明时，按列表顺序接力。
          </p>

          {draft.models.length ? (
            <div className="pchips">
              {draft.models.map((m) => (
                <span key={m} className="pchip mono">
                  {m}
                  <button type="button" aria-label={`去掉 ${m}`} onClick={() => toggle(m)}>
                    <Icon name="close" size={10} />
                  </button>
                </span>
              ))}
            </div>
          ) : null}

          <div className="row" style={{ gap: 6 }}>
            <input
              className="input mono"
              value={typed}
              onChange={(e) => setTyped(e.target.value)}
              onKeyDown={(e) => {
                if (e.key === "Enter") {
                  e.preventDefault();
                  addTyped();
                }
              }}
              placeholder="手动加：模型 id，回车添加（可一次贴几个）"
              spellCheck={false}
            />
            <button type="button" className="btn btn-sm" disabled={!typed.trim()} onClick={addTyped}>
              添加
            </button>
          </div>

          {catalog ? (
            <div className="pcat">
              <div className="row-between" style={{ gap: 8 }}>
                <input className="input" value={filter} onChange={(e) => setFilter(e.target.value)} placeholder={`在 ${catalog.length} 个里筛选`} />
                <span className="row" style={{ gap: 4, flex: "none" }}>
                  <button type="button" className="btn btn-sm btn-quiet" onClick={() => patch({ models: cleanModels([...draft.models, ...shown]) })}>
                    全选{filter ? "筛出的" : ""}
                  </button>
                  <button type="button" className="btn btn-sm btn-quiet" onClick={() => patch({ models: draft.models.filter((m) => !shown.some((s) => s.toLowerCase() === m.toLowerCase())) })}>
                    全不选
                  </button>
                </span>
              </div>
              <div className="pcat-list">
                {shown.slice(0, 300).map((id) => (
                  <label key={id} className={`pcat-row${has(id) ? " is-on" : ""}`}>
                    <input type="checkbox" className="tick" checked={has(id)} onChange={() => toggle(id)} />
                    <span className="mono truncate">{id}</span>
                  </label>
                ))}
                {!shown.length ? <span className="faint tiny">没有对得上的。</span> : null}
              </div>
            </div>
          ) : null}
        </div>
      </div>
    </Modal>
  );
}

function Field({ id, label, children }: { id: string; label: string; children: React.ReactNode }) {
  return (
    <div className="field">
      <label htmlFor={id}>{label}</label>
      {children}
    </div>
  );
}
