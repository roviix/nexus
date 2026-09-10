/**
 * 接入配置的生成器。纯函数：给地址、钥匙、模型，吐出能直接抄进客户端的配置。
 *
 * 对客户端来说网关只是一个地址和一把钥匙，配置文件的写法与接任何 OpenAI / Anthropic
 * 兼容服务一模一样。
 */
import { homePath, PLATFORM, type Platform } from "../ui/platform";

export type Tool = "claude" | "codex" | "opencode" | "grok" | "cline" | "sdk" | "cursor_agent";
export type Protocol = "openai" | "anthropic" | "responses";
export type Lang = "curl" | "python" | "js";

export interface ToolMeta {
  id: Tool;
  label: string;
  /** 这是给谁 / 什么场景用的，一句短话。 */
  sub: string;
  /** 卡片上的一个字符标识。 */
  glyph: string;
  /** 这个工具对中转讲哪种方言。SDK 卡可切。 */
  protocol: Protocol;
  /** 走网关的透传口而不是方言口（cursor-agent 讲的是 Cursor 原生协议）。 */
  passthrough?: boolean;
  /**
   * 配置文件相对家目录的位置。写相对形式而不是 `~/…`，是因为它在 Windows 上要显示成
   * `%USERPROFILE%\…`——同一份数据两种写法，只能存不带前缀的那部分。
   * 没有配置文件的工具（填面板、写代码）为 `null`，改看 [`where`]。
   */
  configRel: string | null;
  /** 没有配置文件时，东西填在哪。 */
  where: string;
}

/** 这个工具的配置该放在哪，按当前系统写。 */
export function toolWhere(tool: ToolMeta, platform: Platform = PLATFORM): string {
  return tool.configRel ? homePath(tool.configRel, platform) : tool.where;
}

export const TOOLS: ToolMeta[] = [
  {
    id: "claude",
    label: "Claude Code",
    sub: "Anthropic 协议",
    glyph: "C",
    protocol: "anthropic",
    configRel: ".claude/settings.json",
    where: "",
  },
  {
    id: "codex",
    label: "Codex CLI",
    sub: "Responses 协议",
    glyph: "≥",
    protocol: "responses",
    configRel: ".codex/config.toml",
    where: "",
  },
  {
    id: "opencode",
    label: "OpenCode",
    sub: "OpenAI 兼容",
    glyph: "○",
    protocol: "openai",
    configRel: ".config/opencode/opencode.json",
    where: "",
  },
  {
    id: "grok",
    label: "Grok CLI",
    sub: "Responses 协议",
    glyph: "G",
    protocol: "responses",
    configRel: ".grok/config.toml",
    where: "",
  },
  {
    id: "cline",
    label: "Cline / 其他兼容客户端",
    sub: "OpenAI 兼容",
    glyph: "◇",
    protocol: "openai",
    configRel: null,
    where: "API Provider",
  },
  {
    id: "sdk",
    label: "SDK / cURL",
    sub: "自己写代码",
    glyph: "{}",
    protocol: "openai",
    configRel: null,
    where: "代码示例",
  },
  {
    id: "cursor_agent",
    label: "cursor-agent CLI",
    sub: "Cursor 协议",
    glyph: "▶",
    protocol: "openai",
    passthrough: true,
    configRel: null,
    where: "终端命令",
  },
];

export function toolMeta(id: Tool): ToolMeta {
  return TOOLS.find((t) => t.id === id)!;
}

/** 一个号源对外的几个地址。`passthrough` 只有本地网关有。 */
export interface Endpoint {
  /** 不带 `/v1` 的根地址：Anthropic 协议填这个。 */
  root: string;
  /** 带 `/v1`：OpenAI / Responses 协议填这个。 */
  v1: string;
  /** cursor-agent 透传口（本地网关专有）。 */
  passthrough?: string;
}

