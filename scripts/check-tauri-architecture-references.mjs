#!/usr/bin/env node
/**
 * 校验 src-tauri 全部代码文件的架构引用关系和调用方向。
 * 脚本只检查引用位置与层级方向，不检查业务算法、业务结果或运行时行为。
 */

import { strict as assert } from "node:assert";
import { execFileSync } from "node:child_process";
import { existsSync, readdirSync, readFileSync } from "node:fs";
import { basename, extname, join, relative } from "node:path";

/**
 * 固定核心调用链，确保脚本输出和后端架构文档使用同一业务语义。
 */
const CORE_CALL_CHAIN = Object.freeze([
  "CLI 生命周期调用 SSH 隧道生命周期来创建 CLI",
  "CLI 实现类调用 CLI 生命周期获取 CLI 句柄",
  "接口定义 CLI 业务，CLI 实现类实现接口",
  "业务调用通过工厂获取对应 CLI 实现类，再调用业务函数",
]);

/**
 * 需要递归检查的代码扩展名；其它文件视为文档、资源或二进制文件跳过。
 */
const CODE_EXTENSIONS = new Set([
  ".rs",
  ".js",
  ".mjs",
  ".cjs",
  ".ts",
  ".tsx",
  ".jsx",
  ".sh",
  ".bash",
  ".zsh",
  ".ps1",
  ".py",
]);

/**
 * 不进入扫描的目录名称，避免把编译产物、依赖、文档和资源当作源码。
 */
const SKIPPED_DIRECTORY_NAMES = new Set([
  ".git",
  "target",
  "node_modules",
  "doc",
  "docs",
  "documentation",
  "image",
  "images",
]);

/**
 * 将规则定义为统一对象，保证每个命中结果都能给出编号和修复方向。
 * @param {string} rule 规则编号。
 * @param {RegExp} pattern 用于识别引用的正则表达式。
 * @param {string} direction 违规后的修复方向。
 * @returns {{rule: string, pattern: RegExp, direction: string}} 规则对象。
 */
function defineRule(rule, pattern, direction) {
  return {
    // 规则编号，用于定位违反的架构边界。
    rule,
    // 只匹配已清理的代码区域，避免注释和字符串造成误报。
    pattern,
    // 面向业务层级的修复指导。
    direction,
  };
}

/**
 * 将文件路径标准化为仓库相对路径格式。
 * @param {string} filePath 待标准化的路径。
 * @returns {string} 使用正斜杠的路径。
 */
