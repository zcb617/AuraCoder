import { access, lstat, realpath } from "node:fs/promises";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { spawn } from "node:child_process";

function isWithin(parentPath, candidatePath) {
  const relativePath = path.relative(parentPath, candidatePath);
  return (
    relativePath === "" ||
    (!relativePath.startsWith(`..${path.sep}`) && relativePath !== ".." && !path.isAbsolute(relativePath))
  );
}

async function inspectClaudeSdkPnpmLayout(repoRoot) {
  const nodeModulesDir = path.join(repoRoot, "node_modules");
  const pnpmDir = path.join(nodeModulesDir, ".pnpm");
  const sdkLinkPath = path.join(nodeModulesDir, "@anthropic-ai", "claude-agent-sdk");

  let sdkLinkStats;
  try {
    sdkLinkStats = await lstat(sdkLinkPath);
  } catch (error) {
    if (error?.code === "ENOENT") {
      return {
        valid: false,
        reason: "Claude Agent SDK is missing from node_modules.",
      };
    }
    throw error;
  }

  if (!sdkLinkStats.isSymbolicLink()) {
    return {
      valid: false,
      reason: "Claude Agent SDK is not a pnpm junction or symbolic link.",
    };
  }

  const sdkTargetPath = await realpath(sdkLinkPath);
  if (!isWithin(pnpmDir, sdkTargetPath)) {
    return {
      valid: false,
      reason: "Claude Agent SDK junction does not target node_modules/.pnpm.",
    };
  }

  let sdkEntryPoint;
  try {
    sdkEntryPoint = fileURLToPath(import.meta.resolve("@anthropic-ai/claude-agent-sdk"));
  } catch (error) {
    return {
      valid: false,
      reason: `Claude Agent SDK cannot be resolved: ${error.message}`,
    };
  }

  const sdkPackageDir = path.resolve(path.dirname(sdkEntryPoint), "..", "..");
  if (!isWithin(pnpmDir, sdkPackageDir)) {
    return {
      valid: false,
      reason: "Claude sidecar copy source resolves outside node_modules/.pnpm.",
    };
  }

  const remoteLinuxSdkBinary = path.join(
    sdkPackageDir,
    "@anthropic-ai",
    "claude-agent-sdk-linux-x64",
    "claude",
  );
  try {
    await access(remoteLinuxSdkBinary);
  } catch (error) {
    if (error?.code === "ENOENT") {
      return {
        valid: false,
        reason: "Claude Agent SDK Linux x64 runtime is missing from node_modules.",
      };
    }
    throw error;
  }

  return {
    valid: true,
    sdkEntryPoint,
    sdkPackageDir,
  };
}

function rebuildDependencies(repoRoot) {
  return new Promise((resolve, reject) => {
    const child = spawn("pnpm", ["install", "--frozen-lockfile"], {
      cwd: repoRoot,
      stdio: "inherit",
      shell: process.platform === "win32",
      windowsHide: true,
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
            ? `pnpm dependency recovery exited with signal ${signal}`
            : `pnpm dependency recovery exited with code ${code}`,
        ),
      );
    });
  });
}

export async function ensureClaudeSdkPnpmLayout(repoRoot) {
  let layout = await inspectClaudeSdkPnpmLayout(repoRoot);
  if (layout.valid) {
    return layout;
  }

  console.warn(`Claude SDK pnpm layout is invalid: ${layout.reason}`);
  console.warn("Rebuilding dependencies from pnpm-lock.yaml before packaging.");
  await rebuildDependencies(repoRoot);

  layout = await inspectClaudeSdkPnpmLayout(repoRoot);
  if (!layout.valid) {
    throw new Error(
      `Claude SDK pnpm layout is still invalid after dependency recovery: ${layout.reason}. Packaging stopped.`,
    );
  }

  console.log("Claude SDK pnpm layout recovered successfully.");
  return layout;
}