/** 去掉尾部斜杠和多余的 `/v1`，再拼出 root / v1 两个地址。 */
export function endpointOf(baseUrl: string, passthrough?: string | null): Endpoint {
  const root = baseUrl.trim().replace(/\/+$/, "").replace(/\/v1$/, "");
  return { root, v1: `${root}/v1`, ...(passthrough ? { passthrough: passthrough.replace(/\/+$/, "") } : {}) };
}

export const PROTOCOL_INFO: Record<Protocol, { label: string; path: string; base: "v1" | "root" }> = {
  openai: { label: "OpenAI Chat", path: "/v1/chat/completions", base: "v1" },
  anthropic: { label: "Anthropic", path: "/v1/messages", base: "root" },
  responses: { label: "Responses", path: "/v1/responses", base: "v1" },
};

/** 某种协议要填的 Base URL。 */
export function protocolBase(p: Protocol, ep: Endpoint): string {
  return PROTOCOL_INFO[p].base === "root" ? ep.root : ep.v1;
}

// ── 各工具的配置 ─────────────────────────────────────────────────────────────

/**
 * Claude Code 的模型选择是一组槽位，不是一个变量：UI 里切 Opus / Sonnet / Haiku 各走各的，
 * 后台标题生成、文件摘要还另走 SMALL_FAST。任何一个没钉死，它就会发 Anthropic 官方模型名，
 * 中转一律 BAD_MODEL_NAME。新版读 `*_MODEL_NAME`、老版读 `*_MODEL`，两套都写才不用管版本。
 */
export function claudeSettings(ep: Endpoint, key: string, model: string): string {
  return JSON.stringify(
    {
      env: {
        ANTHROPIC_BASE_URL: ep.root,
        ANTHROPIC_AUTH_TOKEN: key,
        ANTHROPIC_MODEL: model,
        ANTHROPIC_SMALL_FAST_MODEL: model,
        ANTHROPIC_DEFAULT_OPUS_MODEL: model,
        ANTHROPIC_DEFAULT_OPUS_MODEL_NAME: model,
        ANTHROPIC_DEFAULT_SONNET_MODEL: model,
        ANTHROPIC_DEFAULT_SONNET_MODEL_NAME: model,
        ANTHROPIC_DEFAULT_HAIKU_MODEL: model,
        ANTHROPIC_DEFAULT_HAIKU_MODEL_NAME: model,
      },
    },
    null,
    2,
  );
}

/**
 * 密钥内联进 config.toml：这是 Codex 三条取钥匙的路里唯一不依赖终端环境、也不会误发
 * ChatGPT 登录态的一条（`requires_openai_auth = false` 就是为了后者）。0.46 及更早只认
 * `auth.json`，所以那份另给一段当兜底。
 */
export function codexToml(ep: Endpoint, key: string, model: string): string {
  return [
    `model_provider = "nexus"`,
    `model = "${model}"`,
    "",
    "[model_providers.nexus]",
    `name = "nexus"`,
    `base_url = "${ep.v1}"`,
    `wire_api = "responses"`,
    `requires_openai_auth = false`,
    `experimental_bearer_token = "${key}"`,
  ].join("\n");
}

export function codexAuth(key: string): string {
  return JSON.stringify({ OPENAI_API_KEY: key }, null, 2);
}

/**
 * 只改内置 `openai` provider 的地址和钥匙，模型写成 `openai/{id}`。
 * 不发明自定义 provider——否则丢掉 OpenCode 自带的模型元数据。
 */
export function opencodeJson(ep: Endpoint, key: string, model: string): string {
  return (
    JSON.stringify(
      {
        provider: {
          openai: {
            options: {
              baseURL: ep.v1,
              apiKey: key,
            },
          },
        },
        model: `openai/${model}`,
      },
      null,
      2,
    ) + "\n"
  );
}

