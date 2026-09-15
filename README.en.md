<div align="center">

<img src="apps/desktop/src-tauri/icons/128x128@2x.png" width="96" alt="Nexus" />

# Nexus

**Turn the AI subscriptions you already pay for into a local OpenAI / Anthropic compatible endpoint.**

[![ci](https://github.com/roviix/nexus/actions/workflows/ci.yml/badge.svg)](https://github.com/roviix/nexus/actions/workflows/ci.yml)
[![release](https://img.shields.io/github/v/release/roviix/nexus?include_prereleases&sort=semver)](https://github.com/roviix/nexus/releases)
[![license](https://img.shields.io/badge/license-MIT-blue.svg)](./LICENSE)
![platform](https://img.shields.io/badge/platform-macOS%20%7C%20Windows-lightgrey)

[中文](./README.md) · English

</div>

Nexus is a Rust + Tauri v2 desktop app for macOS and Windows. It runs a gateway on `127.0.0.1`
that uses **your own** Cursor / ChatGPT / Grok / Kiro accounts as upstreams and speaks the standard
`/v1/chat/completions`, `/v1/messages`, `/v1/responses`, `/v1/models` and `/v1/images/generations`.
Anything that talks the OpenAI or Anthropic dialect — Claude Code, Codex CLI, OpenCode, the official
SDKs, plain `curl` — can point at it directly. No API key to apply for.

Everything stays on your machine: accounts, tokens, the request ledger, backups. No cloud, no
account system, no telemetry.

![Overview](docs/images/overview.png)

> **The app's interface is currently Chinese only.** The screenshots below reflect that.
> Localisation is not implemented yet; if you want to work on it, please open an issue first.

> Architecture, key mechanisms and the deliberate trade-offs are documented in
> [`docs/ARCHITECTURE.md`](./docs/ARCHITECTURE.md) (Chinese). Decision records for the two Cursor
> patch channels are in [`docs/SAND.md`](./docs/SAND.md) and [`docs/CRSR.md`](./docs/CRSR.md)
> (Chinese).

## What it does

### The local gateway (the main feature)

- **One port, four dialects.** OpenAI Chat Completions, Anthropic Messages (including
  `count_tokens`), OpenAI Responses, OpenAI Images. Inbound requests are parsed into a single
  intermediate representation and then bridged to the upstream; streaming SSE passes through as-is.
- **Multiple upstreams.** Cursor (`aiserver.v1.InferenceService/Stream`), ChatGPT subscriptions
  (`chatgpt.com/backend-api/codex`), Grok and Kiro. `/v1/models` aggregates the catalogue according
  to what each upstream can actually do.
- **The channel is part of the model name.** Catalogue entries are keyed as `{channel}/{model}`
  (`cursor/claude-opus-5`, `chatgpt/gpt-5`). A prefixed name is forced onto that channel; a bare name
  goes to **the default channel you picked**. Which pool a request lands on is visible and editable
  rather than something the gateway infers.
- **Quota relay, not load balancing.** One user, one machine — one account is enough at any moment.
  Nexus keeps using the current account and only moves to the next when the quota runs out.
  Session stickiness therefore comes for free and accounts rotate very rarely.
- **Model name mapping.** Claude Code sends `claude-sonnet-4-5`, Codex sends `gpt-5`; the gateway
  maps those to names the upstream recognises. You can also force every request onto one model.
- **A passthrough port.** A separate h2c port forwards `cursor-agent`'s native Connect traffic
  verbatim after swapping the identity headers.
- **A request ledger.** Every request records account, model, tokens and latency. The overview page
  reads from it.

![Local gateway](docs/images/gateway.png)

The default port is `8787` and the gateway is off by default. Once enabled:

```bash
curl http://127.0.0.1:8787/v1/chat/completions \
  -H "Authorization: Bearer $NEXUS_GATEWAY_TOKEN" \
  -H "Content-Type: application/json" \
  -d '{"model":"claude-sonnet-4-5","messages":[{"role":"user","content":"hi"}]}'
```

### One-click client setup

The "connect" page edits your client config files directly — Claude Code
(`~/.claude/settings.json`), Codex CLI (`~/.codex/config.toml`), OpenCode — pointing them at the
local gateway. It backs the file up first and every change is revertible. Only our keys are touched;
if the file isn't valid JSON/TOML to begin with, Nexus refuses to edit it rather than guess.

![Connect](docs/images/connect.png)

### Account pool

- Add Cursor / ChatGPT / Grok / Kiro accounts. OAuth opens your system browser and the app collects
  the token in the background; you can also bulk-import by pasting refresh tokens, `crsr_` API keys
  or Codex session JSON — several at once, format detected automatically.
- See plan, quota and reset times. Expired, banned and exhausted accounts are flagged automatically.
- Quota and billing are two separate cards: one for what's left and when it resets, one for list
  price, discounts, next charge and past invoices. They are different units and are never merged
  into a single number.
- Credentials live in a local SQLite database — `0700` / `0600` on macOS, the default `%APPDATA%`
  ACL on Windows. They do not go into the OS keychain and they are never uploaded anywhere — see
  [`docs/ARCHITECTURE.md`](./docs/ARCHITECTURE.md) §9.1 for why, including what that costs you.

![Accounts](docs/images/accounts.png)

### Cursor account switching

No reverse engineering, no patching, and it doesn't break when Cursor updates: Nexus writes
Cursor's own login state database (`state.vscdb`) directly. Each account gets its own machine ID,
and the current state is backed up before every switch.

![Switcher](docs/images/switcher.png)

### Playground

An in-app multi-turn chat workbench that talks to the local gateway over exactly the same address,
key and path your clients use. It is there to verify that an account still works and to compare
model output — not to be yet another chat UI.

![Playground](docs/images/playground.png)

### Two Cursor patch channels (optional, advanced)

Both **modify Cursor's application files**. They occupy the same hook, so only one can be installed
at a time. Read the corresponding document and the disclaimer below before you enable either.

- **Sand** ([`docs/SAND.md`](./docs/SAND.md)) reroutes the Agent panel built into the Cursor IDE to
  the local gateway, so inference inside the IDE also runs on your account pool. It can be installed
  on a remote dev box over SSH.
- **CRSR** ([`docs/CRSR.md`](./docs/CRSR.md)) changes no routing at all. It only swaps the
  authorization header on panel requests for a short-lived token minted from one account's `crsr_`
  User API Key — the panel still speaks native `agent.v1.AgentService/Run`, someone else is just
  paying. The patch renews the token itself, so closing Nexus won't hand you a 401 mid-keystroke.

## Install

Download the installer for your platform from
[Releases](https://github.com/roviix/nexus/releases) (macOS `.dmg`, Windows `.exe`). Signed releases
support in-app auto-update. Unsigned Preview builds must be downloaded manually, and on macOS you
will need to allow the first launch under System Settings → Privacy & Security.

## Building from source

Requirements: Rust stable, Node.js 20+, and the Tauri v2
[platform prerequisites](https://v2.tauri.app/start/prerequisites/).

```bash
git clone git@github.com:roviix/nexus.git
cd nexus/apps/desktop
npm ci
npm run dev            # Vite + Tauri with hot reload
npm run tauri build    # unsigned installer for the current platform
```

If you only want to look at the UI without installing a Rust toolchain, `ui-preview` swaps the Tauri
layer for mocks and runs in a plain browser:

```bash
cd apps/desktop && npm run preview:ui   # http://127.0.0.1:1500/?route=overview
```

There are also scripts that build only what makes sense on the current platform:

```bash
scripts/build-dmg.sh                                              # macOS, produces a dmg (signed with a local certificate)
scripts/build-dmg.sh --install                                    # macOS, install into /Applications when done
powershell -ExecutionPolicy Bypass -File scripts\build-windows.ps1 # Windows, produces an nsis installer
```

The macOS script **signs the build** (`scripts/macos-signing-identity.sh` picks a certificate from
your keychain, or generates a self-signed one). This matters: an unsigned bundle cannot obtain the
system "App Management" permission, which means the Sand patch cannot be installed. Signing is not
notarisation — such a build only works on your own machine.

The checks CI runs (all of them must pass on macOS/Linux **and** Windows):

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cd apps/desktop && npm run typecheck && npm test
node scripts/test-package-release.mjs
```

Platform-specific branches (`tasklist` / `taskkill`, `%APPDATA%` paths, read-only attributes) never
get compiled on Linux, so CI has a separate `check-windows` job running the same clippy and tests.
Without it, "CI is green" would only mean "it works on macOS".

### Environment variables for troubleshooting

| Variable | Effect |
|---|---|
| `CURSOR_USER_DIR` | Override the Cursor data directory (also settable in the app) |
| `CURSOR_STATE_DB` | Point at a specific `state.vscdb` — aim it at a copy to test switching safely |
| `CURSOR_APP_PATH` | Override where the Cursor application itself lives (also settable in the app) |
| `SAND_INFERENCE_ENDPOINT` | Reroute Cursor's inference to this endpoint when installing the Sand patch |
| `NEXUS_CRSR_CREDENTIAL_FILE` | Point the CRSR credential file elsewhere. Nexus and the patched Cursor are separate processes, so it has to be set where both can see it (see [`docs/CRSR.md`](./docs/CRSR.md) §7) |
| `NEXUS_PASSTHROUGH_DUMP_DIR` | Dump inbound inference request bodies verbatim into this directory (disables streaming; the dumps are plaintext business data — delete them when you're done) |

### Where the data lives

| | Data and logs | Cursor data directory |
|---|---|---|
| macOS | `~/Library/Application Support/com.roviix.nexus/`, `~/Library/Logs/com.roviix.nexus/` | `~/Library/Application Support/Cursor` |
| Windows | `%APPDATA%\com.roviix.nexus\` and its `logs\` | `%APPDATA%\Cursor` |

All credentials sit in `nexus.db` under the app data directory, in plaintext, not in the OS
keychain. On macOS the directory is `0700` and the file is `0600`; on Windows no permissions are
changed and protection relies on the default ACL of `%APPDATA%`. This is a deliberate trade-off,
not an oversight — the reasoning is in [`docs/ARCHITECTURE.md`](./docs/ARCHITECTURE.md) §9.1.

Whole-database moves go through `~/.roviix/backups`. To move accounts only, export from the accounts
page into `~/.roviix/exports` and import on the other machine. **Export files contain plaintext
credentials — delete them when you're done.**

## Repository layout

```
.
├── Cargo.toml                      # workspace
├── crates/
│   ├── nexus-core/                 # domain types / errors / ids / time (no IO)
│   ├── nexus-store/                # SQLite (data + secrets) + migrations + activity log + settings
│   ├── nexus-cursor/               # locating Cursor, reading/writing state.vscdb and machine IDs, process control
│   ├── nexus-switcher/             # switch book, backups, switch orchestration
│   ├── nexus-accounts/             # account pool, OAuth, token refresh, usage
│   ├── nexus-chatgpt/              # ChatGPT subscriptions: login, refresh, Codex backend
│   ├── nexus-grok/ nexus-grokbot/  # Grok accounts and Grok Bot quota
│   ├── nexus-kiro/                 # Kiro accounts
│   ├── nexus-gateway/              # the local gateway: dialect port, passthrough port, quota relay, ledger
│   ├── nexus-connect/              # one-click setup for Claude Code / Codex / OpenCode
│   ├── nexus-playground/           # playground thread and message storage
│   ├── nexus-sand/                 # Sand patch engine (local + remote over SSH)
│   └── nexus-crsr/                 # CRSR patch: native Agent panel on a crsr_ API key
├── apps/desktop/
│   ├── src-tauri/                  # Tauri commands, events, capabilities
│   ├── src/                        # React frontend
│   └── ui-preview/                 # browser-only UI preview with a mocked core
├── packages/design-tokens/         # CSS variables
├── scripts/                        # packaging, signing, release manifests
└── docs/                           # ARCHITECTURE.md / SAND.md / CRSR.md
```

**Dependencies only point downwards**, and `nexus-switcher` and `nexus-accounts` **do not depend on
each other** — the switch book and the account pool are two independent modules, and the Cargo
dependency graph enforces it. The only data path between them is one explicit copy inside the
`accounts_add_to_switch_book` command.

## Releasing

Push a `v*` tag (for example `v0.5.0`, which must match the version in `Cargo.toml`,
`package.json` and `tauri.conf.json`). [`release.yml`](./.github/workflows/release.yml) builds on
macOS and Windows, produces SHA-256 / MD5 sums and the `latest.json` the updater reads, and publishes
a GitHub Release. The update endpoint is
`https://github.com/roviix/nexus/releases/latest/download/latest.json`.

Required GitHub configuration:

| Name | Type | Notes |
|---|---|---|
| `TAURI_UPDATER_PUBLIC_KEY` | Variable | Updater public key (written into `tauri.conf.json`) |
| `TAURI_SIGNING_PRIVATE_KEY` / `TAURI_SIGNING_PRIVATE_KEY_PASSWORD` | Secret | Updater private key — **required** |
| `APPLE_CERTIFICATE` / `APPLE_CERTIFICATE_PASSWORD` / `APPLE_SIGNING_IDENTITY` / `APPLE_ID` / `APPLE_PASSWORD` / `APPLE_TEAM_ID` | Secret | Optional; without them macOS ships an unsigned Preview |
| `WINDOWS_CERTIFICATE_BASE64` / `WINDOWS_CERTIFICATE_PASSWORD` | Secret | Optional; without them Windows ships an unsigned Preview |
| `TAURI_UPDATER_ENDPOINT` | Variable | Optional; defaults to this repository's GitHub Releases |

With certificates for both platforms you get a **signed stable release** (eligible for auto-update);
otherwise you get a **Preview** that can only be downloaded manually.

See [CHANGELOG.md](./CHANGELOG.md) for what changed between versions.

## Risks and disclaimer

- Nexus calls each platform's **non-public client APIs** using your own subscription accounts. This
  may violate the terms of service of those platforms, and your accounts may be rate-limited,
  warned or banned. Assess that risk yourself, and **do not** use accounts you cannot afford to lose.
- The Sand and CRSR patches modify Cursor's application files. They are idempotent, reversible and
  guarded by a version check, but they are still modifications of third-party software, and you have
  to reinstall after Cursor updates. Both occupy the same hook, so only one can be installed.
- The CRSR channel bills against Cursor's **API key** pricing, which is a different ledger from your
  subscription quota. Make sure you know what you are spending before enabling it.
- This project is not affiliated with or endorsed by Cursor, OpenAI, xAI or Amazon.
- The software is provided "as is", without warranty of any kind. See [LICENSE](./LICENSE).

Other **deliberate** trade-offs (plaintext credentials, the gateway being reachable by other
processes on the same machine) are listed in [`docs/ARCHITECTURE.md`](./docs/ARCHITECTURE.md) §11.
Please read it before filing an issue.

## Contributing

See [CONTRIBUTING.md](./CONTRIBUTING.md) and the [Code of Conduct](./CODE_OF_CONDUCT.md).
The existing code and documentation are predominantly in Chinese; contributions in either language
are welcome, just don't mix the two within one paragraph. Report security issues privately as
described in [SECURITY.md](./SECURITY.md).

## License

[MIT](./LICENSE) © 2026 Roviix
