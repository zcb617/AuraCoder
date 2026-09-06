import { spawnSync } from "node:child_process";
import { existsSync, readFileSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

const scriptDir = path.dirname(fileURLToPath(import.meta.url));
const repoRoot = path.resolve(scriptDir, "..");
const claudeSidecarDir = path.join(repoRoot, "src-tauri", "sidecar-dist");
const claudeSdkRuntime =
  (process.env.PANES_CLAUDE_SDK_PLATFORM ?? process.platform) === "linux"
    ? path.join(claudeSidecarDir, "claude-sdk-node_modules.tar.gz")
    : path.join(
        claudeSidecarDir,
        "node_modules",
        "@anthropic-ai",
        "claude-agent-sdk",
        "sdk.mjs",
      );
const requiredClaudeComponents = [
  path.join(claudeSidecarDir, "claude-agent-sdk-server.mjs"),
  path.join(claudeSidecarDir, "claude-remote-session-server.mjs"),
  path.join(claudeSidecarDir, "claude-remote-runtime-linux-x64.tar.gz"),
  path.join(claudeSidecarDir, "claude-remote-runtime-version.txt"),
  claudeSdkRuntime,
];

function missingClaudeComponents() {
  return requiredClaudeComponents.filter((componentPath) => !existsSync(componentPath));
}

/**
 * 判断 Claude sidecar 是否需要重新生成，确保源码和生成目录使用同一版本。
 */
export function shouldBuildClaudeComponents({
  missingComponents = [],
  sourceVersion,
  stagedVersion,
} = {}) {
  const normalizedSourceVersion =
    typeof sourceVersion === "string" ? sourceVersion.trim() : "";
  const normalizedStagedVersion =
    typeof stagedVersion === "string" ? stagedVersion.trim() : "";
  return (
    missingComponents.length > 0 ||
    normalizedSourceVersion.length === 0 ||
    normalizedStagedVersion.length === 0 ||
    normalizedSourceVersion !== normalizedStagedVersion
  );
}

/**
 * 调用现有 Claude sidecar 构建脚本读取源码版本，用于开发启动前的生成物校验。
 */
export function getClaudeSourceVersion({
  run = spawnSync,
  cwd = repoRoot,
  scriptPath = path.join(scriptDir, "build-claude-sidecar.mjs"),
} = {}) {
  const result = run(process.execPath, [scriptPath, "--print-version"], {
    cwd,
    encoding: "utf8",
    stdio: ["ignore", "pipe", "pipe"],
    windowsHide: true,
  });
  if (result.error) {
    throw result.error;
  }
  if (result.signal) {
    throw new Error(`Claude sidecar 源码版本命令被信号 ${result.signal} 终止`);
  }
  if (result.status !== 0) {
    const stderr = typeof result.stderr === "string" ? result.stderr.trim() : "";
    throw new Error(
      `Claude sidecar 源码版本命令退出码为 ${result.status ?? "unknown"}${
        stderr ? `：${stderr}` : ""
      }`,
    );
  }
  const version = typeof result.stdout === "string" ? result.stdout.trim() : "";
  if (!version) {
    throw new Error("Claude sidecar 源码版本命令未返回版本号");
  }
  return version;
}

/**
 * 读取 sidecar-dist 中的远端运行时版本文件，并保留读取异常供启动日志输出。
 */
function readStagedClaudeRuntimeVersion() {
  try {
    const version = readFileSync(
      path.join(claudeSidecarDir, "claude-remote-runtime-version.txt"),
      "utf8",
    ).trim();
    return {
      // 生成目录中的版本号，空文件按缺失版本处理。
      version: version || null,
      // 读取版本文件时捕获的原始异常，成功时为空。
      error: null,
    };
  } catch (error) {
    return {
      // 读取失败时没有可供比对的生成版本号。
      version: null,
      // 保留原始异常，调用方负责输出真实失败原因。
      error,
    };
  }
}

function ensureClaudeComponents() {
  const missingComponents = missingClaudeComponents();
  let sourceVersion = null;
  let sourceVersionError = null;
  try {
    sourceVersion = getClaudeSourceVersion();
  } catch (error) {
    sourceVersionError = error;
  }
  const stagedVersionResult = readStagedClaudeRuntimeVersion();
  const needsBuild = shouldBuildClaudeComponents({
    missingComponents,
    sourceVersion,
    stagedVersion: stagedVersionResult.version,
  });
  if (!needsBuild) {
    return;
  }

  if (missingComponents.length > 0) {
    console.log("Claude 运行组件不完整，正在自动编译……");
    for (const componentPath of missingComponents) {
      console.log(`缺少组件：${path.relative(repoRoot, componentPath)}`);
    }
  } else if (sourceVersionError) {
    console.error("读取 Claude 源码版本失败，保留原始异常：", sourceVersionError);
    console.log("Claude 源码版本读取失败，正在自动编译……");
  } else if (stagedVersionResult.error) {
    console.error(
      "读取 Claude 运行组件版本文件失败，保留原始异常：",
      stagedVersionResult.error,
    );
    console.log("Claude 运行组件版本文件不可用，正在自动编译……");
  } else if (!stagedVersionResult.version) {
    console.log("Claude 运行组件版本文件为空，正在自动编译……");
  } else {
    console.log("Claude 运行组件版本与源码不一致，正在自动编译……");
    console.log(`源码版本：${sourceVersion}`);
    console.log(`生成版本：${stagedVersionResult.version}`);
  }

  const buildResult = spawnSync(
    process.execPath,
    [path.join(scriptDir, "build-claude-sidecar.mjs")],
    {
      cwd: repoRoot,
      stdio: "inherit",
      env: process.env,
      windowsHide: true,
    },
  );
  if (buildResult.error) {
    throw buildResult.error;
  }
  if (buildResult.signal) {
    throw new Error(`Claude 运行组件编译进程被信号 ${buildResult.signal} 终止`);
  }
  if (buildResult.status !== 0) {
    throw new Error(`Claude 运行组件编译进程退出码为 ${buildResult.status ?? "unknown"}`);
  }

  const remainingMissingComponents = missingClaudeComponents();
  if (remainingMissingComponents.length > 0) {
    throw new Error(
      `Claude 运行组件编译完成后仍缺少：${remainingMissingComponents
        .map((componentPath) => path.relative(repoRoot, componentPath))
        .join("、")}`,
    );
  }

  let verifiedSourceVersion;
  try {
    verifiedSourceVersion = getClaudeSourceVersion();
  } catch (error) {
    throw new Error("Claude 运行组件编译完成后无法读取源码版本。", { cause: error });
  }
  const verifiedStagedVersionResult = readStagedClaudeRuntimeVersion();
  if (verifiedStagedVersionResult.error) {
    throw new Error("Claude 运行组件编译完成后无法读取生成版本文件。", {
      cause: verifiedStagedVersionResult.error,
    });
  }
  if (
    shouldBuildClaudeComponents({
      missingComponents: remainingMissingComponents,
      sourceVersion: verifiedSourceVersion,
      stagedVersion: verifiedStagedVersionResult.version,
    })
  ) {
    throw new Error(
      `Claude 运行组件编译完成后版本仍不一致：源码 ${verifiedSourceVersion}，生成 ${
        verifiedStagedVersionResult.version || "缺失"
      }`,
    );
  }
  console.log("Claude 运行组件自动编译完成。");
}

const isMainModule =
  process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url);

