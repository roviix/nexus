import fs from 'fs';
import path from 'path';

const outPath1 = path.resolve('docs/account-cards-v2.html');
const outPath2 = path.resolve('docs/account-cards-prototype.html');
const outPathArtifact = '/Users/zhongshangwu/.gemini/antigravity/brain/6430926c-b3e3-4592-a603-abae501a9f45/account-cards-v2.html';

const html = `<!DOCTYPE html>
<html lang="zh-CN">
<head>
<meta charset="UTF-8" />
<meta name="viewport" content="width=device-width, initial-scale=1.0" />
<title>Nexus · Account Cards v2 · 账号卡片设计系统与原型</title>
<link rel="preconnect" href="https://fonts.googleapis.com">
<link href="https://fonts.googleapis.com/css2?family=Inter:wght@400;500;600;700&family=JetBrains+Mono:wght@400;500;600&display=swap" rel="stylesheet">
<style>
/* ════════════════════════════════════════════════
   DESIGN TOKENS (设计系统规范)
   ════════════════════════════════════════════════ */
:root {
  /* Surfaces */
  --bg:            #0B0D12;
  --surface-1:     #12151C;
  --surface-2:     #171B24;
  --surface-3:     #1E232E;
  --border:        rgba(255,255,255,0.06);
  --border-hover:  rgba(255,255,255,0.14);
  --border-focus:  rgba(99,140,255,0.55);

  /* Typography / Foreground */
  --fg-1: #ECEEF3;                 /* 主标题、高亮数值 */
  --fg-2: rgba(236,238,243,0.65);  /* 次要文本、标签 */
  --fg-3: rgba(236,238,243,0.40);  /* 辅助信息、倒计时说明 */
  --fg-4: rgba(236,238,243,0.22);  /* 分隔符、禁用态 */

  /* Semantics */
  --ok:        #34C77B;
  --accent:    #638CFF;
  --warn:      #F5A524;
  --danger:    #F0485A;
  --ok-bg:     rgba(52,199,123,0.12);
  --accent-bg: rgba(99,140,255,0.12);
  --warn-bg:   rgba(245,165,36,0.12);
  --danger-bg: rgba(240,72,90,0.12);

  /* Fonts */
  --font-sans: "Inter", -apple-system, BlinkMacSystemFont, "PingFang SC", "Hiragino Sans GB", sans-serif;
  --font-mono: "JetBrains Mono", "SF Mono", Menlo, monospace;
  --t-10: 10px; --t-11: 11px; --t-12: 12px; --t-13: 13px; --t-14: 14px; --t-15: 15px; --t-20: 20px; --t-24: 24px;

  /* 4pt Space Scale */
  --s-1: 4px; --s-2: 8px; --s-3: 12px; --s-4: 16px; --s-5: 20px; --s-6: 24px; --s-8: 32px;

  /* Radii */
  --r-xs: 4px; --r-sm: 6px; --r-md: 10px; --r-lg: 14px; --r-full: 999px;

  /* Motion */
  --ease: cubic-bezier(.2,.8,.2,1);
}

html.light {
  --bg:            #F8FAFC;
  --surface-1:     #FFFFFF;
  --surface-2:     #F1F5F9;
  --surface-3:     #E2E8F0;
  --border:        rgba(15,23,42,0.08);
  --border-hover:  rgba(15,23,42,0.18);
  --border-focus:  rgba(37,99,235,0.6);

  --fg-1: #0F172A;
  --fg-2: rgba(15,23,42,0.72);
  --fg-3: rgba(15,23,42,0.48);
  --fg-4: rgba(15,23,42,0.25);

  --ok:        #059669;
  --accent:    #2563EB;
  --warn:      #D97706;
  --danger:    #DC2626;
  --ok-bg:     rgba(5,150,105,0.12);
  --accent-bg: rgba(37,99,235,0.12);
  --warn-bg:   rgba(217,119,6,0.12);
  --danger-bg: rgba(220,38,38,0.12);
}

* { box-sizing: border-box; margin: 0; padding: 0; }
html, body {
  background: var(--bg);
  color: var(--fg-1);
  font-family: var(--font-sans);
  font-size: var(--t-13);
  line-height: 1.45;
  -webkit-font-smoothing: antialiased;
  text-rendering: optimizeLegibility;
}
body {
  padding: var(--s-8) var(--s-6) 120px;
  background-image: 
    radial-gradient(1200px 600px at 20% -10%, rgba(99,140,255,0.08), transparent 60%),
    radial-gradient(900px 500px at 90% 0%, rgba(52,199,123,0.06), transparent 60%);
}

/* Page scaffold */
.page { max-width: 1280px; margin: 0 auto; }
.page-head {
  display: flex;
  align-items: flex-end;
  justify-content: space-between;
  margin-bottom: var(--s-6);
  padding-bottom: var(--s-4);
  border-bottom: 1px solid var(--border);
}
.brand-title {
  display: flex;
  align-items: center;
  gap: 12px;
}
.brand-icon {
  width: 34px; height: 34px; border-radius: var(--r-md);
  background: linear-gradient(135deg, var(--ok), var(--accent));
  display: grid; place-items: center; font-weight: 800; font-size: 17px; color: #fff;
  box-shadow: 0 4px 12px rgba(99,140,255,0.3);
}
.page-head h1 { font-size: var(--t-20); font-weight: 700; letter-spacing: -0.015em; }
.page-head p { color: var(--fg-3); font-size: var(--t-12); margin-top: 3px; }

.head-controls { display: flex; align-items: center; gap: var(--s-3); }
.seg {
  display: flex; gap: 2px; padding: 2px;
  background: var(--surface-1); border: 1px solid var(--border); border-radius: var(--r-md);
}
.seg button {
  height: 28px; padding: 0 var(--s-3); border: 0; border-radius: 7px;
  background: transparent; color: var(--fg-3); font: 500 var(--t-12) var(--font-sans);
  cursor: pointer; transition: all .15s ease;
}
.seg button:hover { color: var(--fg-2); }
.seg button.on { background: var(--surface-3); color: var(--fg-1); font-weight: 600; }

.btn-theme {
  height: 32px; padding: 0 12px; border-radius: var(--r-md);
  background: var(--surface-1); border: 1px solid var(--border);
  color: var(--fg-2); font-size: 12px; font-weight: 500; cursor: pointer;
  display: flex; align-items: center; gap: 6px; transition: all .15s;
}
.btn-theme:hover { background: var(--surface-2); color: var(--fg-1); }

/* Banner */
.design-banner {
  background: linear-gradient(135deg, rgba(99,140,255,0.08), rgba(52,199,123,0.05));
  border: 1px solid rgba(99,140,255,0.18);
  border-radius: var(--r-lg);
  padding: 20px 24px;
  margin-bottom: var(--s-8);
  display: flex; justify-content: space-between; align-items: flex-start; gap: 20px;
}
.design-banner h2 { font-size: 16px; font-weight: 700; color: var(--fg-1); margin-bottom: 6px; }
.design-banner p { font-size: 13px; color: var(--fg-2); line-height: 1.6; max-width: 820px; }
.pill-list { display: flex; gap: 8px; flex-wrap: wrap; margin-top: 10px; }
.pill-tag {
  font-size: 11px; padding: 2px 8px; border-radius: var(--r-full);
  background: rgba(255,255,255,0.06); border: 1px solid var(--border); color: var(--fg-2);
}
.pill-tag.hi { background: var(--accent-bg); color: #9DB6FF; border-color: rgba(99,140,255,0.25); font-weight: 600; }

/* Sections */
.section { margin-bottom: var(--s-8); }
.section-head {
  display: flex; align-items: baseline; justify-content: space-between;
  margin-bottom: var(--s-4); padding-bottom: var(--s-3); border-bottom: 1px solid var(--border);
}
.section-head-left { display: flex; align-items: baseline; gap: var(--s-3); }
.section-head h2 { font-size: var(--t-14); font-weight: 700; color: var(--fg-1); }
.section-head .archetype { font-size: var(--t-11); color: var(--fg-3); font-family: var(--font-mono); }
.section-head .note { font-size: var(--t-12); color: var(--fg-3); }
.grid { display: grid; grid-template-columns: repeat(auto-fill, minmax(310px, 1fr)); gap: var(--s-4); }

/* ════════════════════════════════════════════════
   THE REFINED CARD COMPONENT
   ════════════════════════════════════════════════ */
.card {
  position: relative;
  background: linear-gradient(180deg, rgba(255,255,255,0.025) 0%, rgba(255,255,255,0) 100%), var(--surface-1);
  border: 1px solid var(--border);
  border-radius: var(--r-lg);
  padding: var(--s-4);
  display: flex; flex-direction: column; gap: var(--s-4);
  transition: border-color .18s var(--ease), transform .18s var(--ease), box-shadow .18s var(--ease);
  cursor: pointer; outline: none;
}
.card::before { /* Top 1px subtle glow highlight line */
  content: ""; position: absolute; inset: 0 0 auto 0; height: 1px; border-radius: var(--r-lg) var(--r-lg) 0 0;
  background: linear-gradient(90deg, transparent, rgba(255,255,255,0.12), transparent); pointer-events: none;
}
.card:hover {
  border-color: var(--border-hover);
  transform: translateY(-2px);
  box-shadow: 0 10px 28px -12px rgba(0,0,0,0.65);
}
.card:focus-visible {
  border-color: var(--border-focus);
  box-shadow: 0 0 0 3px rgba(99,140,255,0.18);
}
.card.is-danger { border-color: rgba(240,72,90,0.32); }
.card.is-danger:hover { border-color: rgba(240,72,90,0.5); }
.card.is-warn { border-color: rgba(245,165,36,0.3); }

/* Header */
.card-head { display: flex; align-items: center; gap: var(--s-3); min-height: 32px; }
.logo {
  flex: none; width: 32px; height: 32px; border-radius: var(--r-md);
  display: grid; place-items: center; font: 600 var(--t-12) var(--font-sans);
  border: 1px solid rgba(255,255,255,0.06); position: relative;
}
.logo svg { width: 16px; height: 16px; }

.identity { flex: 1; min-width: 0; display: flex; flex-direction: column; gap: 2px; }
.identity .name { display: flex; align-items: center; gap: var(--s-2); min-width: 0; }
.identity .email {
  font-size: var(--t-13); font-weight: 600; color: var(--fg-1);
  white-space: nowrap; overflow: hidden; text-overflow: ellipsis; letter-spacing: -0.005em;
}
.identity .platform { font-size: var(--t-11); color: var(--fg-3); display: flex; align-items: center; gap: 6px; }
.identity .platform i { display: inline-block; width: 2px; height: 2px; border-radius: 50%; background: var(--fg-4); }

.head-actions { flex: none; display: flex; align-items: center; gap: var(--s-2); }

.badge {
  display: inline-flex; align-items: center; height: 18px; padding: 0 6px;
  border-radius: var(--r-xs); font: 600 var(--t-10) var(--font-sans);
  letter-spacing: .03em; white-space: nowrap; border: 1px solid transparent;
}
.badge.plan { background: rgba(255,255,255,0.06); color: var(--fg-2); border-color: rgba(255,255,255,0.05); }
.badge.plan.hi { background: var(--accent-bg); color: #9DB6FF; border-color: rgba(99,140,255,0.22); }
.badge.plan.ult { background: linear-gradient(135deg,rgba(99,140,255,0.18),rgba(190,120,255,0.18)); color: #C9B8FF; border-color: rgba(190,120,255,0.25); }
.badge.plan.free { background: rgba(255,255,255,0.04); color: var(--fg-3); }
.badge.cap { background: transparent; color: var(--fg-3); border-color: var(--border); text-transform: none; letter-spacing: 0; font-size: var(--t-10); font-weight: 400; }
.badge.cap svg { width: 10px; height: 10px; margin-right: 3px; }

/* Status Breathing Dot */
.dot { width: 7px; height: 7px; border-radius: 50%; flex: none; position: relative; }
.dot.ok { background: var(--ok); box-shadow: 0 0 0 3px var(--ok-bg); }
.dot.warn { background: var(--warn); box-shadow: 0 0 0 3px var(--warn-bg); }
.dot.danger { background: var(--danger); box-shadow: 0 0 0 3px var(--danger-bg); }
.dot.ok::after {
  content: ""; position: absolute; inset: -3px; border-radius: 50%;
  border: 1px solid var(--ok); opacity: 0; animation: pulse 2.6s ease-out infinite;
}
@keyframes pulse { 0% { transform: scale(.6); opacity: .7; } 100% { transform: scale(2.0); opacity: 0; } }

.icon-btn {
  width: 24px; height: 24px; border-radius: var(--r-sm);
  display: grid; place-items: center; color: var(--fg-3); border: 0;
  background: transparent; cursor: pointer; opacity: 0;
  transition: opacity .15s, background .15s, color .15s;
}
.card:hover .icon-btn, .card:focus-within .icon-btn { opacity: 1; }
.icon-btn:hover { background: rgba(255,255,255,0.08); color: var(--fg-1); }
.icon-btn svg { width: 14px; height: 14px; }

/* Metrics & Tracks */
.metrics { display: flex; flex-direction: column; gap: var(--s-3); }
.metric { display: flex; flex-direction: column; gap: 6px; }
.metric-row { display: flex; align-items: baseline; justify-content: space-between; gap: var(--s-3); }
.metric-label { font-size: var(--t-11); color: var(--fg-2); display: flex; align-items: center; gap: 6px; font-weight: 500; }
.metric-label .win { color: var(--fg-3); font-weight: 500; font-family: var(--font-mono); font-size: var(--t-10); background: rgba(255,255,255,0.05); padding: 1px 4px; border-radius: 3px; }
.metric-val { font-family: var(--font-mono); font-size: var(--t-12); color: var(--fg-1); font-weight: 600; font-variant-numeric: tabular-nums; letter-spacing: -0.01em; }
.metric-val small { color: var(--fg-3); font-size: var(--t-11); font-weight: 400; }
.metric-val.warn { color: var(--warn); }
.metric-val.danger { color: var(--danger); }

.track { height: 4px; border-radius: var(--r-full); background: rgba(255,255,255,0.06); overflow: hidden; position: relative; }
html.light .track { background: rgba(0,0,0,0.06); }
.fill { height: 100%; border-radius: var(--r-full); background: var(--ok); transition: width .6s var(--ease); position: relative; }
.fill::after { content: ""; position: absolute; inset: 0; background: linear-gradient(90deg, transparent, rgba(255,255,255,0.2)); }
.fill.warn { background: var(--warn); }
.fill.danger { background: var(--danger); }
.fill.accent { background: var(--accent); }

.metric-sub { display: flex; justify-content: space-between; font-size: var(--t-11); color: var(--fg-3); font-variant-numeric: tabular-nums; }
.metric-sub b { font-weight: 500; color: var(--fg-2); }
.metric-sub .mono { font-family: var(--font-mono); font-size: var(--t-10); color: var(--fg-3); }

/* Symmetric Dual-Column (Antigravity Variant) */
.metrics.cols { display: grid; grid-template-columns: 1fr 1fr; gap: var(--s-4); }
.metrics.cols .metric { padding-right: var(--s-3); border-right: 1px solid var(--border); }
.metrics.cols .metric:last-child { padding-right: 0; border-right: 0; padding-left: var(--s-1); }

/* Credits (Numerical Value Variant: Kiro / Cursor) */
.credits { display: flex; align-items: flex-end; justify-content: space-between; gap: var(--s-4); }
.credits .num { font-family: var(--font-mono); font-size: var(--t-24); font-weight: 600; line-height: 1; letter-spacing: -0.02em; font-variant-numeric: tabular-nums; }
.credits .num small { font-size: var(--t-12); color: var(--fg-3); font-weight: 400; margin-left: 4px; }
.credits .cap { font-size: var(--t-11); color: var(--fg-3); margin-top: 6px; }
.credits .cap b { color: var(--fg-2); font-weight: 500; font-family: var(--font-mono); }
.credit-split { display: flex; gap: var(--s-4); text-align: right; }
.credit-split div span { display: block; font-size: var(--t-10); color: var(--fg-3); letter-spacing: .02em; }
.credit-split div b { display: block; font-family: var(--font-mono); font-weight: 500; font-size: var(--t-12); color: var(--fg-2); margin-top: 2px; font-variant-numeric: tabular-nums; }
.track.stacked { display: flex; gap: 2px; background: transparent; }
.track.stacked span { height: 100%; border-radius: var(--r-full); background: rgba(255,255,255,0.06); overflow: hidden; position: relative; }
.track.stacked span i { display: block; height: 100%; background: var(--ok); }
.track.stacked span.trial i { background: #8EA6FF; opacity: .8; }

/* Compact Credential Variant (Zcode / Provider) */
.card.compact { gap: 0; padding: var(--s-3) var(--s-4); }
.card.compact .card-head { min-height: 38px; }
.card.compact .logo { width: 28px; height: 28px; border-radius: 8px; }
.card.compact .logo svg { width: 14px; height: 14px; }
.kv { display: flex; gap: var(--s-4); margin-top: var(--s-3); padding-top: var(--s-3); border-top: 1px solid var(--border); }
.kv div { display: flex; flex-direction: column; gap: 2px; min-width: 0; }
.kv span { font-size: var(--t-10); color: var(--fg-3); letter-spacing: .02em; }
.kv b { font-weight: 500; font-size: var(--t-12); color: var(--fg-2); font-family: var(--font-mono); white-space: nowrap; overflow: hidden; text-overflow: ellipsis; font-variant-numeric: tabular-nums; }
.kv b.ok { color: var(--ok); }
.chip-row { display: flex; gap: 4px; flex-wrap: wrap; margin-top: 2px; }
.chip { height: 19px; padding: 0 6px; display: inline-flex; align-items: center; border-radius: var(--r-xs); background: rgba(255,255,255,0.05); border: 1px solid var(--border); font: 500 var(--t-10) var(--font-mono); color: var(--fg-2); }

/* Footer */
.card-foot {
  display: flex; align-items: center; justify-content: space-between; gap: var(--s-3);
  padding-top: var(--s-3); border-top: 1px solid var(--border); font-size: var(--t-11); color: var(--fg-3);
}
.card-foot .meta { display: flex; align-items: center; gap: 6px; white-space: nowrap; overflow: hidden; text-overflow: ellipsis; }
.card-foot .meta i { width: 2px; height: 2px; border-radius: 50%; background: var(--fg-4); display: inline-block; }
.card-foot .sync { font-family: var(--font-mono); font-size: var(--t-10); display: flex; align-items: center; gap: 4px; color: var(--fg-3); }
.card-foot .sync svg { width: 10px; height: 10px; }

/* Inline Alerts */
.alert {
  display: flex; align-items: center; gap: var(--s-2);
  padding: var(--s-2) var(--s-3); border-radius: var(--r-sm); font-size: var(--t-11); line-height: 1.35;
}
.alert svg { width: 14px; height: 14px; flex: none; }
.alert.danger { background: var(--danger-bg); color: #FF8A96; border: 1px solid rgba(240,72,90,0.22); }
.alert.warn { background: var(--warn-bg); color: #FFC76B; border: 1px solid rgba(245,165,36,0.22); }
.alert .act {
  margin-left: auto; font-weight: 500; color: var(--fg-1); background: rgba(255,255,255,0.1);
  height: 22px; padding: 0 var(--s-2); border-radius: var(--r-xs); display: inline-flex;
  align-items: center; border: 0; cursor: pointer; font-size: var(--t-11); white-space: nowrap;
}
.alert .act:hover { background: rgba(255,255,255,0.18); }

/* Skeleton */
.sk {
  background: linear-gradient(90deg, rgba(255,255,255,0.04) 25%, rgba(255,255,255,0.09) 50%, rgba(255,255,255,0.04) 75%);
  background-size: 200% 100%; animation: sh 1.4s infinite; border-radius: var(--r-xs);
}
@keyframes sh { 0% { background-position: 200% 0; } 100% { background-position: -200% 0; } }

/* Platform Logos */
.lg-openai   { background: #10A37F14; color: #5BD6B0; }
.lg-claude   { background: #D9772914; color: #E8A06A; }
.lg-google   { background: #4285F414; color: #8AB4F8; }
.lg-xai      { background: #FFFFFF0F; color: #E6E6E6; }
.lg-kiro     { background: #8B5CF614; color: #B79CFF; }
.lg-qoder    { background: #06B6D414; color: #67E8F9; }
.lg-zcode    { background: #F59E0B14; color: #FBBF24; }
.lg-provider { background: #10B98114; color: #34D399; }
.lg-cursor   { background: #FFFFFF0F; color: #F3F3F3; }

/* ════════ Spec Documentation Panels ════════ */
.spec-grid { display: grid; grid-template-columns: repeat(auto-fit, minmax(280px, 1fr)); gap: var(--s-4); }
.spec-card { background: var(--surface-1); border: 1px solid var(--border); border-radius: var(--r-lg); padding: var(--s-4); }
.spec-card h3 { font-size: var(--t-12); font-weight: 700; color: var(--fg-2); margin-bottom: var(--s-3); letter-spacing: .02em; }
.spec-row { display: flex; align-items: center; justify-content: space-between; padding: 7px 0; border-bottom: 1px dashed var(--border); font-size: var(--t-11); color: var(--fg-3); }
.spec-row:last-child { border-bottom: 0; }
.spec-row code { font-family: var(--font-mono); color: var(--fg-2); font-size: var(--t-11); }
.swatch { display: flex; align-items: center; gap: 8px; }
.swatch i { width: 14px; height: 14px; border-radius: 4px; display: inline-block; }
.bar-preview { height: 6px; background: var(--accent); border-radius: 3px; opacity: .8; }
</style>
</head>
<body>
<div class="page">

  <!-- 页面头部 -->
  <header class="page-head">
    <div class="brand-title">
      <div class="brand-icon">N</div>
      <div>
        <h1>Nexus · 账号卡片设计系统 v2</h1>
        <p>基于设计系统 Token · 派生 4 类卡片原型 · 覆盖 9 大平台全生命周期状态</p>
      </div>
    </div>
    <div class="head-controls">
      <div class="seg" id="filter-seg">
        <button class="on" onclick="filterCards('all', this)">全部 (11)</button>
        <button onclick="filterCards('ok', this)">正常 (8)</button>
        <button onclick="filterCards('attention', this)">需关注 (3)</button>
      </div>
      <button class="btn-theme" onclick="toggleTheme()">
        <span id="theme-ico">☀️</span> 切换明暗
      </button>
    </div>
  </header>

  <!-- 设计理念横幅 -->
  <div class="design-banner">
    <div>
      <h2>✨ 设计系统升级要点：从「拼凑换皮」到「原子化 Token 与变体派生」</h2>
      <p>
        深入拆解上游 9 个平台的数据颗粒度差异后，我们摒弃了一刀切的冗余文本模式，建立了一套严格的 
        <b>4pt 间距网格</b>、<b>微光质感 Surface</b>、<b>顶部 1px 高光细线</b> 以及 
        <b>4 大卡片原型变体</b>。彻底解决了 Zcode/供应商等无上游配额平台的尴尬占位，同时将 Kiro 的积分可视化升级为数值分层。
      </p>
      <div class="pill-list">
        <span class="pill-tag hi">A · 标准双桶/多桶计量 (ChatGPT, Antigravity)</span>
        <span class="pill-tag hi">B · 紧凑单周期计量 (Claude, Grok, Qoder)</span>
        <span class="pill-tag hi">C · 数值积分与多段堆叠 (Kiro, Cursor)</span>
        <span class="pill-tag hi">D · 极简凭证收缩卡 (Zcode, 供应商 API Key)</span>
        <span class="pill-tag">呼吸脉冲状态点</span>
        <span class="pill-tag">无上游配额自动坍缩高度</span>
      </div>
    </div>
  </div>

  <!-- ═══════════════ A · 标准计量卡 ═══════════════ -->
  <section class="section">
    <div class="section-head">
      <div class="section-head-left">
        <h2>变体 A · 标准多桶计量卡</h2>
        <span class="archetype">Standard Metered · ChatGPT / Antigravity</span>
      </div>
      <span class="note">支持 5h 滚动窗口与 7d 周期配额，分桶清晰，倒计时精准内联</span>
    </div>
    <div class="grid">

      <!-- ChatGPT · 正常 -->
      <article class="card" data-status="ok" tabindex="0">
        <div class="card-head">
          <div class="logo lg-openai">
            <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><path d="M12 2l3 5 5 1-4 4 1 6-5-3-5 3 1-6-4-4 5-1z"/></svg>
          </div>
          <div class="identity">
            <div class="name">
              <span class="email">arvid.pfeffer@outlook.com</span>
              <span class="badge plan hi">Pro</span>
            </div>
            <div class="platform">ChatGPT <i></i> 本地网关接力</div>
          </div>
          <div class="head-actions">
            <button class="icon-btn" title="更多操作"><svg viewBox="0 0 24 24" fill="currentColor"><circle cx="5" cy="12" r="1.8"/><circle cx="12" cy="12" r="1.8"/><circle cx="19" cy="12" r="1.8"/></svg></button>
            <span class="dot ok"></span>
          </div>
        </div>
        <div class="metrics">
          <div class="metric">
            <div class="metric-row">
              <span class="metric-label">Codex <span class="win">5h</span></span>
              <span class="metric-val">28<small>%</small></span>
            </div>
            <div class="track"><div class="fill" style="width:28%"></div></div>
            <div class="metric-sub"><span>4h 50m 后重置</span><span class="mono">17:30</span></div>
          </div>
          <div class="metric">
            <div class="metric-row">
              <span class="metric-label">Codex <span class="win">7d</span></span>
              <span class="metric-val">55<small>%</small></span>
            </div>
            <div class="track"><div class="fill" style="width:55%"></div></div>
            <div class="metric-sub"><span>周日 09:41 重置</span><span class="mono">6 天后</span></div>
          </div>
        </div>
        <div class="card-foot">
          <span class="meta">订阅还剩 12 天 <i></i> Spark 等次要桶见详情</span>
          <span class="sync">
            <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.5"><path d="M4 12a8 8 0 0 1 14-5M20 12a8 8 0 0 1-14 5"/></svg>
            21m
          </span>
        </div>
      </article>

      <!-- ChatGPT · 预警态 (Warning > 80%) -->
      <article class="card is-warn" data-status="attention" tabindex="0">
        <div class="card-head">
          <div class="logo lg-openai">
            <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><path d="M12 2l3 5 5 1-4 4 1 6-5-3-5 3 1-6-4-4 5-1z"/></svg>
          </div>
          <div class="identity">
            <div class="name">
              <span class="email">mara.quill@outlook.com</span>
              <span class="badge plan">Plus</span>
            </div>
            <div class="platform">ChatGPT <i></i> 本地网关接力</div>
          </div>
          <div class="head-actions">
            <button class="icon-btn" title="更多操作"><svg viewBox="0 0 24 24" fill="currentColor"><circle cx="5" cy="12" r="1.8"/><circle cx="12" cy="12" r="1.8"/><circle cx="19" cy="12" r="1.8"/></svg></button>
            <span class="dot warn"></span>
          </div>
        </div>
        <div class="metrics">
          <div class="metric">
            <div class="metric-row">
              <span class="metric-label">Codex <span class="win">5h</span></span>
              <span class="metric-val warn">88<small>%</small></span>
            </div>
            <div class="track"><div class="fill warn" style="width:88%"></div></div>
            <div class="metric-sub"><span><b>42m</b> 后重置</span><span class="mono">12:57</span></div>
          </div>
          <div class="metric">
            <div class="metric-row">
              <span class="metric-label">Codex <span class="win">7d</span></span>
              <span class="metric-val">47<small>%</small></span>
            </div>
            <div class="track"><div class="fill" style="width:47%"></div></div>
            <div class="metric-sub"><span>周日 重置</span><span class="mono">03/17</span></div>
          </div>
        </div>
        <div class="card-foot">
          <span class="meta">订阅还剩 6 天 <i></i> 23 天前添加</span>
          <span class="sync">
            <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.5"><path d="M4 12a8 8 0 0 1 14-5M20 12a8 8 0 0 1-14 5"/></svg>
            刚刚
          </span>
        </div>
      </article>

      <!-- Antigravity · 对称双列 (Gemini / Others) -->
      <article class="card" data-status="ok" tabindex="0">
        <div class="card-head">
          <div class="logo lg-google">
            <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><circle cx="12" cy="12" r="9"/><path d="M12 3v18M3 12h18"/></svg>
          </div>
          <div class="identity">
            <div class="name">
              <span class="email">ada@gmail.com</span>
              <span class="badge plan ult">Google AI Pro</span>
            </div>
            <div class="platform">Antigravity <i></i> aicode-demo</div>
          </div>
          <div class="head-actions">
            <button class="icon-btn" title="更多操作"><svg viewBox="0 0 24 24" fill="currentColor"><circle cx="5" cy="12" r="1.8"/><circle cx="12" cy="12" r="1.8"/><circle cx="19" cy="12" r="1.8"/></svg></button>
            <span class="dot ok"></span>
          </div>
        </div>
        <div class="metrics cols">
          <div class="metric">
            <div class="metric-row"><span class="metric-label">Gemini <span class="win">5h</span></span><span class="metric-val">28<small>%</small></span></div>
            <div class="track"><div class="fill" style="width:28%"></div></div>
            <div class="metric-sub"><span>2h 后重置</span></div>
          </div>
          <div class="metric">
            <div class="metric-row"><span class="metric-label">Others <span class="win">5h</span></span><span class="metric-val">59<small>%</small></span></div>
            <div class="track"><div class="fill" style="width:59%"></div></div>
            <div class="metric-sub"><span>1h 后重置</span></div>
          </div>
        </div>
        <div class="card-foot">
          <span class="meta">周限 Gemini 0% <i></i> Others 0%</span>
          <span class="sync">
            <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.5"><path d="M4 12a8 8 0 0 1 14-5M20 12a8 8 0 0 1-14 5"/></svg>
            6m
          </span>
        </div>
      </article>

    </div>
  </section>

  <!-- ═══════════════ B · 紧凑单条卡 ═══════════════ -->
  <section class="section">
    <div class="section-head">
      <div class="section-head-left">
        <h2>变体 B · 紧凑单周期计量卡</h2>
        <span class="archetype">Compact Single · Claude / Grok / Qoder</span>
      </div>
      <span class="note">单主指标排版，突出多模态能力徽标 (生图/视频)，去除了冗余的「额度」字样</span>
    </div>
    <div class="grid">

      <!-- Claude · 正常 -->
      <article class="card" data-status="ok" tabindex="0">
        <div class="card-head">
          <div class="logo lg-claude">
            <svg viewBox="0 0 24 24" fill="currentColor"><path d="M7 4h4l6 16h-4l-1.3-3.6H8.1L6.8 20H3zm2.2 9.2h3.4L10.9 8.4z"/></svg>
          </div>
          <div class="identity">
            <div class="name">
              <span class="email">ada@anthropic.com</span>
              <span class="badge plan ult">Max</span>
            </div>
            <div class="platform">Claude <i></i> OAuth 会话</div>
          </div>
          <div class="head-actions">
            <button class="icon-btn"><svg viewBox="0 0 24 24" fill="currentColor"><circle cx="5" cy="12" r="1.8"/><circle cx="12" cy="12" r="1.8"/><circle cx="19" cy="12" r="1.8"/></svg></button>
            <span class="dot ok"></span>
          </div>
        </div>
        <div class="metrics">
          <div class="metric">
            <div class="metric-row"><span class="metric-label">周限周期</span><span class="metric-val">64<small>%</small></span></div>
            <div class="track"><div class="fill" style="width:64%"></div></div>
            <div class="metric-sub"><span>后天 12:44 重置</span><span class="mono">2 天后</span></div>
          </div>
        </div>
        <div class="card-foot">
          <span class="meta">20 天前添加</span>
          <span class="sync"><svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.5"><path d="M4 12a8 8 0 0 1 14-5M20 12a8 8 0 0 1-14 5"/></svg>12m</span>
        </div>
      </article>

      <!-- Grok · 具备生图能力 -->
      <article class="card" data-status="ok" tabindex="0">
        <div class="card-head">
          <div class="logo lg-xai">
            <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.2"><path d="M4 4l16 16M20 4L4 20"/></svg>
          </div>
          <div class="identity">
            <div class="name">
              <span class="email">you@x.ai</span>
              <span class="badge plan hi">SuperGrok</span>
            </div>
            <div class="platform">
              Grok Build <i></i> 
              <span class="badge cap"><svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.5"><rect x="3" y="5" width="18" height="14" rx="2"/><path d="M3 15l5-5 4 4 3-3 6 6"/></svg>生图</span>
              <span class="badge cap"><svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.5"><rect x="3" y="6" width="13" height="12" rx="2"/><path d="M16 10l5-3v10l-5-3"/></svg>视频</span>
            </div>
          </div>
          <div class="head-actions">
            <button class="icon-btn"><svg viewBox="0 0 24 24" fill="currentColor"><circle cx="5" cy="12" r="1.8"/><circle cx="12" cy="12" r="1.8"/><circle cx="19" cy="12" r="1.8"/></svg></button>
            <span class="dot ok"></span>
          </div>
        </div>
        <div class="metrics">
          <div class="metric">
            <div class="metric-row"><span class="metric-label">周限周期 <span class="win">7d</span></span><span class="metric-val">37<small>%</small></span></div>
            <div class="track"><div class="fill" style="width:37%"></div></div>
            <div class="metric-sub"><span>3 天后重置</span><span class="mono">03/15</span></div>
          </div>
        </div>
        <div class="card-foot">
          <span class="meta">已自动探测媒体生成资格</span>
          <span class="sync"><svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.5"><path d="M4 12a8 8 0 0 1 14-5M20 12a8 8 0 0 1-14 5"/></svg>5m</span>
        </div>
      </article>

      <!-- Qoder · 套餐内与加购双桶 (已去噪) -->
      <article class="card" data-status="ok" tabindex="0">
        <div class="card-head">
          <div class="logo lg-qoder">
            <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.2"><circle cx="11" cy="11" r="7"/><path d="M15 15l5 5"/></svg>
          </div>
          <div class="identity">
            <div class="name">
              <span class="email">ada@example.com</span>
              <span class="badge plan hi">Pro</span>
              <span class="badge cap">国际版</span>
            </div>
            <div class="platform">Qoder <i></i> PAT 令牌</div>
          </div>
          <div class="head-actions">
            <button class="icon-btn"><svg viewBox="0 0 24 24" fill="currentColor"><circle cx="5" cy="12" r="1.8"/><circle cx="12" cy="12" r="1.8"/><circle cx="19" cy="12" r="1.8"/></svg></button>
            <span class="dot ok"></span>
          </div>
        </div>
        <div class="metrics">
          <div class="metric">
            <div class="metric-row"><span class="metric-label">套餐内</span><span class="metric-val">42<small>%</small></span></div>
            <div class="track"><div class="fill" style="width:42%"></div></div>
            <div class="metric-sub"><span>11 天后重置</span><span class="mono">安全清洗远期戳</span></div>
          </div>
          <div class="metric">
            <div class="metric-row"><span class="metric-label">加购</span><span class="metric-val">2<small>%</small></span></div>
            <div class="track"><div class="fill" style="width:2%"></div></div>
          </div>
        </div>
        <div class="card-foot">
          <span class="meta">••••pt-8f…c2a1 <i></i> 2026/9/1 添加</span>
          <span class="sync"><svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.5"><path d="M4 12a8 8 0 0 1 14-5M20 12a8 8 0 0 1-14 5"/></svg>2m</span>
        </div>
      </article>

    </div>
  </section>

  <!-- ═══════════════ C · 数值积分卡 ═══════════════ -->
  <section class="section">
    <div class="section-head">
      <div class="section-head-left">
        <h2>变体 C · 数值积分卡</h2>
        <span class="archetype">Numerical Credit · Kiro / Cursor (基准)</span>
      </div>
      <span class="note">突出剩余/总限额大字数值 (420/1,000)，支持基础与试用/赠送堆叠进度条</span>
    </div>
    <div class="grid">

      <!-- Kiro · 积分数值 -->
      <article class="card" data-status="ok" tabindex="0">
        <div class="card-head">
          <div class="logo lg-kiro">
            <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.2"><path d="M6 4v16M6 12l10-8M6 12l10 8"/></svg>
          </div>
          <div class="identity">
            <div class="name">
              <span class="email">ada@kiro.dev</span>
              <span class="badge plan hi">Kiro Pro</span>
            </div>
            <div class="platform">Kiro <i></i> 社交登录</div>
          </div>
          <div class="head-actions">
            <button class="icon-btn"><svg viewBox="0 0 24 24" fill="currentColor"><circle cx="5" cy="12" r="1.8"/><circle cx="12" cy="12" r="1.8"/><circle cx="19" cy="12" r="1.8"/></svg></button>
            <span class="dot ok"></span>
          </div>
        </div>
        <div class="metrics">
          <div class="credits">
            <div>
              <div class="num">580<small>点可用</small></div>
              <div class="cap">总额 <b>1,000</b> · 已消耗 <b>420</b></div>
            </div>
            <div class="credit-split">
              <div><span>月度基数</span><b>420 / 1,000</b></div>
              <div><span>超额扣费</span><b>未开启</b></div>
            </div>
          </div>
          <div class="track stacked">
            <span style="flex:1000"><i style="width:42%"></i></span>
          </div>
        </div>
        <div class="card-foot">
          <span class="meta">月账期 <b style="color:var(--fg-2);font-weight:500">8 天后重置</b> <i></i> 20 天前添加</span>
          <span class="sync"><svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.5"><path d="M4 12a8 8 0 0 1 14-5M20 12a8 8 0 0 1-14 5"/></svg>8m</span>
        </div>
      </article>

      <!-- Cursor · 基准对比 -->
      <article class="card" data-status="attention" tabindex="0">
        <div class="card-head">
          <div class="logo lg-cursor">
            <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><path d="M12 2l9 5v10l-9 5-9-5V7z"/><path d="M12 12l9-5M12 12v10M12 12L3 7"/></svg>
          </div>
          <div class="identity">
            <div class="name">
              <span class="email">dev.wang@proton.me</span>
              <span class="badge plan hi">Pro</span>
              <span class="badge cap">切号池</span>
            </div>
            <div class="platform">Cursor <i></i> 当前正在登录</div>
          </div>
          <div class="head-actions">
            <button class="icon-btn"><svg viewBox="0 0 24 24" fill="currentColor"><circle cx="5" cy="12" r="1.8"/><circle cx="12" cy="12" r="1.8"/><circle cx="19" cy="12" r="1.8"/></svg></button>
            <span class="dot warn"></span>
          </div>
        </div>
        <div class="metrics">
          <div class="credits">
            <div>
              <div class="num" style="color:var(--warn)">38<small>快速剩余</small></div>
              <div class="cap">快速请求 <b>500</b> · 已用 <b>462</b></div>
            </div>
            <div class="credit-split">
              <div><span>按需消费</span><b>$4.20</b></div>
            </div>
          </div>
          <div class="track"><div class="fill warn" style="width:92%"></div></div>
        </div>
        <div class="card-foot">
          <span class="meta">月额 <b style="color:var(--fg-2);font-weight:500">17 天后重置</b> <i></i> Bot 明天重置</span>
          <span class="sync"><svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.5"><path d="M4 12a8 8 0 0 1 14-5M20 12a8 8 0 0 1-14 5"/></svg>1m</span>
        </div>
      </article>

    </div>
  </section>

  <!-- ═══════════════ D · 极简凭证卡 ═══════════════ -->
  <section class="section">
    <div class="section-head">
      <div class="section-head-left">
        <h2>变体 D · 极简凭证收缩卡</h2>
        <span class="archetype">Compact Credential · Zcode / 自定义供应商 API Key</span>
      </div>
      <span class="note">上游无配额时自动坍缩垂直高度 (~68px)，彻底消除假空进度条与呆板文字框</span>
    </div>
    <div class="grid">

      <!-- Zcode · 智谱 GLM -->
      <article class="card compact" data-status="ok" tabindex="0">
        <div class="card-head">
          <div class="logo lg-zcode">
            <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.2"><path d="M5 5h14L5 19h14"/></svg>
          </div>
          <div class="identity">
            <div class="name">
              <span class="email">lin@z.ai</span>
              <span class="badge plan free">个人版</span>
              <span class="badge cap">API Key</span>
            </div>
            <div class="platform">Zcode <i></i> 智谱 GLM 编码套餐</div>
          </div>
          <div class="head-actions">
            <button class="icon-btn"><svg viewBox="0 0 24 24" fill="currentColor"><circle cx="5" cy="12" r="1.8"/><circle cx="12" cy="12" r="1.8"/><circle cx="19" cy="12" r="1.8"/></svg></button>
            <span class="dot ok"></span>
          </div>
        </div>
        <div class="kv">
          <div><span>钥匙凭证</span><b>••••sk-z9ab</b></div>
          <div><span>配额类型</span><b>永久编码套餐</b></div>
          <div><span>本机网关调用</span><b class="ok">1.4k 次 · 12M Token</b></div>
        </div>
      </article>

      <!-- 供应商 · DeepSeek -->
      <article class="card compact" data-status="ok" tabindex="0">
        <div class="card-head">
          <div class="logo lg-provider">
            <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.2"><circle cx="12" cy="12" r="9"/><path d="M8 12h8M12 8v8"/></svg>
          </div>
          <div class="identity">
            <div class="name">
              <span class="email">DeepSeek Official</span>
              <span class="badge plan hi">Anthropic 协议</span>
              <span class="badge cap">2 模型</span>
            </div>
            <div class="platform">api.deepseek.com <i></i> ••••3f9a</div>
          </div>
          <div class="head-actions">
            <button class="icon-btn"><svg viewBox="0 0 24 24" fill="currentColor"><circle cx="5" cy="12" r="1.8"/><circle cx="12" cy="12" r="1.8"/><circle cx="19" cy="12" r="1.8"/></svg></button>
            <span class="dot ok"></span>
          </div>
        </div>
        <div class="kv">
          <div><span>健康延迟</span><b class="ok">32ms (在线)</b></div>
          <div><span>支持模型</span>
            <div class="chip-row">
              <span class="chip">deepseek-chat</span>
              <span class="chip">deepseek-reasoner</span>
            </div>
          </div>
        </div>
      </article>

    </div>
  </section>

  <!-- ═══════════════ E · 全生命周期异常与状态系统 ═══════════════ -->
  <section class="section">
    <div class="section-head">
      <div class="section-head-left">
        <h2>状态系统 · 耗尽 / 需重授权 / 骨架屏</h2>
        <span class="archetype">State System · Danger / Warning / Skeleton</span>
      </div>
      <span class="note">补全卡片在真实运行中的边界状态，确保错误可感知、可操作</span>
    </div>
    <div class="grid">

      <!-- Claude · 耗尽态 -->
      <article class="card is-danger" data-status="attention" tabindex="0">
        <div class="card-head">
          <div class="logo lg-claude">
            <svg viewBox="0 0 24 24" fill="currentColor"><path d="M7 4h4l6 16h-4l-1.3-3.6H8.1L6.8 20H3zm2.2 9.2h3.4L10.9 8.4z"/></svg>
          </div>
          <div class="identity">
            <div class="name">
              <span class="email">backup.acc@outlook.com</span>
              <span class="badge plan">Pro</span>
            </div>
            <div class="platform">Claude <i></i> 会话凭证</div>
          </div>
          <div class="head-actions">
            <button class="icon-btn"><svg viewBox="0 0 24 24" fill="currentColor"><circle cx="5" cy="12" r="1.8"/><circle cx="12" cy="12" r="1.8"/><circle cx="19" cy="12" r="1.8"/></svg></button>
            <span class="dot danger"></span>
          </div>
        </div>
        <div class="metrics">
          <div class="metric">
            <div class="metric-row"><span class="metric-label">本周用量</span><span class="metric-val danger">100<small>%</small></span></div>
            <div class="track"><div class="fill danger" style="width:100%"></div></div>
            <div class="metric-sub"><span style="color:var(--danger)">已耗尽 · <b style="color:#FF8A96">1h 48m</b> 后恢复</span><span class="mono">14:03</span></div>
          </div>
        </div>
        <div class="card-foot">
          <span class="meta">已自动暂停接入网关候选</span>
          <span class="sync">刚刚</span>
        </div>
      </article>

      <!-- 需重新授权 (带行内动作警示条) -->
      <article class="card is-danger" data-status="attention" tabindex="0">
        <div class="card-head">
          <div class="logo lg-openai">
            <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><path d="M12 2l3 5 5 1-4 4 1 6-5-3-5 3 1-6-4-4 5-1z"/></svg>
          </div>
          <div class="identity">
            <div class="name">
              <span class="email">expired.session@corp.com</span>
              <span class="badge plan free">失效</span>
            </div>
            <div class="platform">ChatGPT <i></i> 会话掉登录</div>
          </div>
          <div class="head-actions">
            <span class="dot danger"></span>
          </div>
        </div>
        
        <!-- 行内告警条 -->
        <div class="alert danger">
          <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><circle cx="12" cy="12" r="10"/><path d="M12 8v4M12 16h.01"/></svg>
          <span>访问令牌过期，需重新登录以恢复接力</span>
          <button class="act" onclick="alert('触发重新授权流程')">重新授权</button>
        </div>

        <div class="card-foot">
          <span class="meta">Refresh Token 已失效</span>
          <span class="sync">2d 前</span>
        </div>
      </article>

      <!-- 骨架屏 (加载状态) -->
      <article class="card" data-status="ok" style="cursor:default">
        <div class="card-head">
          <div class="sk" style="width:32px;height:32px;border-radius:10px;"></div>
          <div class="identity">
            <div class="sk" style="width:140px;height:14px;margin-bottom:4px;"></div>
            <div class="sk" style="width:90px;height:10px;"></div>
          </div>
          <div class="sk" style="width:16px;height:16px;border-radius:50%;"></div>
        </div>
        <div class="metrics">
          <div class="metric">
            <div style="display:flex;justify-content:space-between;">
              <div class="sk" style="width:70px;height:11px;"></div>
              <div class="sk" style="width:32px;height:11px;"></div>
            </div>
            <div class="sk" style="width:100%;height:4px;border-radius:999px;margin:4px 0;"></div>
            <div class="sk" style="width:100px;height:10px;"></div>
          </div>
        </div>
        <div class="card-foot" style="border-top-color:transparent;">
          <div class="sk" style="width:120px;height:10px;"></div>
          <div class="sk" style="width:30px;height:10px;"></div>
        </div>
      </article>

    </div>
  </section>

  <!-- ═══════════════ F · 设计系统规范面板 (TOKEN SPEC) ═══════════════ -->
  <section class="section">
    <div class="section-head">
      <div class="section-head-left">
        <h2>设计规范与 Token 字典 (Design System Spec)</h2>
        <span class="archetype">Tokens · Typography · Spacing · Color Scale</span>
      </div>
      <span class="note">供专业设计与研发直接对齐的 Token 定义</span>
    </div>
    <div class="spec-grid">

      <!-- Colors -->
      <div class="spec-card">
        <h3>🎨 语义色与微光表面 (Color & Surface)</h3>
        <div class="spec-row">
          <div class="swatch"><i style="background:#0B0D12;border:1px solid #333"></i><span>Background</span></div>
          <code>#0B0D12 (--bg)</code>
        </div>
        <div class="spec-row">
          <div class="swatch"><i style="background:#12151C;border:1px solid #333"></i><span>Surface 1 (卡片)</span></div>
          <code>#12151C (--surface-1)</code>
        </div>
        <div class="spec-row">
          <div class="swatch"><i style="background:#34C77B"></i><span>健康可用 (OK)</span></div>
          <code>#34C77B (--ok)</code>
        </div>
        <div class="spec-row">
          <div class="swatch"><i style="background:#638CFF"></i><span>品牌主色 (Accent)</span></div>
          <code>#638CFF (--accent)</code>
        </div>
        <div class="spec-row">
          <div class="swatch"><i style="background:#F5A524"></i><span>预警阈值 (Warn)</span></div>
          <code>#F5A524 (--warn)</code>
        </div>
        <div class="spec-row">
          <div class="swatch"><i style="background:#F0485A"></i><span>危险耗尽 (Danger)</span></div>
          <code>#F0485A (--danger)</code>
        </div>
      </div>

      <!-- Typography -->
      <div class="spec-card">
        <h3>🔤 字体与字号规范 (Typography Scale)</h3>
        <div class="spec-row">
          <span>大字数值 (Credits)</span>
          <code>24px / JetBrains Mono 600</code>
        </div>
        <div class="spec-row">
          <span>主标题 / 邮箱</span>
          <code>13px / Inter 600</code>
        </div>
        <div class="spec-row">
          <span>百分比数值</span>
          <code>12px / JetBrains Mono Tabular</code>
        </div>
        <div class="spec-row">
          <span>分桶标签 / 窗口</span>
          <code>11px / Inter 500</code>
        </div>
        <div class="spec-row">
          <span>辅助元数据 / 倒计时</span>
          <code>10.5px / Inter 400</code>
        </div>
        <div class="spec-row">
          <span>胶囊徽标 (Badges)</span>
          <code>10px / Inter 600 uppercase</code>
        </div>
      </div>

      <!-- Spacing & Radii -->
      <div class="spec-card">
        <h3>📐 间距与网格尺度 (4pt Grid Scale)</h3>
        <div class="spec-row">
          <span>卡片内边距 (Padding)</span>
          <code>16px (--s-4)</code>
        </div>
        <div class="spec-row">
          <span>网格间隙 (Grid Gap)</span>
          <code>16px (--s-4)</code>
        </div>
        <div class="spec-row">
          <span>卡片大圆角 (Border Radius)</span>
          <code>14px (--r-lg)</code>
        </div>
        <div class="spec-row">
          <span>Logo 头像圆角</span>
          <code>10px (--r-md)</code>
        </div>
        <div class="spec-row">
          <span>微进度条高度 (Track Height)</span>
          <code>4px (全圆角 var(--r-full))</code>
        </div>
        <div class="spec-row">
          <span>卡片顶部微光高线</span>
          <code>1px linear-gradient</code>
        </div>
      </div>

    </div>
  </section>

</div>

<script>
  function toggleTheme() {
    const html = document.documentElement;
    const isDark = !html.classList.contains('light');
    if (isDark) {
      html.classList.add('light');
      document.getElementById('theme-ico').textContent = '🌙';
    } else {
      html.classList.remove('light');
      document.getElementById('theme-ico').textContent = '☀️';
    }
  }

  function filterCards(type, btn) {
    document.querySelectorAll('#filter-seg button').forEach(b => b.classList.remove('on'));
    btn.classList.add('on');

    const cards = document.querySelectorAll('article.card');
    cards.forEach(c => {
      const status = c.getAttribute('data-status');
      if (type === 'all') {
        c.style.display = 'flex';
      } else if (type === 'ok') {
        c.style.display = status === 'ok' ? 'flex' : 'none';
      } else if (type === 'attention') {
        c.style.display = status === 'attention' ? 'flex' : 'none';
      }
    });
  }
</script>
</body>
</html>
`;

fs.writeFileSync(outPath1, html, 'utf8');
fs.writeFileSync(outPath2, html, 'utf8');
fs.writeFileSync(outPathArtifact, html, 'utf8');
console.log('Saved prototype v2 to:');
console.log('1.', outPath1);
console.log('2.', outPath2);
console.log('3.', outPathArtifact);
