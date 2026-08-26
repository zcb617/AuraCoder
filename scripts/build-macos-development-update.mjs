import { access, mkdir, readFile } from "node:fs/promises";
import { constants as fsConstants } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { execFile, spawn } from "node:child_process";
import { promisify } from "node:util";

const scriptDir = path.dirname(fileURLToPath(import.meta.url));
const repoRoot = path.resolve(scriptDir, "..");
const signingDirectory = path.join(repoRoot, ".tauri-signing");
const signingKeyPath = path.join(signingDirectory, "development-updater.key");
const signingPassword = "auracoder-development-updater";
const execFileAsync = promisify(execFile);

if (process.platform !== "darwin") {
  throw new Error("macOS development update build is only supported on macOS");
}

/// 运行一个继承终端输入输出的子进程，并在失败时保留原始非零结果。
function run(command, args, environment = process.env) {
  return new Promise((resolve, reject) => {
    const child = spawn(command, args, {
      cwd: repoRoot,
      env: environment,
      stdio: "inherit",
      shell: false,
    });
    child.on("error", reject);
    child.on("exit", (code, signal) => {
      if (code === 0) {
        resolve();
        return;
      }
      const failure = signal
        ? `${command} ${args.join(" ")} exited with signal ${signal}`
        : `${command} ${args.join(" ")} exited with code ${code}`;
      const error = new Error(failure);
      error.exitCode = typeof code === "number" ? code : 1;
      reject(error);
    });
  });
}

await mkdir(signingDirectory, { recursive: true });
try {
  await access(signingKeyPath, fsConstants.F_OK);
} catch {
  await run("pnpm", [
    "exec",
    "tauri",
    "signer",
    "generate",
    "--ci",
    "--password",
    signingPassword,
    "--write-keys",
    signingKeyPath,
  ]);
}

const privateKey = (await readFile(signingKeyPath, "utf8")).trim();
if (!privateKey) {
  throw new Error(`Development signing key is empty: ${signingKeyPath}`);
}

/**
 * 校验 macOS 开发应用的深层签名、固定证书 Authority 和开发版标识。
 */
async function verifySignedApp(appPath) {
  await run("/usr/bin/codesign", [
    "--verify",
    "--deep",
    "--strict",
    "--verbose=2",
    appPath,
  ]);
  const codesignDetails = await execFileAsync(
    "/usr/bin/codesign",
    ["-dv", "--verbose=4", appPath],
    { cwd: repoRoot },
  );
  const details = [codesignDetails.stdout, codesignDetails.stderr].join("\n");
  if (!details.includes("Authority=Yunxiang AuraCoder Signing")) {
    throw new Error("macOS 应用签名证书不匹配: " + appPath);
  }
  if (!details.includes("Identifier=com.0573zzz.auracoder.dev")) {
    throw new Error("macOS 开发应用标识不匹配: " + appPath);
  }
}

/**
 * 读取 macOS 应用 Info.plist 中指定的业务字段。
 */
async function readPlistValue(infoPlistPath, key) {
  const result = await execFileAsync(
    "/usr/libexec/PlistBuddy",
    ["-c", "Print :" + key, infoPlistPath],
    { cwd: repoRoot },
  );
  return result.stdout.trim();
}

await run("/bin/zsh", ["packaging/macos/prepare-codesign.sh"]);

const buildOutputAppPath = path.join(
  repoRoot,
  "src-tauri",
  "target",
  "release",
  "bundle",
  "macos",
  "AuraCoderDev.app",
);

// 原参数 --no-sign 保留为注释：开发版现在必须使用固定证书完成签名校验。
// const disabledBuildArgument = "--no-sign";
await run(
  "pnpm",
  [
    "exec",
    "tauri",
    "build",
    "--bundles",
    "app",
    "--config",
    "src-tauri/tauri.macos.development.conf.json",
  ],
  {
    ...process.env,
    PANES_BUILD_TYPE: "development",
    TAURI_SIGNING_PRIVATE_KEY: privateKey,
    TAURI_SIGNING_PRIVATE_KEY_PASSWORD: signingPassword,
  },
);

await access(buildOutputAppPath, fsConstants.F_OK);
await verifySignedApp(buildOutputAppPath);

const installedAppPath = "/Applications/AuraCoderDev.app";
const temporaryAppPath = path.join(
  "/Applications",
  ".AuraCoderDev.app.dev-build." + process.pid + ".tmp",
);
await run("/bin/rm", ["-rf", temporaryAppPath]);
await run("/usr/bin/ditto", [buildOutputAppPath, temporaryAppPath]);
await verifySignedApp(temporaryAppPath);
await run("/bin/rm", ["-rf", installedAppPath]);
await run("/bin/mv", [temporaryAppPath, installedAppPath]);
await verifySignedApp(installedAppPath);

const installedInfoPlistPath = path.join(
  installedAppPath,
  "Contents",
  "Info.plist",
);
const installedDisplayName = await readPlistValue(
  installedInfoPlistPath,
  "CFBundleDisplayName",
);
if (installedDisplayName !== "AuraCoderDev") {
  throw new Error(
    "安装后的 macOS 开发应用名称不匹配: " + installedDisplayName,
  );
}
const installedBundleIdentifier = await readPlistValue(
  installedInfoPlistPath,
  "CFBundleIdentifier",
);
if (installedBundleIdentifier !== "com.0573zzz.auracoder.dev") {
  throw new Error(
    "安装后的 macOS 开发应用标识不匹配: " + installedBundleIdentifier,
  );
}