function normalizePath(filePath) {
  return filePath.replaceAll("\\", "/").replace(/^\.\//, "");
}

/**
 * 获取当前 worktree 的 Git 仓库根目录。
 * @returns {string} 仓库根目录绝对路径。
 */
function getRepositoryRoot() {
  return execFileSync("git", ["rev-parse", "--show-toplevel"], {
    // 使用 UTF-8 读取 Git 返回的仓库根目录。
    encoding: "utf8",
    // 不向终端输出 Git 的中间结果，只返回根目录文本。
    stdio: ["ignore", "pipe", "ignore"],
  }).trim();
}

/**
 * 判断扩展名是否属于可执行源码，文档、图片、压缩包和二进制不会进入匹配。
 * @param {string} filePath 文件路径。
 * @returns {boolean} 是否属于源码扩展名。
 */
function isCodeFile(filePath) {
  return CODE_EXTENSIONS.has(extname(filePath).toLowerCase());
}

/**
 * 递归收集 src-tauri 下的全部代码文件，并跳过编译产物、依赖和非源码目录。
 * @param {string} sourceRoot src-tauri 根目录，用于限定根目录下的精确排除路径。
 * @param {string} directory 当前遍历目录。
 * @param {string[]} files 累积的绝对路径列表。
 * @returns {string[]} 收集到的代码文件绝对路径。
 */
function collectCodeFiles(sourceRoot, directory = sourceRoot, files = []) {
  if (!existsSync(directory)) {
    return files;
  }

  for (const entry of readdirSync(directory, { withFileTypes: true })) {
    if (
      directory === sourceRoot &&
      entry.isDirectory() &&
      entry.name.toLowerCase() === "tests"
    ) {
      // 只跳过 src-tauri/tests 及其子目录，保留其它层级名为 tests 的源码目录扫描。
      continue;
    }

    if (entry.isDirectory() && SKIPPED_DIRECTORY_NAMES.has(entry.name.toLowerCase())) {
      continue;
    }

    const entryPath = join(directory, entry.name);
    if (entry.isDirectory()) {
      collectCodeFiles(sourceRoot, entryPath, files);
      continue;
    }

    if (entry.isFile() && isCodeFile(entryPath)) {
      files.push(entryPath);
    }
  }
  return files;
}

/**
 * 清空一段注释或字符串内容，同时保留换行和字符索引以便准确报告行号。
 * @param {string[]} output 清理中的字符数组。
 * @param {string} source 原始源码。
 * @param {number} start 起始索引（含）。
 * @param {number} end 结束索引（不含）。
 * @returns {void}
 */
function blankRange(output, source, start, end) {
  const boundedStart = Math.max(0, start);
  const boundedEnd = Math.min(source.length, end);
  for (let index = boundedStart; index < boundedEnd; index += 1) {
    if (source[index] !== "\n" && source[index] !== "\r") {
      output[index] = " ";
    }
  }
}

/**
 * 识别 Rust 原始字符串的起始标记，避免字符串中的模块名被误识别。
 * @param {string} source 源码文本。
 * @param {number} index 当前索引。
 * @returns {{end: number, delimiter: string}|null} 原始字符串信息。
 */
function readRustRawStringStart(source, index) {
  const hasBytePrefix = source[index] === "b" && source[index + 1] === "r";
  const isRawStart = source[index] === "r" || hasBytePrefix;
  if (!isRawStart) {
    return null;
  }

  const rawIndex = hasBytePrefix ? index + 1 : index;
  let cursor = rawIndex + 1;
  let hashCount = 0;
  while (source[cursor] === "#") {
    hashCount += 1;
    cursor += 1;
  }
  if (source[cursor] !== '"') {
    return null;
  }
  return {
    // 起始双引号索引，用于保留源码位置。
    end: cursor,
    // 闭合原始字符串需要匹配的结束标记。
    delimiter: `"${"#".repeat(hashCount)}`,
  };
}

/**
 * 判断单引号是否是 Rust 字符字面量，而不是生命周期标记。
 * @param {string} source 源码文本。
 * @param {number} index 单引号索引。
 * @returns {boolean} 是否应按字符字面量清理。
 */
function isRustCharLiteral(source, index) {
  let cursor = index + 1;
  if (source[cursor] === "\\") {
    cursor += 2;
  } else {
    cursor += 1;
  }
  return source[cursor] === "'" && !source.slice(index, cursor + 1).includes("\n");
}

/**
 * 判断当前扩展名是否采用井号行注释。
 * @param {string} extension 文件扩展名。
 * @returns {boolean} 是否采用井号行注释。
 */
function usesHashLineComment(extension) {
  return new Set([".py", ".sh", ".bash", ".zsh", ".ps1"]).has(extension);
}

/**
 * 清理 Rust、JavaScript、Shell、PowerShell 和 Python 的注释及字符串。
 * 只保留可作为代码引用的字符，换行保留以便输出原始行号。
 * @param {string} source 源码文本。
 * @param {string} extension 文件扩展名。
 * @returns {string} 注释和字符串被空格替换后的代码。
 */
function stripCommentsAndStrings(source, extension = ".rs") {
  const output = source.split("");
  const isRust = extension === ".rs";
  const isHashComment = usesHashLineComment(extension);
  const supportsTemplate = new Set([".js", ".mjs", ".cjs", ".ts", ".tsx", ".jsx"]).has(
    extension,
  );
  let state = "code";
  let blockDepth = 0;
  let quote = "";
  let rawDelimiter = "";
  let tripleDelimiter = "";

  /**
   * 清理当前字符串状态中的转义字符并保留换行位置。
   * @param {number} index 当前字符索引。
   * @returns {number} 下一个待处理索引。
   */
  function consumeEscapedCharacter(index) {
    blankRange(output, source, index, index + 1);
    if (source[index] === "\\" && index + 1 < source.length) {
      blankRange(output, source, index + 1, index + 2);
      return index + 2;
    }
    return index + 1;
  }

  for (let index = 0; index < source.length; ) {
    const current = source[index];
    const next = source[index + 1];

    if (state === "line-comment") {
      if (current === "\n" || current === "\r") {
        state = "code";
      } else {
        blankRange(output, source, index, index + 1);
      }
      index += 1;
      continue;
    }

    if (state === "block-comment") {
      if (current === "/" && next === "*") {
        blankRange(output, source, index, index + 2);
        blockDepth += 1;
        index += 2;
        continue;
      }
      if (current === "*" && next === "/") {
        blankRange(output, source, index, index + 2);
        blockDepth -= 1;
        state = blockDepth === 0 ? "code" : "block-comment";
        index += 2;
        continue;
      }
      blankRange(output, source, index, index + 1);
      index += 1;
      continue;
    }

    if (state === "powershell-block-comment") {
      if (current === "#" && next === ">") {
        blankRange(output, source, index, index + 2);
        state = "code";
        index += 2;
        continue;
      }
      blankRange(output, source, index, index + 1);
      index += 1;
      continue;
    }

    if (state === "raw-string") {
      if (source.startsWith(rawDelimiter, index)) {
        const delimiterLength = rawDelimiter.length;
        blankRange(output, source, index, index + delimiterLength);
        state = "code";
        rawDelimiter = "";
        index += delimiterLength;
        continue;
      }
      blankRange(output, source, index, index + 1);
      index += 1;
      continue;
    }

    if (state === "triple-string") {
      if (source.startsWith(tripleDelimiter, index)) {
        blankRange(output, source, index, index + tripleDelimiter.length);
        state = "code";
        tripleDelimiter = "";
        index += 3;
        continue;
      }
      blankRange(output, source, index, index + 1);
      index += 1;
      continue;
    }

    if (state === "string") {
      if (current === "\\") {
        index = consumeEscapedCharacter(index);
        continue;
      }
      blankRange(output, source, index, index + 1);
      if (current === quote) {
        state = "code";
        quote = "";
      }
      index += 1;
      continue;
    }

    if (current === "/" && next === "*") {
      blankRange(output, source, index, index + 2);
      blockDepth = 1;
      state = "block-comment";
      index += 2;
      continue;
    }
    if (current === "/" && next === "/") {
      blankRange(output, source, index, index + 2);
      state = "line-comment";
      index += 2;
      continue;
    }
    if (isHashComment && current === "#") {
      blankRange(output, source, index, index + 1);
      state = "line-comment";
      index += 1;
      continue;
    }
    if (extension === ".ps1" && current === "<" && next === "#") {
      blankRange(output, source, index, index + 2);
      state = "powershell-block-comment";
      index += 2;
      continue;
    }

    if (isRust) {
      const rawStart = readRustRawStringStart(source, index);
      if (rawStart) {
        blankRange(output, source, index, rawStart.end + 1);
        rawDelimiter = rawStart.delimiter;
        state = "raw-string";
        index = rawStart.end + 1;
        continue;
      }
    }

    if (current === '"' || current === "'" || (supportsTemplate && current === "`")) {
      const triple =
        (extension === ".py" || extension === ".ps1") &&
        source.startsWith(current.repeat(3), index);
      if (triple) {
        tripleDelimiter = current.repeat(3);
        blankRange(output, source, index, index + 3);
        state = "triple-string";
        index += 3;
        continue;
      }
      if (current !== "'" || !isRust || isRustCharLiteral(source, index)) {
        quote = current;
        blankRange(output, source, index, index + 1);
        state = "string";
        index += 1;
        continue;
      }
    }

    index += 1;
  }

  return output.join("");
}

/**
 * 在清理后的代码中查找正则表达式的全部匹配。
 * @param {string} code 清理后的代码。
 * @param {RegExp} pattern 引用匹配器。
 * @returns {{text: string, index: number}[]} 匹配文本及字符索引。
 */
function findMatches(code, pattern) {
  const flags = pattern.flags.includes("g") ? pattern.flags : `${pattern.flags}g`;
  const matcher = new RegExp(pattern.source, flags);
  const matches = [];
  for (const match of code.matchAll(matcher)) {
    matches.push({
      // 规则实际命中的代码文本。
      text: match[0],
      // 命中在清理代码中的字符索引。
      index: match.index ?? 0,
    });
  }
  return matches;
}

/**
 * 按字符索引计算源码行号。
 * @param {string} source 源码文本。
 * @param {number} index 字符索引。
 * @returns {number} 从 1 开始的行号。
 */
function getLineNumber(source, index) {
  let line = 1;
  for (let cursor = 0; cursor < index; cursor += 1) {
    if (source[cursor] === "\n") {
      line += 1;
    }
  }
  return line;
}

/**
 * 读取单个代码文件并生成原文、清理代码和按行索引。
 * @param {string} repositoryRoot 仓库根目录。
 * @param {string} absolutePath 文件绝对路径。
 * @returns {{path: string, raw: string, clean: string, rawLines: string[], cleanLines: string[]}} 文件快照。
 */
function readCodeSource(repositoryRoot, absolutePath) {
  const raw = readFileSync(absolutePath, "utf8");
  if (raw.includes(" ")) {
    return null;
  }
  const extension = extname(absolutePath).toLowerCase();
  const clean = stripCommentsAndStrings(raw, extension);
  return {
    // 仓库相对路径，作为结果定位信息。
    path: normalizePath(relative(repositoryRoot, absolutePath)),
    // 未清理原文，用于输出命中的原始行。
    raw,
    // 去除注释和字符串后的代码，用于架构匹配。
    clean,
    // 原文按行拆分，保留业务代码上下文。
    rawLines: raw.split(/\r?\n/),
    // 清理代码按行拆分，供行号和自检使用。
    cleanLines: clean.split(/\r?\n/),
  };
}

/**
 * 读取整个 src-tauri 代码树，不依赖 Git diff，因此历史代码和未跟踪代码均会检查。
 * @param {string} repositoryRoot 仓库根目录。
 * @returns {Map<string, ReturnType<typeof readCodeSource>>} 按仓库相对路径索引的文件快照。
 */
function readCodeSources(repositoryRoot) {
  const sourceRoot = join(repositoryRoot, "src-tauri");
  const sources = new Map();
  for (const absolutePath of collectCodeFiles(sourceRoot)) {
    const source = readCodeSource(repositoryRoot, absolutePath);
    if (source) {
      sources.set(source.path, source);
    }
  }
  return sources;
}

/**
 * 生成带文件、行号、规则、命中引用和修复方向的违规对象。
 * @param {string} file 仓库相对路径。
 * @param {number} line 源码行号；结构缺失使用 0。
 * @param {string} rule 规则编号。
 * @param {string} match 命中的引用或缺失说明。
 * @param {string} direction 修复方向。
 * @param {string} originalLine 命中的原始源码行。
 * @returns {{file: string, line: number, rule: string, match: string, direction: string, originalLine: string}} 违规对象。
 */
function createFinding(file, line, rule, match, direction, originalLine = "") {
  return {
    // 违规文件的仓库相对路径。
    file,
    // 违规引用所在行号或结构缺失标记 0。
    line,
    // 架构规则编号。
    rule,
    // 实际命中的引用文本或缺失结构说明。
    match,
    // 恢复正确调用方向的业务修复指导。
    direction,
    // 命中的完整原始源码行。
    originalLine,
  };
}

/**
 * 记录指定文件缺失的结构要求。
 * @param {ReturnType<typeof createFinding>[]} findings 违规列表。
 * @param {string} file 文件路径。
 * @param {string} rule 规则编号。
 * @param {string} match 缺失说明。
 * @param {string} direction 修复方向。
 * @returns {void}
 */
function addMissingFinding(findings, file, rule, match, direction) {
  findings.push(createFinding(file, 0, rule, match, direction));
}

/**
 * 将文件中的规则匹配转换为带原始行内容的违规结果。
 * @param {ReturnType<typeof readCodeSource>} source 文件快照。
 * @param {ReturnType<typeof defineRule>} rule 适用规则。
 * @returns {ReturnType<typeof createFinding>[]} 文件命中的违规结果。
 */
function sourceRuleMatchesToFindings(source, rule) {
  return findMatches(source.clean, rule.pattern).map((match) => {
    const line = getLineNumber(source.clean, match.index);
    return createFinding(
      source.path,
      line,
      rule.rule,
      match.text.replace(/\s+/g, " ").trim(),
      rule.direction,
      source.rawLines[line - 1] ?? "",
    );
  });
}

/**
 * 判断路径是否属于远端运行时适配层；该层允许通过 SSH CLI 生命周期取得服务。
 * @param {string} filePath 仓库相对路径。
 * @returns {boolean} 是否为远端运行时适配文件。
 */
function isRemoteRuntimeAdapterPath(filePath) {
  return /^src-tauri\/src\/remote_project_(?:codex|opencode|claude)_runtime_service\.rs$/.test(
    normalizePath(filePath),
  );
}

/**
 * 判断路径是否属于必须通过工厂和统一接口调用的业务代码。
 * 远端运行时适配层由 REF-03 单独约束，避免与 SSH 生命周期允许关系冲突。
 * @param {string} filePath 仓库相对路径。
 * @returns {boolean} 是否属于业务调用层。
 */
function isBusinessPath(filePath) {
  const normalized = normalizePath(filePath);
  if (normalized.startsWith("src-tauri/src/commands/")) {
    return true;
  }
  if (normalized.startsWith("src-tauri/src/remote/")) {
    return true;
  }
  if (isRemoteRuntimeAdapterPath(normalized)) {
    return false;
  }

  const name = basename(normalized);
  return (
    name.startsWith("remote_project_") ||
    name === "remote_project_session_refresh_service.rs" ||
    name === "auracoder_thread_mcp_service.rs" ||
    name === "mcp_gateway.rs"
  );
}

/**
 * 构造 Rust 模块直接引用匹配器，覆盖 crate、super、self、use 和 mod 写法。
 * @param {string[]} moduleNames 需要识别的模块名称。
 * @returns {RegExp} 模块引用正则表达式。
 */
function moduleReferencePattern(moduleNames) {
  const names = moduleNames
    .map((name) => name.replace(/[.*+?^${}()|[\]\\]/g, "\\$&"))
    .join("|");
  return new RegExp(
    [
      `\\b(?:crate|super|self)\\s*::\\s*(?:${names})\\b`,
      `\\buse\\s+(?:(?:crate|super|self)\\s*::\\s*)?(?:${names})\\b`,
      `\\buse\\s+(?:crate|super|self)\\s*::\\s*\\{[^}]*?\\b(?:${names})\\b`,
      `\\b(?:pub\\s+)?mod\\s+(?:${names})\\b`,
    ].join("|"),
    "g",
  );
}

/**
 * 返回每个源码路径适用的全量引用方向规则。
 * @param {string} filePath 仓库相对路径。
 * @returns {ReturnType<typeof defineRule>[]} 路径适用的规则列表。
 */
function rulesForPath(filePath) {
  const normalized = normalizePath(filePath);
  const rules = [];

  if (isBusinessPath(normalized)) {
    const direction =
      "业务代码只能通过 CliToolFactory 获取 CliTool，再调用统一业务接口；不得直接接触 CLI 生命周期、SSH Tunnel 或具体实现类。";
    rules.push(
      defineRule("REF-01", /\blocal_cli_service_lifecycle\b/g, direction),
      defineRule("REF-01", /\bcli_service_lifecycle\b/g, direction),
      defineRule("REF-01", /\bcli_tunnel_registry\b/g, direction),
      defineRule("REF-01", /\bSshCliTunnel(?:Registry)?\b/g, direction),
      defineRule("REF-01", /\b(?:CodexCli|OpenCodeCli|ClaudeCodeCli)\b/g, direction),
      defineRule("REF-01", /\bwith_mcp_runtime\b/g, direction),
    );
  }

  if (/^src-tauri\/src\/cli_tools\/(?:codex|opencode|claude_code)\.rs$/.test(normalized)) {
    const direction =
      "CLI 实现类只能通过本机 CLI 生命周期或对应远端运行时适配层取得 CLI 服务，不得直接碰 SSH Tunnel。";
    rules.push(
      defineRule("REF-02", /\bcli_tunnel_registry\b/g, direction),
      defineRule("REF-02", /\bSshCliTunnel(?:Registry)?\b/g, direction),
    );
  }

  if (isRemoteRuntimeAdapterPath(normalized)) {
    const direction =
      "远端运行时适配只能通过 ssh::cli_service_lifecycle 获取 CLI 服务，不得直接取得、注册、启停或释放 Tunnel。";
    rules.push(
      defineRule("REF-03", /\blocal_cli_service_lifecycle\b/g, direction),
      defineRule("REF-03", /\bcli_tunnel_registry\b/g, direction),
      defineRule("REF-03", /\bSshCliTunnel(?:Registry)?\b/g, direction),
      defineRule(
        "REF-03",
        /\b(?:start|stop)_remote_cli_service(?:_with_mcp_token|_for_tunnel)?\b/g,
        direction,
      ),
      defineRule(
        "REF-03",
        /\b(?:register_cli_tunnels|init_all_ssh_remote_server|open_tunnel|allocate_remote_port|acquire_(?:temporary|persistent)_service_use|release_(?:temporary|persistent)_service_use)\b/g,
        direction,
      ),
    );
  }

  if (normalized === "src-tauri/src/ssh/cli_tunnel_registry.rs") {
    const direction =
      "SSH Tunnel 层只能管理隧道网络能力，不得向上引用 CliTool、工厂、具体实现类或 engines。";
    rules.push(
      defineRule("REF-04", /\bCliTool\b/g, direction),
      defineRule("REF-04", /\bCliToolFactory\b/g, direction),
      defineRule("REF-04", /\bcli_tools\b/g, direction),
      defineRule("REF-04", /\b(?:CodexCli|OpenCodeCli|ClaudeCodeCli)\b/g, direction),
      defineRule("REF-04", /\bengines\b/g, direction),
    );
  }

  if (
    normalized === "src-tauri/src/local_cli_service_lifecycle.rs" ||
    normalized === "src-tauri/src/ssh/cli_service_lifecycle.rs"
  ) {
    const direction =
      "CLI 生命周期只负责 CLI 服务创建、复用和终止，不得依赖 commands、会话、消息或其它业务模块；SSH 生命周期可使用 Tunnel 注册表。";
    rules.push(
      defineRule(
        "REF-05",
        moduleReferencePattern([
          "commands",
          "db",
          "remote",
          "extensions",
          "messages",
          "message_notify_helper",
          "threads",
          "session",
          "chat",
          "mcp_gateway",
          "auracoder_thread_mcp_service",
        ]),
        direction,
      ),
      defineRule(
        "REF-05",
        /\b(?:remote_project_session_refresh_service|auracoder_thread_mcp_service)\b/g,
        direction,
      ),
    );
  }

  if (normalized === "src-tauri/src/cli_tools.rs") {
    const direction =
      "公共 CliTool 接口只定义稳定 CLI 业务能力，不得依赖具体实现、工厂或任一生命周期。";
    rules.push(
      defineRule("REF-06", /\b(?:CodexCli|OpenCodeCli|ClaudeCodeCli)\b/g, direction),
      defineRule("REF-06", /\bCliToolFactory\b/g, direction),
      defineRule(
        "REF-06",
        /\b(?:local_cli_service_lifecycle|cli_service_lifecycle|cli_tunnel_registry)\b/g,
        direction,
      ),
      defineRule("REF-06", /\bSshCliTunnel(?:Registry)?\b/g, direction),
    );
  }

  if (normalized === "src-tauri/src/cli_tools/factory.rs") {
    const direction =
      "CliToolFactory 只负责三个 CLI 实现类映射和统一接口，不得引用生命周期、Tunnel 或业务模块。";
    rules.push(
      defineRule(
        "REF-07",
        /\b(?:local_cli_service_lifecycle|cli_service_lifecycle|cli_tunnel_registry)\b/g,
        direction,
      ),
      defineRule("REF-07", /\bSshCliTunnel(?:Registry)?\b/g, direction),
      defineRule(
        "REF-07",
        moduleReferencePattern([
          "commands",
          "db",
          "remote",
          "extensions",
          "message_notify_helper",
          "messages",
          "threads",
          "session",
          "chat",
          "mcp_gateway",
          "auracoder_thread_mcp_service",
        ]),
        direction,
      ),
      defineRule(
        "REF-07",
        /\b(?:remote_project_session_refresh_service|remote_project_(?:codex|opencode|claude)_runtime_service|auracoder_thread_mcp_service)\b/g,
        direction,
      ),
    );
  }

  if (normalized === "src-tauri/src/engines/opencode.rs") {
    const direction =
      "OpenCode Engine 不得自行创建、启动、复用或停止 CLI 服务端；必须移交对应 CLI 生命周期管理。";
    rules.push(
      defineRule("REF-08", /\bensure_server\b/g, direction),
      defineRule("REF-08", /\bstart_server\b/g, direction),
      defineRule("REF-08", /\bstop_server_if_unused\b/g, direction),
      defineRule("REF-08", /\bOpenCodeServer\b/g, direction),
      defineRule("REF-08", /\bCommand\s*::\s*new\s*\(\s*&executable\s*\)/g, direction),
      defineRule("REF-08", /\bcommand\s*\.\s*(?:arg|spawn)\b/g, direction),
      defineRule("REF-08", /\bopencode\s+serve\b/g, direction),
    );
  }

  return rules;
}

/**
 * 校验固定文件和符号，缺失时以结构违规返回而不是抛出异常。
 * @param {Map<string, ReturnType<typeof readCodeSource>>} sources 文件快照索引。
 * @param {ReturnType<typeof createFinding>[]} findings 违规结果列表。
 * @param {string} file 文件路径。
 * @param {string} rule 规则编号。
 * @param {RegExp} pattern 必须出现的符号模式。
 * @param {string} symbol 业务符号名称。
 * @param {string} direction 修复方向。
 * @returns {void}
 */
function requireStructure(sources, findings, file, rule, pattern, symbol, direction) {
  const source = sources.get(file);
  if (!source) {
    addMissingFinding(findings, file, rule, `文件不存在: ${file}`, direction);
    return;
  }
  if (findMatches(source.clean, pattern).length === 0) {
    addMissingFinding(findings, file, rule, `缺少结构符号: ${symbol}`, direction);
  }
}

/**
 * 执行接口、工厂、实现类、生命周期和 Tunnel Registry 的全局结构检查。
 * @param {Map<string, ReturnType<typeof readCodeSource>>} sources 文件快照索引。
 * @returns {ReturnType<typeof createFinding>[]} 结构违规结果。
 */
function checkGlobalStructure(sources) {
  const findings = [];
  const interfaceFile = "src-tauri/src/cli_tools.rs";
  const factoryFile = "src-tauri/src/cli_tools/factory.rs";
  const localLifecycleFile = "src-tauri/src/local_cli_service_lifecycle.rs";
  const sshLifecycleFile = "src-tauri/src/ssh/cli_service_lifecycle.rs";
  const tunnelFile = "src-tauri/src/ssh/cli_tunnel_registry.rs";

  requireStructure(
    sources,
    findings,
    interfaceFile,
    "REF-06",
    /\bpub\s+trait\s+CliTool\b/,
    "pub trait CliTool",
    "保留 CliTool 公共接口定义，并把实现、工厂和生命周期引用移出接口文件。",
  );

  requireStructure(
    sources,
    findings,
    factoryFile,
    "REF-07",
    /\b(?:pub(?:\s*\([^)]*\))?\s+)?struct\s+CliToolFactory\b/,
    "CliToolFactory",
    "保留工厂结构，并把三个实现类的映射集中放在工厂中。",
  );
  for (const method of ["create", "create_mcp_cli"]) {
    requireStructure(
      sources,
      findings,
      factoryFile,
      "REF-07",
      new RegExp(`\\bfn\\s+${method}\\s*\\(`),
      method,
      "工厂必须保留统一创建入口，并按 CLI 标识返回统一接口实现。",
    );
  }
  for (const cli of ["CodexCli", "OpenCodeCli", "ClaudeCodeCli"]) {
    requireStructure(
      sources,
      findings,
      factoryFile,
      "REF-07",
      new RegExp(`\\b${cli}\\s*::\\s*(?:new|with_mcp_runtime)\\b`),
      `${cli} 映射`,
      "工厂只负责三个实现类映射，不要把生命周期或业务编排放入工厂。",
    );
  }

  for (const [file, cli] of [
    ["src-tauri/src/cli_tools/codex.rs", "CodexCli"],
    ["src-tauri/src/cli_tools/opencode.rs", "OpenCodeCli"],
    ["src-tauri/src/cli_tools/claude_code.rs", "ClaudeCodeCli"],
  ]) {
    requireStructure(
      sources,
      findings,
      file,
      "REF-02",
      new RegExp(`\\bimpl\\s+CliTool\\s+for\\s+${cli}\\b`),
      `impl CliTool for ${cli}`,
      "每个 CLI 实现类都必须实现统一 CliTool 接口。",
    );
    const source = sources.get(file);
    if (source) {
      const hasLocalLifecycle =
        findMatches(source.clean, /\bLocalCliServiceLifecycle\s*::\s*get\s*\(/).length > 0;
      const hasRemoteAdapter =
        findMatches(source.clean, /\bremote_project_(?:codex|opencode|claude)_runtime_service\b/).length >
        0;
      if (!hasLocalLifecycle && !hasRemoteAdapter) {
        addMissingFinding(
          findings,
          file,
          "REF-02",
          "缺少 LocalCliServiceLifecycle::get 或对应 remote_project_*_runtime_service 引用",
          "实现类只能通过本机 CLI 生命周期或对应远端运行时适配层取得 CLI 句柄。",
        );
      }
    }
  }

  requireStructure(
    sources,
    findings,
    localLifecycleFile,
    "REF-05",
    /\b(?:pub(?:\s*\([^)]*\))?\s+)?enum\s+LocalCliHandle\b/,
    "LocalCliHandle",
    "本机 CLI 生命周期必须保留句柄类型，统一管理本机 CLI 服务。",
  );
  requireStructure(
    sources,
    findings,
    localLifecycleFile,
    "REF-05",
    /\b(?:pub(?:\s*\([^)]*\))?\s+)?struct\s+LocalCliServiceLifecycle\b/,
    "LocalCliServiceLifecycle",
    "本机 CLI 生命周期必须保留生命周期入口类型。",
  );
  for (const method of ["get", "set", "terminate"]) {
    requireStructure(
      sources,
      findings,
      localLifecycleFile,
      "REF-05",
      new RegExp(`\\bfn\\s+${method}\\s*\\(`),
      method,
      "本机 CLI 生命周期必须提供 get、set、terminate 生命周期入口。",
    );
    requireStructure(
      sources,
      findings,
      sshLifecycleFile,
      "REF-05",
      new RegExp(`\\bfn\\s+${method}\\s*\\(`),
      method,
      "SSH CLI 生命周期必须提供 get、set、terminate 生命周期入口。",
    );
  }
  requireStructure(
    sources,
    findings,
    localLifecycleFile,
    "REF-05",
    /\bCliToolFactory\b/,
    "CliToolFactory 引用",
    "本机 CLI 生命周期通过 CliToolFactory 创建对应 CLI 业务实现。",
  );
  requireStructure(
    sources,
    findings,
    sshLifecycleFile,
    "REF-05",
    /\bcli_tunnel_registry\b/,
    "cli_tunnel_registry 引用",
    "SSH CLI 生命周期必须通过 Tunnel 注册表取得底层隧道能力。",
  );
  requireStructure(
    sources,
    findings,
    sshLifecycleFile,
    "REF-05",
    /\bCliToolFactory\b/,
    "CliToolFactory 引用",
    "SSH CLI 生命周期通过 CliToolFactory 创建对应 CLI 业务实现。",
  );
  requireStructure(
    sources,
    findings,
    tunnelFile,
    "REF-04",
    /\b(?:pub(?:\s*\([^)]*\))?\s+)?struct\s+SshCliTunnelRegistry\b/,
    "SshCliTunnelRegistry",
    "保留 SSH Tunnel 注册表类型，并把 Tunnel 管理限定在 SSH 隧道层。",
  );

  for (const file of [
    "src-tauri/src/remote_project_codex_runtime_service.rs",
    "src-tauri/src/remote_project_opencode_runtime_service.rs",
    "src-tauri/src/remote_project_claude_runtime_service.rs",
  ]) {
    requireStructure(
      sources,
      findings,
      file,
      "REF-03",
      /\bssh\s*::\s*(?:\{[\s\S]*?\bcli_service_lifecycle\b|cli_service_lifecycle\b)/,
      "ssh::cli_service_lifecycle 引用",
      "远端运行时适配器必须通过 SSH CLI 生命周期取得远端 CLI 服务。",
    );
  }

  return findings;
}

