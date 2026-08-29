import { spawnSync } from "node:child_process";
import { existsSync } from "node:fs";
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

function ensureClaudeComponents() {
  const missingComponents = missingClaudeComponents();
  if (missingComponents.length === 0) {
    return;
  }

  console.log("Claude 运行组件不完整，正在自动编译……");
  for (const componentPath of missingComponents) {
    console.log(`缺少组件：${path.relative(repoRoot, componentPath)}`);
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
  console.log("Claude 运行组件自动编译完成。");
}

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
