/**
 * 供应商表单用到的纯函数与预设。
 *
 * 供应商就是网关的一条通道：一把钥匙、一个兼容地址、一张模型清单。客户端怎么叫这些模型
 * （Claude Code 的四档、Codex 的一个模型）在「接入」里按客户端配，不存在供应商身上。
 */
import type { ApiFormat, AuthField, KeyProvider } from "../../ipc/types";

export type { ApiFormat, AuthField };

export function formatLabel(format: ApiFormat): string {
  switch (format) {
    case "anthropic":
      return "Anthropic";
    case "openai_chat":
      return "OpenAI Chat";
    case "openai_responses":
      return "Responses";
  }
}

/** 地址框下面那句提示：各格式填什么形状的地址。 */
export function baseUrlHint(format: ApiFormat): string {
  return format === "anthropic"
    ? "根地址，后面会接 /v1/messages。填成 …/v1 也行，会自动去掉。"
    : "OpenAI SDK 里的 base URL，一般以 /v1 结尾（智谱是 /api/paas/v4）。没写版本段时会补 /v1。";
}

/** 模型清单去空白、去 `[1m]`、去重（大小写不敏感），保留顺序。与 Rust `clean_models` 同一条规则。 */
export function cleanModels(models: string[]): string[] {
  const out: string[] = [];
  for (const raw of models) {
    const id = raw.trim().replace(/\[1m\]$/i, "").trim();
    if (!id || /\s/.test(id)) continue;
    if (out.some((m) => m.toLowerCase() === id.toLowerCase())) continue;
    out.push(id);
  }
  return out;
}

/** 一次粘进来好几个模型（逗号、空格、换行分隔）。 */
export function splitModels(text: string): string[] {
  return cleanModels(text.split(/[\s,，;；]+/));
}

/** 接入页「用它」时预选的模型：清单第一个。 */
export function primaryModel(p: Pick<KeyProvider, "models">): string {
  return p.models[0] ?? "";
}

export interface Preset {
  id: string;
  label: string;
  website: string;
  baseUrl: string;
  format: ApiFormat;
  auth: AuthField;
}

/** 常见的几家。模型清单不预填：各家改名快，点「获取模型列表」从它那里拉最准。 */
export const PRESETS: Preset[] = [
  { id: "custom", label: "自定义", website: "", baseUrl: "", format: "openai_chat", auth: "auth_token" },
  { id: "deepseek", label: "DeepSeek", website: "https://api-docs.deepseek.com", baseUrl: "https://api.deepseek.com/anthropic", format: "anthropic", auth: "auth_token" },
  { id: "kimi", label: "Kimi", website: "https://platform.moonshot.cn", baseUrl: "https://api.moonshot.cn/anthropic", format: "anthropic", auth: "auth_token" },
  { id: "zhipu", label: "智谱 GLM", website: "https://open.bigmodel.cn", baseUrl: "https://open.bigmodel.cn/api/anthropic", format: "anthropic", auth: "auth_token" },
  { id: "openrouter", label: "OpenRouter", website: "https://openrouter.ai", baseUrl: "https://openrouter.ai/api/v1", format: "openai_chat", auth: "auth_token" },
  { id: "siliconflow", label: "硅基流动", website: "https://siliconflow.cn", baseUrl: "https://api.siliconflow.cn/v1", format: "openai_chat", auth: "auth_token" },
  { id: "dashscope", label: "通义千问", website: "https://dashscope.aliyun.com", baseUrl: "https://dashscope.aliyuncs.com/compatible-mode/v1", format: "openai_chat", auth: "auth_token" },
  { id: "anthropic", label: "Anthropic", website: "https://docs.anthropic.com", baseUrl: "https://api.anthropic.com", format: "anthropic", auth: "api_key" },
  { id: "openai", label: "OpenAI", website: "https://platform.openai.com", baseUrl: "https://api.openai.com/v1", format: "openai_responses", auth: "auth_token" },
];