/**
 * 对源码树执行所有路径适用的 REF-01 至 REF-08 引用检查。
 * @param {Map<string, ReturnType<typeof readCodeSource>>} sources 文件快照索引。
 * @returns {ReturnType<typeof createFinding>[]} 全量引用违规结果。
 */
function checkAllReferences(sources) {
  const findings = [];
  for (const source of sources.values()) {
    for (const rule of rulesForPath(source.path)) {
      findings.push(...sourceRuleMatchesToFindings(source, rule));
    }
  }
  return findings;
}

/**
 * 从样例代码中取得指定路径的规则命中，用于脚本自身自检。
 * @param {string} filePath 样例文件路径。
 * @param {string} snippet 样例代码。
 * @returns {ReturnType<typeof createFinding>[]} 样例命中结果。
 */
function findSampleFindings(filePath, snippet) {
  const extension = extname(filePath).toLowerCase() || ".rs";
  const clean = stripCommentsAndStrings(snippet, extension);
  const source = {
    // 自检样例使用固定的虚拟仓库路径。
    path: filePath,
    // 自检不需要原始文件内容。
    raw: snippet,
    // 清理后的样例代码。
    clean,
    // 保留样例行用于定位结果。
    rawLines: snippet.split(/\r?\n/),
    // 保留清理样例行用于内部回归。
    cleanLines: clean.split(/\r?\n/),
  };
  return rulesForPath(filePath).flatMap((rule) => sourceRuleMatchesToFindings(source, rule));
}

