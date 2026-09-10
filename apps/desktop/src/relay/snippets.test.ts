/**
 * 配置生成器是用户会**逐字照抄**的东西，这里钉住几条抄错就跑不起来的规则。
 */
import { describe, expect, it } from "vitest";
import {
  claudeSettings,
  clineFields,
  codexToml,
  endpointOf,
  envLines,
  grokToml,
  opencodeJson,
  passthroughAgentExample,
  protocolBase,
  sdkSnippet,
  shellLabel,
  toolMeta,
  toolWhere,
  TOOLS,
} from "./snippets";

const LOCAL = endpointOf("http://127.0.0.1:8787", "http://127.0.0.1:8788");
const CLOUD = endpointOf("https://relay.example.com/v1");

describe("endpointOf", () => {
  it("normalises trailing slashes and a stray /v1", () => {
    expect(CLOUD).toEqual({ root: "https://relay.example.com", v1: "https://relay.example.com/v1" });
    expect(endpointOf("http://127.0.0.1:8787/")).toEqual({ root: "http://127.0.0.1:8787", v1: "http://127.0.0.1:8787/v1" });
    expect(LOCAL.passthrough).toBe("http://127.0.0.1:8788");
  });

  it("Anthropic takes the root, OpenAI-style protocols take /v1", () => {
    expect(protocolBase("anthropic", LOCAL)).toBe("http://127.0.0.1:8787");
    expect(protocolBase("openai", LOCAL)).toBe("http://127.0.0.1:8787/v1");
    expect(protocolBase("responses", CLOUD)).toBe("https://relay.example.com/v1");
  });
});

describe("Claude Code settings", () => {
  it("pins every model slot, old and new names alike", () => {
    const env = JSON.parse(claudeSettings(CLOUD, "sk-x", "claude-sonnet-5")).env as Record<string, string>;
    expect(env.ANTHROPIC_BASE_URL).toBe("https://relay.example.com");
    expect(env.ANTHROPIC_AUTH_TOKEN).toBe("sk-x");
    for (const k of Object.keys(env).filter((k) => k.includes("MODEL"))) expect(env[k]).toBe("claude-sonnet-5");
    expect(env.ANTHROPIC_DEFAULT_OPUS_MODEL_NAME).toBeDefined();
    expect(env.ANTHROPIC_DEFAULT_OPUS_MODEL).toBeDefined();
  });
});

describe("Codex config", () => {
  it("inlines the key and refuses ChatGPT auth", () => {
    const toml = codexToml(LOCAL, "nx-local-1", "gpt-5.6-sol");
    expect(toml).toContain('base_url = "http://127.0.0.1:8787/v1"');
    expect(toml).toContain('wire_api = "responses"');
    expect(toml).toContain("requires_openai_auth = false");
    expect(toml).toContain('experimental_bearer_token = "nx-local-1"');
    expect(toml).toContain('model = "gpt-5.6-sol"');
  });
});

describe("cursor-agent passthrough", () => {
  it("points both -e and --agent-endpoint at the passthrough base url", () => {
    const cmd = passthroughAgentExample("http://127.0.0.1:8788");
    expect(cmd).toBe('cursor-agent -e http://127.0.0.1:8788 --agent-endpoint http://127.0.0.1:8788 --model auto --print "你好"');
    expect(passthroughAgentExample("http://127.0.0.1:9999", "hi")).toContain('--print "hi"');
  });

  it("is a local-only tool", () => {
    expect(TOOLS.find((t) => t.id === "cursor_agent")!.passthrough).toBe(true);
    expect(envLines("cursor_agent", CLOUD, "k", "m")).toEqual([]);
    expect(envLines("cursor_agent", LOCAL, "k", "m")).toEqual(["export CURSOR_API_ENDPOINT=http://127.0.0.1:8788"]);
  });
});

/**
 * 这几条盯的是同一件事：Windows 用户复制走的东西，在 PowerShell 里能原样跑。
 * bash 语法贴进 PowerShell 不会「大致能用」，是直接报错。
 */
