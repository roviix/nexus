<div align="center">

<img src="apps/desktop/src-tauri/icons/128x128@2x.png" width="96" alt="Nexus Logo" />

# Nexus

**The All-in-One Local AI Gateway & Decentralized Mesh Pool**

Turn your existing AI subscriptions and API providers into standard, unified OpenAI & Anthropic endpoints on your local machine.

[![Release](https://img.shields.io/github/v/release/roviix/nexus?include_prereleases&sort=semver&style=flat-square)](https://github.com/roviix/nexus/releases)
[![CI](https://github.com/roviix/nexus/actions/workflows/ci.yml/badge.svg?style=flat-square)](https://github.com/roviix/nexus/actions/workflows/ci.yml)
[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg?style=flat-square)](./LICENSE)
![Platform](https://img.shields.io/badge/Platform-macOS%20%7C%20Windows-lightgrey?style=flat-square)

[**📥 Download Latest Release**](https://github.com/roviix/nexus/releases/latest) · [中文](./README.md) · **English**

<br/>

<a href="https://github.com/roviix/nexus/releases/latest">
  <img src="docs/images/channels.png" width="850" alt="Nexus Channels & Mesh Dashboard" style="border-radius: 10px; box-shadow: 0 8px 30px rgba(0,0,0,0.25);" />
</a>

</div>

---

## 💡 Why Nexus?

If you use **Claude Code**, **Cursor**, **Codex CLI**, or other AI coding agents in your daily development, you have likely run into these frustrations:

- 💸 **Expensive API Keys**: You already pay for web subscriptions like ChatGPT Plus, Claude Pro, or Kiro, yet you can't use them directly inside your favorite terminal agents without paying twice;
- 🛑 **Painful Rate Limits (429 / Quota Exhaustion)**: Hitting a rate limit right in the middle of writing code breaks your workflow, forcing you to manually log out, switch accounts, and restart;
- ⚙️ **Tedious Configuration**: Every CLI tool requires its own configuration file format (JSON, TOML, or environment variables) in different paths, and a single syntax error breaks your workflow;
- 🌐 **Difficult Device & Team Sharing**: Wanting to share idle quota with your home desktop or a teammate usually requires complex proxy setups or public IP routing.

👉 **Nexus solves all of this**. Running locally on your machine, Nexus translates your existing accounts into a unified, high-availability pipeline with automatic account relaying and decentralized P2P sharing (Mesh).

---

## ✨ Key Features

### 1. 🔀 Dual-Dialect Unified Gateway
A single local port (`8787`) simultaneously translates **OpenAI** (`/v1/chat/completions`) and **Anthropic** (`/v1/messages`) formats. Full support for streaming SSE, Thinking Tokens, Prompt Caching (`cache_control`), and Tool Calls.

### 2. 🔄 Smart Account Pools & Quota Auto-Relay
Manage multiple accounts across different platforms. When an account runs out of quota or hits rate limits, Nexus automatically relays traffic to the next available account in the pool, maintaining session stickiness to prevent broken conversations.

<div align="center">
  <img src="docs/images/accounts.png" width="800" alt="Nexus Accounts" style="border-radius: 8px;" />
</div>

### 3. 🌐 Nexus Mesh (Decentralized P2P Sharing)
Lend your idle channels to other computers or team members using secure, end-to-end encrypted reverse tunnels.
- **Zero Configuration**: Pair in seconds using a 6-character dynamic code without public IPs or port forwarding;
- **Local Owner Protection**: If you make a request locally, Nexus automatically prioritizes local traffic so external sharing never lags your own experience.

### 4. ⚡ One-Click Client Onboarding
Stop manually editing config files. Nexus seamlessly hooks into developer tools, creating automatic backups before making any modifications:
- **Claude Code**: Configures `~/.claude/settings.json`, skips login onboarding, and supports Sonnet / Opus / Haiku / Fable routes;
- **Codex CLI**: Injects config into `~/.codex/config.toml`;
- **OpenCode** / **Grok CLI** / **Cursor IDE** ready out-of-the-box.

<div align="center">
  <img src="docs/images/connect.png" width="800" alt="Nexus Connect" style="border-radius: 8px;" />
</div>

### 5. 🔒 100% Local-First & Private
All tokens, credentials, and request logs are stored strictly inside your local machine's SQLite database. **No central servers, no cloud tracking, zero telemetry**. Your sensitive code never passes through third-party servers.

---

## 🧩 Supported Ecosystem

| Supported Upstream | Authentication | Highlights |
| :--- | :--- | :--- |
| **ChatGPT** | Browser OAuth / Session Import | Native Codex CLI backend, supports o1 / o3 / gpt-5 models |
| **Claude** | Web OAuth / Credential Import | Full Anthropic Messages protocol with Thinking stream support |
| **Kiro** | AWS SSO / CLI Token Import | Aggregates all top coding models with rock-solid concurrency |
| **Grok** | xAI Auth / Refresh Token | Large context window and deep analytical reasoning |
| **ZCode** | Auto-detects local credentials / API Key | Zhipu GLM coding models with direct, low-latency connection |
| **Qoder** | Personal Access Token (PAT) | Dedicated routes for both international and domestic regions |
| **Google Antigravity** | Auth Token Import | Google Antigravity developer model suite |
| **Custom Providers** | Custom Base URL + API Key | DeepSeek, Kimi, OpenRouter, SiliconFlow, and OpenAI/Anthropic proxies |

**Supported Downstream Clients**: Claude Code CLI, Codex CLI, OpenCode, Cursor, Chatbox, NextChat, official Python/Node.js SDKs, and any client supporting custom base URLs.

---

## 🚀 3-Step Quick Start

### Step 1: Download & Install
Download the binary for your operating system from [GitHub Releases](https://github.com/roviix/nexus/releases/latest):
- **macOS**: Download `.dmg` and drag to Applications (Universal binary for Apple Silicon & Intel)
- **Windows**: Download `.exe` installer and run

### Step 2: Add Your Accounts
Launch Nexus, head to **Accounts**, and click on any provider card to log in via browser OAuth or paste credentials.

### Step 3: Connect Your Client
Go to **Connect**, locate your favorite tool (such as Claude Code), and click **Connect**. Open your terminal and start coding!

You can also send standard OpenAI-compatible requests directly:

```bash
curl http://127.0.0.1:8787/v1/chat/completions \
  -H "Authorization: Bearer $NEXUS_GATEWAY_TOKEN" \
  -H "Content-Type: application/json" \
  -d '{
    "model": "kiro/claude-sonnet-4.5",
    "messages": [{"role": "user", "content": "Hello Nexus!"}]
  }'
```

---

<details>
<summary>🛠️ Developer Guide: Building from Source & Architecture (Click to expand)</summary>

### Prerequisites
- **Node.js** >= 20
- **Rust** stable (via `rustup`)
- **Tauri v2** prerequisites (see [Tauri Prerequisites](https://v2.tauri.app/start/prerequisites/))

### Build & Development
```bash
# Clone repository
git clone https://github.com/roviix/nexus.git
cd nexus/apps/desktop

# Install dependencies and start development server
npm ci
npm run dev

# Build production installer
npm run tauri build
```

### Architecture Overview
```
nexus/
├── Cargo.toml                  # Workspace configuration
├── crates/
│   ├── nexus-core/             # Core entities, error types, and utilities
│   ├── nexus-store/            # Local SQLite storage, secret encryption, and migrations
│   ├── nexus-gateway/          # Local HTTP gateway, dual-dialect translation & dispatching
│   ├── nexus-mesh/             # Decentralized P2P reverse tunneling and pairing
│   ├── nexus-connect/          # Client configuration injection, backup, and restore
│   ├── nexus-accounts/         # Account pool lifecycle, quota tracking, and auto-relay
│   └── nexus-*/                # Platform adapters (chatgpt, claude, kiro, grok, zcode, qoder...)
├── apps/desktop/               # Tauri v2 + React desktop app source
└── docs/                       # Technical architecture specs (ARCHITECTURE.md, MESH.md)
```

</details>

---

## 💬 Community & Feedback

Join our developer community to share workflows, explore multi-device mesh setups, and report issues:

<div align="center">
  <img src="docs/images/wechat_qr.jpg" width="220" alt="WeChat Community QR Code" style="border-radius: 12px; box-shadow: 0 4px 20px rgba(0,0,0,0.15);" />
  <p style="margin-top: 10px; font-size: 13.5px; color: #555;">
    Scan the WeChat QR code above to connect with the author (ID: <strong>ouyz</strong>) with note <strong>Nexus</strong> to join the developer group, or open a discussion on <a href="https://github.com/roviix/nexus/discussions">GitHub Discussions</a>.
  </p>
</div>

---

## ⚠️ Disclaimer

- Nexus is intended solely for personal study, research, and legitimate multi-device workflow management.
- This software connects to authorized user sessions or public client endpoints provided by third-party platforms. Users are responsible for complying with the relevant Terms of Service.
- This project is not affiliated with, endorsed by, or sponsored by OpenAI, Anthropic, Cursor, xAI, AWS, or any related entity.
- The software is provided "AS IS" under the MIT license, without warranty of any kind.

---

## 📄 License

Licensed under the [MIT License](./LICENSE). Stars and feedback are always welcome!