/**
 * 用内存样例验证业务直引、实现类直引、Engine 生命周期和注释字符串过滤。
 * REF-08 的真实源码不参与“零违规”断言，因为当前 OpenCode Engine 必须如实报告。
 * @returns {void}
 */
function runSelfChecks() {
  assert.ok(
    findSampleFindings(
      "src-tauri/src/commands/example.rs",
      "use crate::local_cli_service_lifecycle::LocalCliServiceLifecycle;",
    ).some((finding) => finding.rule === "REF-01"),
    "REF-01 业务直引样例未被识别",
  );
  assert.ok(
    findSampleFindings(
      "src-tauri/src/cli_tools/codex.rs",
      "use crate::ssh::cli_tunnel_registry::SshCliTunnel;",
    ).some((finding) => finding.rule === "REF-02"),
    "REF-02 实现类直引 Tunnel 样例未被识别",
  );
  assert.ok(
    findSampleFindings(
      "src-tauri/src/engines/opencode.rs",
      "async fn ensure_server() { let _: OpenCodeServer; start_server(); stop_server_if_unused(); }",
    ).some((finding) => finding.rule === "REF-08"),
    "REF-08 OpenCode Engine 生命周期样例未被识别",
  );
  assert.ok(
    findSampleFindings(
      "src-tauri/src/engines/opencode.rs",
      "opencode serve;",
    ).some((finding) => finding.match.includes("opencode serve")),
    "REF-08 opencode serve 样例未被识别",
  );

  const commentedAndStringSample = `
    // use crate::local_cli_service_lifecycle::LocalCliServiceLifecycle;
    /* use crate::ssh::cli_tunnel_registry::SshCliTunnel; */
    const text = "ensure_server OpenCodeServer opencode serve";
  `;
  assert.equal(
    findSampleFindings("src-tauri/src/commands/example.rs", commentedAndStringSample).length,
    0,
    "注释和字符串中的架构引用不应被识别",
  );
  assert.equal(
    findSampleFindings("src-tauri/src/engines/opencode.rs", commentedAndStringSample).length,
    0,
    "OpenCode 注释和字符串中的生命周期引用不应被识别",
  );
  assert.equal(CORE_CALL_CHAIN.length, 4, "核心调用链必须保留四段业务语义");
  assert.ok(CORE_CALL_CHAIN.every((item) => item.length > 0), "核心调用链不能出现空语义");
}