if (isMainModule) {
  try {
    ensureClaudeComponents();
  } catch (error) {
    console.error(error);
    console.error("AuraCoder 开发启动失败：无法自动编译 Claude 运行组件。");
    process.exit(1);
  }

  // 选择当前平台可执行的 Tauri CLI 命令，确保开发启动流程跨平台一致。
  const tauriCommand = process.platform === "win32" ? "tauri.cmd" : "tauri";
  const result = spawnSync(tauriCommand, ["dev", ...process.argv.slice(2)], {
    cwd: repoRoot,
    // 继承终端输入输出，让 Tauri 开发进程直接提供交互和日志。
    stdio: "inherit",
    // 注入开发构建标识，让 Rust 运行时使用开发版默认数据库目录。
    env: { ...process.env, PANES_BUILD_TYPE: "development" },
    // Windows 的 tauri.cmd 是批处理 shim，必须经 shell 启动。
    shell: process.platform === "win32",
    windowsHide: true,
  });

  // 启动 Tauri 失败时直接抛出原始错误，确保失败原因对调用方可见。
  if (result.error) {
    throw result.error;
  }

  // 子进程被信号终止时，向当前进程转发相同信号以保持退出行为一致。
  if (result.signal) {
    process.kill(process.pid, result.signal);
  } else {
    // 子进程正常退出时，将其退出码返回给调用方。
    process.exit(result.status ?? 1);
  }
}
