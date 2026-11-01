import type { CodexPersonalityValue, CodexServiceTierValue } from "./CodexConfigPicker";
import type { Thread } from "../../types";
import { serializePrettyJson } from "./formatters";

/** 读取线程保存的人格选择，缺省时使用继承值。 */
export function readThreadPersonalityValue(thread: Thread | null): CodexPersonalityValue {
  const value = thread?.engineMetadata?.personality;
  if (value === "none" || value === "friendly" || value === "pragmatic") {
    return value;
  }
  return "inherit";
}

/** 读取线程保存的服务档位，缺省时使用继承值。 */
export function readThreadServiceTierValue(thread: Thread | null): CodexServiceTierValue {
  const value = thread?.engineMetadata?.serviceTier;
  if (value === "fast" || value === "flex") {
    return value;
  }
  return "inherit";
}

/** 将线程保存的输出结构转换为配置编辑器文本。 */
export function readThreadOutputSchemaText(thread: Thread | null): string {
  const value = thread?.engineMetadata?.outputSchema;
  if (value === undefined || value === null) {
    return "";
  }
  return serializePrettyJson(value);
}

/** 读取线程配置的 OpenCode agent，未保存时使用 build agent。 */
export function readThreadOpenCodeAgentValue(thread: Thread | null): string {
  const value = thread?.engineMetadata?.opencodeAgent;
  return typeof value === "string" && value.trim() ? value.trim() : "build";
}

/** 验证并解析线程配置的 JSON Schema 对象或布尔值。 */
export function parseStoredOutputSchema(
  text: string,
): Record<string, unknown> | boolean | null {
  const normalized = text.trim();
  if (!normalized) {
    return null;
  }

  // 输出结构只允许 JSON Schema 对象或布尔值。
  const parsed = JSON.parse(normalized) as unknown;
  if (
    typeof parsed !== "boolean" &&
    (!parsed || typeof parsed !== "object" || Array.isArray(parsed))
  ) {
    throw new Error("output schema must be a JSON Schema object or boolean");
  }

  return parsed as Record<string, unknown> | boolean;
}

/** 读取线程保存的最后模型标识，清除空白值。 */
export function readThreadLastModelId(thread: { modelId?: string | null }): string | null {
  const normalized = thread.modelId?.trim();
  return normalized && normalized.length > 0 ? normalized : null;
}
