import { cp, mkdir, readFile, rm, writeFile } from "node:fs/promises";
import { createHash } from "node:crypto";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { execFile, spawn } from "node:child_process";
import { promisify } from "node:util";

import { stageClaudeSdkPlatformAssets } from "./claude-sidecar-staging.mjs";
import { ensureClaudeSdkPnpmLayout } from "./ensure-claude-sdk-pnpm-layout.mjs";

const scriptDir = path.dirname(fileURLToPath(import.meta.url));
const repoRoot = path.resolve(scriptDir, "..");
const entryPoint = path.join(
  repoRoot,
  "src-tauri",
  "sidecar",
  "claude-agent-sdk-server.mjs",
);
const outFile = path.join(
  repoRoot,
  "src-tauri",
  "sidecar-dist",
  "claude-agent-sdk-server.mjs",
);
const outDir = path.dirname(outFile);
const remoteSessionEntryPoint = path.join(
  repoRoot,
  "src-tauri",
  "sidecar",
  "claude-remote-session-server.mjs",
);
const remoteSessionOutFile = path.join(
  outDir,
  "claude-remote-session-server.mjs",
);
const sdkDistNodeModulesDir = path.join(outDir, "node_modules");
const sdkDistPackageDir = path.join(
  sdkDistNodeModulesDir,
  "@anthropic-ai",
  "claude-agent-sdk",
);
const linuxSdkArchiveFile = path.join(outDir, "claude-sdk-node_modules.tar.gz");
const remoteLinuxRuntimeArchiveFile = path.join(
  outDir,
  "claude-remote-runtime-linux-x64.tar.gz",
);
const remoteRuntimeVersionFile = path.join(
  outDir,
  "claude-remote-runtime-version.txt",
);
const execFileAsync = promisify(execFile);

function run(command, args, options = {}) {
  return new Promise((resolve, reject) => {
    const child = spawn(command, args, {
      cwd: repoRoot,
      stdio: "inherit",
      ...options,
    });

    child.on("error", reject);
    child.on("exit", (code, signal) => {
      if (code === 0) {
        resolve();
        return;
      }

      reject(
        new Error(
          signal
            ? `${command} ${args.join(" ")} exited with signal ${signal}`
            : `${command} ${args.join(" ")} exited with code ${code}`,
        ),
      );
    });
  });
}

async function removeGeneratedSidecarPath(targetPath, options) {
  const relativePath = path.relative(repoRoot, targetPath);
  const { stdout } = await execFileAsync("git", ["ls-files", "--", relativePath], {
    cwd: repoRoot,
    windowsHide: true,
  });

  if (stdout.trim()) {
    throw new Error(
      `Refusing to remove tracked sidecar content: ${relativePath}. Only generated sidecar content may be cleaned.`,
    );
  }

  await rm(targetPath, options);
}

async function calculateRemoteRuntimeVersion() {
  const versionHash = createHash("sha256");
  for (const source of [
    entryPoint,
    remoteSessionEntryPoint,
    path.join(repoRoot, "pnpm-lock.yaml"),
  ]) {
    versionHash.update(await readFile(source));
  }
  return versionHash.digest("hex").slice(0, 16);
}

async function stageRemoteLinuxRuntime(sdkPackageDir, contentVersion) {
  const stagingDir = path.join(outDir, ".claude-remote-linux-x64");
  const stagingNodeModulesDir = path.join(stagingDir, "node_modules");
  const stagingSdkPackageDir = path.join(
    stagingNodeModulesDir,
    "@anthropic-ai",
    "claude-agent-sdk",
  );

  await removeGeneratedSidecarPath(stagingDir, { recursive: true, force: true });
  await removeGeneratedSidecarPath(remoteLinuxRuntimeArchiveFile, { force: true });
  await mkdir(stagingDir, { recursive: true });
  await cp(entryPoint, path.join(stagingDir, "claude-agent-sdk-server.mjs"), { force: true });
  await cp(remoteSessionEntryPoint, path.join(stagingDir, "claude-remote-session-server.mjs"), {
    force: true,
  });
  await cp(sdkPackageDir, stagingNodeModulesDir, {
    recursive: true,
    dereference: true,
    force: true,
  });
  await stageClaudeSdkPlatformAssets({
    sdkDistNodeModulesDir: stagingNodeModulesDir,
    sdkDistPackageDir: stagingSdkPackageDir,
    targetPlatform: "linux",
    targetArch: "x64",
    targetLibc: "glibc",
  });
  await writeFile(path.join(stagingDir, "claude-remote-runtime-version.txt"), `${contentVersion}\n`);
  await run(
    "tar",
    ["-czf", path.relative(stagingDir, remoteLinuxRuntimeArchiveFile), "."],
    { cwd: stagingDir },
  );
  await writeFile(remoteRuntimeVersionFile, `${contentVersion}\n`);
  await removeGeneratedSidecarPath(stagingDir, { recursive: true, force: true });
  console.log(`Built Claude SSH remote Linux runtime ${contentVersion}.`);
}

async function archiveLinuxSdkNodeModules() {
  const targetPlatform = process.env.PANES_CLAUDE_SDK_PLATFORM ?? process.platform;
  if (targetPlatform !== "linux") {
    return;
  }

  await removeGeneratedSidecarPath(linuxSdkArchiveFile, { force: true });
  await run("tar", ["-czf", path.basename(linuxSdkArchiveFile), "node_modules"], {
    cwd: outDir,
  });
  await removeGeneratedSidecarPath(sdkDistNodeModulesDir, {
    recursive: true,
    force: true,
  });
  console.log("Archived Claude SDK node_modules for Linux runtime staging.");
}

if (process.argv.includes("--print-version")) {
  console.log(await calculateRemoteRuntimeVersion());
  process.exit(0);
}

const { sdkPackageDir } = await ensureClaudeSdkPnpmLayout(repoRoot);

await removeGeneratedSidecarPath(sdkDistNodeModulesDir, {
  recursive: true,
  force: true,
});
await removeGeneratedSidecarPath(linuxSdkArchiveFile, { force: true });
await mkdir(outDir, { recursive: true });

await cp(entryPoint, outFile, {
  force: true,
});
await cp(remoteSessionEntryPoint, remoteSessionOutFile, {
  force: true,
});

await cp(sdkPackageDir, sdkDistNodeModulesDir, {
  recursive: true,
  dereference: true,
  force: true,
});

await stageClaudeSdkPlatformAssets({
  sdkDistNodeModulesDir,
  sdkDistPackageDir,
  targetPlatform: process.env.PANES_CLAUDE_SDK_PLATFORM ?? process.platform,
  targetArch: process.env.PANES_CLAUDE_SDK_ARCH ?? process.arch,
  targetLibc: process.env.PANES_CLAUDE_SDK_LIBC ?? "glibc",
});
const contentVersion = await calculateRemoteRuntimeVersion();
await stageRemoteLinuxRuntime(sdkPackageDir, contentVersion);
await archiveLinuxSdkNodeModules();

const output = await readFile(outFile, "utf8");
if (!output.includes('import("@anthropic-ai/claude-agent-sdk")')) {
  throw new Error(
    "Claude sidecar staging no longer imports @anthropic-ai/claude-agent-sdk from node_modules as expected.",
  );
}