/** 对齐 Grok CLI / CLIProxyAPI 的 named model，走 Responses。 */
export function grokToml(ep: Endpoint, key: string, model: string): string {
  return [
    `[models]`,
    `default = "grok"`,
    "",
    `[model.grok]`,
    `model = "${model}"`,
    `base_url = "${ep.v1}"`,
    `api_key = "${key}"`,
    `api_backend = "responses"`,
  ].join("\n");
}

export interface Field {
  label: string;
  value: string;
}

/** Cline 这类只有设置面板的客户端：一行一个字段，各自能复制。 */
export function clineFields(ep: Endpoint, key: string, model: string): Field[] {
  return [
    { label: "API Provider", value: "OpenAI Compatible" },
    { label: "Base URL", value: ep.v1 },
    { label: "API Key", value: key },
    { label: "Model ID", value: model },
  ];
}

/**
 * cursor-agent 透传（模式⑤）的示例命令：`-e` 和 `--agent-endpoint` 都要指到透传端口，
 * 缺一个 agentic 主循环还是会硬拨官方 api5（见 `service.rs` 的入口说明）。
 */
export function passthroughAgentExample(passthroughBaseUrl: string, prompt = "你好"): string {
  return `cursor-agent -e ${passthroughBaseUrl} --agent-endpoint ${passthroughBaseUrl} --model auto --print "${prompt}"`;
}

/**
 * 一段能贴进 shell 的环境变量。`cursor_agent` 的透传口没有对应的环境变量，只给 `-e` 那个。
 *
 * Windows 上给 PowerShell 的写法：`export` 在 PowerShell 里根本不是命令，照抄过去
 * 只会得到一句 "not recognized"，而这几行正是用户最可能整段复制的东西。
 */
export function envLines(
  tool: Tool,
  ep: Endpoint,
  key: string,
  model: string,
  platform: Platform = PLATFORM,
): string[] {
  const pairs: Array<[string, string]> =
    tool === "claude"
      ? [
          ["ANTHROPIC_BASE_URL", ep.root],
          ["ANTHROPIC_AUTH_TOKEN", key],
          ["ANTHROPIC_MODEL", model],
        ]
      : tool === "cursor_agent"
        ? ep.passthrough
          ? [["CURSOR_API_ENDPOINT", ep.passthrough]]
          : []
        : [
            ["OPENAI_BASE_URL", ep.v1],
            ["OPENAI_API_KEY", key],
          ];
  return pairs.map(([k, v]) =>
    platform === "windows" ? `$env:${k} = "${v}"` : `export ${k}=${v}`,
  );
}

// ── SDK / cURL ───────────────────────────────────────────────────────────────

/**
 * 拼一条 curl。两个系统的差别不止换行符：
 *
 * - `curl.exe`：PowerShell 5.1 把 `curl` 别名成 `Invoke-WebRequest`，参数完全不通用。
 *   写全名才绕得过去，而且在 cmd 里也一样能跑。
 * - 反斜杠续行是 POSIX shell 的写法，PowerShell 的续行符是反引号。与其教用户换符号，
 *   不如在 Windows 上直接给一整行——反正是复制粘贴，不是给人读的排版。
 * - JSON body 不能用单引号：PowerShell / cmd 里单引号不阻止解析，双引号又会被外层吃掉，
 *   所以整体用双引号、内部的双引号用反斜杠转义。
 */
