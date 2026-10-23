import { useEffect, useMemo, useState } from "react";
import { localChannels } from "../gateway/channels";
import { useRelay } from "../relay/useRelay";
import { go, SECTIONS, type Route } from "./nav";
import { Icon, Modal } from "../ui/primitives";

export function CommandPalette({
  open,
  onClose,
  onGo,
}: {
  open: boolean;
  onClose: () => void;
  onGo: (r: Route) => void;
}) {
  const [query, setQuery] = useState("");
  const relay = useRelay({ catalogs: true });
  const channels = useMemo(() => localChannels(relay.gateway, relay.local), [relay.gateway, relay.local]);

  useEffect(() => {
    function handleKeyDown(e: KeyboardEvent) {
      if ((e.metaKey || e.ctrlKey) && e.key.toLowerCase() === "k") {
        e.preventDefault();
        if (open) onClose();
        else setQuery("");
      }
    }
    window.addEventListener("keydown", handleKeyDown);
    return () => window.removeEventListener("keydown", handleKeyDown);
  }, [open, onClose]);

  if (!open) return null;

  const q = query.trim().toLowerCase();

  const matchingPages = SECTIONS.filter(
    (s) => !q || s.label.toLowerCase().includes(q) || s.id.toLowerCase().includes(q)
  );

  const matchingChannels = channels.filter(
    (c) => !q || c.label.toLowerCase().includes(q) || c.id.toLowerCase().includes(q) || c.prefixes.some((p) => p.includes(q))
  );

  const matchingModels = (relay.local ?? [])
    .filter((m) => !q || m.id.toLowerCase().includes(q) || (m.series && m.series.toLowerCase().includes(q)))
    .slice(0, 15);

  return (
    <Modal onClose={onClose} title="全局快速导航 (⌘K)" wide>
      <div className="command-palette">
        <div className="cmd-input-wrap">
          <Icon name="search" size={16} className="cmd-search-icon" />
          <input
            type="text"
            className="cmd-input"
            placeholder="搜索功能页面、通道或模型 ID…"
            value={query}
            onChange={(e) => setQuery(e.target.value)}
            autoFocus
          />
          {query ? (
            <button
              type="button"
              className="btn btn-sm btn-icon btn-quiet"
              onClick={() => setQuery("")}
            >
              <Icon name="close" size={12} />
            </button>
          ) : null}
        </div>

        <div className="cmd-results">
          {matchingPages.length > 0 ? (
            <div className="cmd-group">
              <div className="cmd-group-title">功能页面</div>
              {matchingPages.map((p) => (
                <div
                  key={p.id}
                  className="cmd-item"
                  onClick={() => {
                    onGo(go(p.id));
                    onClose();
                  }}
                >
                  <Icon name={p.icon} size={14} className="cmd-item-icon" />
                  <span className="cmd-item-label">{p.label}</span>
                  <span className="cmd-item-meta mono">#{p.id}</span>
                </div>
              ))}
            </div>
          ) : null}

          {matchingChannels.length > 0 ? (
            <div className="cmd-group">
              <div className="cmd-group-title">路由通道</div>
              {matchingChannels.map((c) => (
                <div
                  key={c.id}
                  className="cmd-item"
                  onClick={() => {
                    onGo(go("channels", { channel: c.id }));
                    onClose();
                  }}
                >
                  <span className={`pod-dot${c.ready ? " is-live" : ""}`} />
                  <span className="cmd-item-label">{c.label}</span>
                  <span className="cmd-item-meta mono">{c.prefixes[0] || `${c.id}/*`}</span>
                </div>
              ))}
            </div>
          ) : null}

          {matchingModels.length > 0 ? (
            <div className="cmd-group">
              <div className="cmd-group-title">模型</div>
              {matchingModels.map((m) => (
                <div
                  key={m.id}
                  className="cmd-item"
                  onClick={() => {
                    onGo(go("models", { model: m.id }));
                    onClose();
                  }}
                >
                  <Icon name="layers" size={14} className="cmd-item-icon" />
                  <span className="cmd-item-label mono">{m.id}</span>
                  <span className="cmd-item-meta">{m.vendorLabel}</span>
                </div>
              ))}
            </div>
          ) : null}

          {matchingPages.length === 0 && matchingChannels.length === 0 && matchingModels.length === 0 ? (
            <div className="cmd-empty">未找到匹配的结果</div>
          ) : null}
        </div>
      </div>
    </Modal>
  );
}
