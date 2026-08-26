function parseScenario() {
  const raw = process.env.CLAUDE_AGENT_SDK_MOCK_SCENARIO;
  if (!raw) {
    return { steps: [] };
  }
  return JSON.parse(raw);
}

function clone(value) {
  if (value == null) {
    return value;
  }
  return JSON.parse(JSON.stringify(value));
}

export function tool(name, description, inputSchema, handler) {
  const isZodSchema =
    inputSchema && typeof inputSchema.safeParse === "function";
  const isZodRawShape =
    inputSchema &&
    typeof inputSchema === "object" &&
    !Array.isArray(inputSchema) &&
    Object.values(inputSchema).every(
      (field) => field && typeof field.safeParse === "function",
    );
  if (!isZodSchema && !isZodRawShape) {
    throw new Error(
      "inputSchema must be a Zod schema or raw shape, received an unrecognized object",
    );
  }
  return { name, description, inputSchema, handler };
}

export function createSdkMcpServer({ name, version, tools }) {
  return { name, version, tools };
}

function defaultResult(partial = {}) {
  return {
    type: "result",
    subtype: "success",
    is_error: false,
    duration_ms: 0,
    duration_api_ms: 0,
    num_turns: 1,
    stop_reason: null,
    total_cost_usd: 0,
    usage: {},
    modelUsage: {},
    errors: [],
    session_id: "mock-session",
    ...clone(partial),
  };
}

async function runHooks(options, hookName, input) {
  const hookEntries = options?.hooks?.[hookName] ?? [];
  for (const entry of hookEntries) {
    for (const hook of entry?.hooks ?? []) {
      await hook(clone(input));
    }
  }
}

export function query({ prompt, options }) {
  const scenario = parseScenario();
  let closed = false;
  // 模拟 Claude query 当前运行配置，供复用会话测试验证每轮更新。
  let currentModel = options?.model;
  let currentEffort = options?.effort ?? null;
  const runtimeControlCalls = [];

  const iterator = (async function* () {
    const observations = [];

    if (scenario.persistentInput === true && typeof prompt !== "string") {
      let initialized = false;
      for await (const userMessage of prompt) {
        if (closed) {
          break;
        }
        if (!initialized) {
          initialized = true;
          yield {
            type: "system",
            subtype: "init",
            session_id: scenario.sessionId ?? "mock-session",
          };
        }
        const content = userMessage?.message?.content;
        const text = typeof content === "string"
          ? content
          : Array.isArray(content)
            ? content
                .filter((block) => block?.type === "text" && typeof block.text === "string")
                .map((block) => block.text)
                .join("")
            : "";
        const result = scenario.emitPersistentRuntimeState === true
          ? JSON.stringify({
              text,
              currentModel: currentModel ?? null,
              currentEffort,
              runtimeControlCalls: clone(runtimeControlCalls),
            })
          : text;
        yield defaultResult({
          result,
          session_id: scenario.sessionId ?? "mock-session",
        });
      }
      return;
    }

    if (scenario.emitQueryOptions) {
      observations.push({
        type: "query_options",
        result: clone({
          permissionMode: options?.permissionMode,
          settings: options?.settings,
          sandbox: clone(options?.sandbox),
          settingSources: clone(options?.settingSources),
          allowedTools: options?.allowedTools,
          mcpServers: Object.fromEntries(
            Object.entries(options?.mcpServers ?? {}).map(([name, server]) => [
              name,
              {
                name: server?.name,
                version: server?.version,
                tools: server?.tools?.map((candidate) => candidate.name),
              },
            ]),
          ),
        }),
      });
    }

    for (const step of scenario.steps ?? []) {
      if (closed) {
        break;
      }

      if (step.type === "yield") {
        yield clone(step.message);
        continue;
      }

      if (step.type === "delay") {
        await new Promise((resolve) => setTimeout(resolve, step.durationMs ?? 0));
        continue;
      }

      if (step.type === "hook") {
        await runHooks(options, step.hook, step.input);
        continue;
      }

      if (step.type === "permission") {
        const permission = await options.canUseTool(
          step.toolName,
          clone(step.input ?? {}),
          {
            signal: new AbortController().signal,
            toolUseID: step.toolUseID ?? "mock-tool-use",
            ...clone(step.options ?? {}),
          },
        );
        observations.push({
          type: "permission_result",
          result: clone(permission),
        });
        continue;
      }

      if (step.type === "computer_control_tool") {
        const server = options?.mcpServers?.["auracoder-computer-control"];
        const definition = server?.tools?.find((candidate) => candidate.name === step.toolName);
        if (!definition) {
          throw new Error(`Computer control tool not found: ${step.toolName}`);
        }
        const result = await definition.handler(clone(step.input ?? {}), {
          signal: new AbortController().signal,
          toolUseID: step.callId ?? `mock-call-${step.toolName}`,
        });
        observations.push({
          type: "computer_control_result",
          result: clone(result),
        });
        continue;
      }
    }

    if (scenario.emitObservationResult) {
      yield defaultResult({
        result: JSON.stringify(observations),
        session_id: scenario.sessionId ?? "mock-session",
      });
    }
  })();

  iterator.close = () => {
    closed = true;
  };
  iterator.setModel = async (model) => {
    if (scenario.failSetModel === true) {
      throw new Error("Mock Claude query setModel failed.");
    }
    runtimeControlCalls.push({ type: "set_model", value: model ?? null });
    currentModel = model;
  };
  iterator.applyFlagSettings = async (settings) => {
    if (scenario.failApplyFlagSettings === true) {
      throw new Error("Mock Claude query applyFlagSettings failed.");
    }
    const effortLevel = settings?.effortLevel ?? null;
    runtimeControlCalls.push({ type: "apply_flag_settings", value: effortLevel });
    currentEffort = effortLevel;
  };
  iterator.interrupt = async () => undefined;
  iterator.streamInput = async (stream) => {
    for await (const _message of stream) {
      if (closed) break;
    }
  };
  iterator.supportedModels = async () => {
    const models = scenario.models ?? [
      {
        value: "default",
        displayName: "Default (recommended)",
        description: "Default Claude model",
        supportsEffort: true,
        supportedEffortLevels: ["low", "medium", "high"],
      },
    ];
    if (Array.isArray(scenario.expectedSupportedModelsSettingSources)) {
      const expected = scenario.expectedSupportedModelsSettingSources;
      const actual = options?.settingSources;
      if (JSON.stringify(actual) !== JSON.stringify(expected)) {
        throw new Error(
          `Unexpected supportedModels settingSources: expected ${JSON.stringify(expected)}, actual ${JSON.stringify(actual)}`,
        );
      }
    }
    return clone(models);
  };

  return iterator;
}