describe("Windows 上的配置与命令", () => {
  it("环境变量用 PowerShell 的写法，不是 export", () => {
    expect(envLines("claude", CLOUD, "sk-x", "m", "windows")).toEqual([
      '$env:ANTHROPIC_BASE_URL = "https://relay.example.com"',
      '$env:ANTHROPIC_AUTH_TOKEN = "sk-x"',
      '$env:ANTHROPIC_MODEL = "m"',
    ]);
    // 其它系统保持原样。
    expect(envLines("claude", CLOUD, "sk-x", "m", "macos")[0]).toBe(
      "export ANTHROPIC_BASE_URL=https://relay.example.com",
    );
  });

  it("配置文件路径写成 %USERPROFILE% 形式", () => {
    expect(toolWhere(toolMeta("claude"), "windows")).toBe("%USERPROFILE%\\.claude\\settings.json");
    expect(toolWhere(toolMeta("codex"), "windows")).toBe("%USERPROFILE%\\.codex\\config.toml");
    expect(toolWhere(toolMeta("claude"), "macos")).toBe("~/.claude/settings.json");
    // 没有配置文件的工具返回简洁的位置名。
    expect(toolWhere(toolMeta("cline"), "windows")).toBe("API Provider");
  });

  it("curl 用 curl.exe、单行、双引号包 JSON", () => {
    const cmd = sdkSnippet("curl", "openai", CLOUD, "k", "m", "windows");
    // PowerShell 5.1 把 curl 别名成 Invoke-WebRequest，必须写全名绕开。
    expect(cmd.startsWith("curl.exe ")).toBe(true);
    // 反斜杠续行是 POSIX 的写法，PowerShell 不认。
    expect(cmd).not.toContain("\\\n");
    expect(cmd.split("\n")).toHaveLength(1);
    // 单引号在 PowerShell / cmd 里包不住 JSON。
    expect(cmd).not.toContain("-d '");
    expect(cmd).toContain('-d "{\\"model\\":\\"m\\"');
    expect(cmd).toContain("https://relay.example.com/v1/chat/completions");
  });

  it("POSIX 那份保持多行续行不变", () => {
    const cmd = sdkSnippet("curl", "anthropic", CLOUD, "k", "m", "macos");
    expect(cmd).toContain(" \\\n");
    expect(cmd).toContain("-d '{");
    expect(cmd.startsWith("curl -sS ")).toBe(true);
  });

  it("代码块标题说明这是给哪个 shell 的", () => {
    expect(shellLabel("windows")).toBe("powershell");
    expect(shellLabel("macos")).toBe("shell");
  });
});

describe("SDK snippets", () => {
  it("use the right base per protocol in every language", () => {
    expect(sdkSnippet("curl", "anthropic", CLOUD, "k", "m")).toContain("https://relay.example.com/v1/messages");
    expect(sdkSnippet("python", "openai", LOCAL, "k", "m")).toContain('base_url="http://127.0.0.1:8787/v1"');
    expect(sdkSnippet("js", "responses", CLOUD, "k", "m")).toContain('baseURL: "https://relay.example.com/v1"');
    expect(sdkSnippet("js", "anthropic", LOCAL, "k", "m")).toContain('baseURL: "http://127.0.0.1:8787"');
  });

  it("Cline gets four copyable fields", () => {
    expect(clineFields(LOCAL, "k", "m").map((f) => f.label)).toEqual(["API Provider", "Base URL", "API Key", "Model ID"]);
  });
});

describe("OpenCode / Grok CLI", () => {
  it("points OpenCode at the built-in openai provider, not a custom one", () => {
    const json = JSON.parse(opencodeJson(CLOUD, "nx-1", "grok-4.5"));
    expect(json.provider.openai.options.baseURL).toBe("https://relay.example.com/v1");
    expect(json.provider.openai.options.apiKey).toBe("nx-1");
    expect(json.model).toBe("openai/grok-4.5");
    expect(json.provider.nexus).toBeUndefined();
  });

  it("writes a named Responses model for Grok CLI", () => {
    const toml = grokToml(LOCAL, "nx-local-1", "grok-4.5");
    expect(toml).toContain('default = "grok"');
    expect(toml).toContain("[model.grok]");
    expect(toml).toContain('model = "grok-4.5"');
    expect(toml).toContain('base_url = "http://127.0.0.1:8787/v1"');
    expect(toml).toContain('api_key = "nx-local-1"');
    expect(toml).toContain('api_backend = "responses"');
  });
});