function curl(p: Protocol, ep: Endpoint, key: string, model: string, platform: Platform): string {
  const win = platform === "windows";
  const [url, headers, payload] =
    p === "anthropic"
      ? [
          `${ep.root}/v1/messages`,
          [`x-api-key: ${key}`, "anthropic-version: 2023-06-01", "Content-Type: application/json"],
          JSON.stringify({ model, max_tokens: 1024, messages: [{ role: "user", content: "ping" }] }),
        ]
      : p === "responses"
        ? [
            `${ep.v1}/responses`,
            [`Authorization: Bearer ${key}`, "Content-Type: application/json"],
            JSON.stringify({ model, input: "ping" }),
          ]
        : [
            `${ep.v1}/chat/completions`,
            [`Authorization: Bearer ${key}`, "Content-Type: application/json"],
            JSON.stringify({ model, messages: [{ role: "user", content: "ping" }] }),
          ];

  const body = win ? `"${payload.replace(/"/g, '\\"')}"` : `'${payload}'`;
  const parts = [
    `${win ? "curl.exe" : "curl"} -sS ${url}`,
    ...headers.map((h) => `-H "${h}"`),
    `-d ${body}`,
  ];
  return win ? parts.join(" ") : parts.join(" \\\n  ");
}

function python(p: Protocol, ep: Endpoint, key: string, model: string): string {
  if (p === "anthropic") {
    return [
      "import anthropic",
      "",
      `client = anthropic.Anthropic(api_key="${key}", base_url="${ep.root}")`,
      "r = client.messages.create(",
      `    model="${model}", max_tokens=1024,`,
      '    messages=[{"role": "user", "content": "ping"}],',
      ")",
      "print(r.content[0].text)",
    ].join("\n");
  }
  if (p === "responses") {
    return [
      "from openai import OpenAI",
      "",
      `client = OpenAI(base_url="${ep.v1}", api_key="${key}")`,
      `r = client.responses.create(model="${model}", input="ping")`,
      "print(r.output_text)",
    ].join("\n");
  }
  return [
    "from openai import OpenAI",
    "",
    `client = OpenAI(base_url="${ep.v1}", api_key="${key}")`,
    "r = client.chat.completions.create(",
    `    model="${model}",`,
    '    messages=[{"role": "user", "content": "ping"}],',
    ")",
    "print(r.choices[0].message.content)",
  ].join("\n");
}

function js(p: Protocol, ep: Endpoint, key: string, model: string): string {
  if (p === "anthropic") {
    return [
      'import Anthropic from "@anthropic-ai/sdk";',
      "",
      `const client = new Anthropic({ apiKey: "${key}", baseURL: "${ep.root}" });`,
      "const r = await client.messages.create({",
      `  model: "${model}", max_tokens: 1024,`,
      '  messages: [{ role: "user", content: "ping" }],',
      "});",
      "console.log(r.content[0].text);",
    ].join("\n");
  }
  if (p === "responses") {
    return [
      'import OpenAI from "openai";',
      "",
      `const client = new OpenAI({ baseURL: "${ep.v1}", apiKey: "${key}" });`,
      `const r = await client.responses.create({ model: "${model}", input: "ping" });`,
      "console.log(r.output_text);",
    ].join("\n");
  }
  return [
    'import OpenAI from "openai";',
    "",
    `const client = new OpenAI({ baseURL: "${ep.v1}", apiKey: "${key}" });`,
    "const r = await client.chat.completions.create({",
    `  model: "${model}",`,
    '  messages: [{ role: "user", content: "ping" }],',
    "});",
    "console.log(r.choices[0].message.content);",
  ].join("\n");
}

export function sdkSnippet(
  lang: Lang,
  p: Protocol,
  ep: Endpoint,
  key: string,
  model: string,
  platform: Platform = PLATFORM,
): string {
  if (lang === "python") return python(p, ep, key, model);
  if (lang === "js") return js(p, ep, key, model);
  return curl(p, ep, key, model, platform);
}

export const LANG_LABEL: Record<Lang, string> = { curl: "cURL", python: "Python", js: "JavaScript" };

/** 代码块上那行小标题：命令片段是给哪个 shell 的。 */
export function shellLabel(platform: Platform = PLATFORM): string {
  return platform === "windows" ? "powershell" : "shell";
}

/** 钥匙还没显示出来时配置里摆的占位。刻意长得像钥匙、又一眼看出不是。 */
export const KEY_PLACEHOLDER = "<在上面点「显示」取钥匙>";