/**
 * 将违规结果格式化为包含路径、行号、规则、命中引用和修复方向的中文报告。
 * @param {ReturnType<typeof createFinding>[]} findings 违规结果。
 * @returns {string} 格式化后的报告文本。
 */
function formatFindings(findings) {
  return findings
    .map((finding, index) => {
      const originalLine = finding.originalLine ? `\n  原始代码行: ${finding.originalLine}` : "";
      return [
        `${index + 1}. 文件: ${finding.file}`,
        `  行号: ${finding.line}`,
        `  规则: ${finding.rule}`,
        `  命中引用: ${finding.match}`,
        `  修复方向: ${finding.direction}${originalLine}`,
      ].join("\n");
    })
    .join("\n");
}

/**
 * 执行脚本自检、全局结构检查和全量引用检查，并返回标准退出码。
 * @returns {number} 无违规返回 0，存在违规或执行异常返回 1。
 */
function main() {
  try {
    runSelfChecks();
    const repositoryRoot = getRepositoryRoot();
    const sources = readCodeSources(repositoryRoot);
    const findings = [...checkGlobalStructure(sources), ...checkAllReferences(sources)];

    if (findings.length > 0) {
      process.stderr.write(
        `Tauri 后端架构引用检查失败，共 ${findings.length} 项。\n${formatFindings(findings)}\n`,
      );
      return 1;
    }

    process.stdout.write(
      [
        "Tauri 后端架构引用检查通过。",
        `全量代码扫描: ${sources.size} 个 src-tauri 代码文件。`,
        "引用方向规则: REF-01 至 REF-08。",
        "脚本自检: 业务直引、实现类直引、OpenCode Engine 生命周期和注释字符串过滤均通过。",
        "依赖检查: 仅使用 Node.js 内置模块，无新增依赖。",
      ].join("\n") + "\n",
    );
    return 0;
  } catch (error) {
    const message = error instanceof Error ? error.stack ?? error.message : String(error);
    process.stderr.write(`Tauri 后端架构引用检查无法完成。\n${message}\n`);
    return 1;
  }
}

process.exitCode = main();
