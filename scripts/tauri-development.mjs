import { spawnSync } from "node:child_process";

// 选择当前平台可执行的 Tauri CLI 命令，确保开发启动流程跨平台一致。
const tauriCommand = process.platform === "win32" ? "tauri.cmd" : "tauri";
const result = spawnSync(tauriCommand, ["dev", ...process.argv.slice(2)], {
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
