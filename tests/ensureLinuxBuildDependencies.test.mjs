import { describe, expect, it } from "vitest";

import { ensureLinuxBuildDependencies } from "../scripts/ensure-linux-build-deps.mjs";

const requiredPkgConfigLibraries = ["libsoup-3.0", "webkit2gtk-4.1"];

const requiredAptPackages = [
  "libglib2.0-dev",
  "libgtk-3-dev",
  "libwebkit2gtk-4.1-dev",
  "libayatana-appindicator3-dev",
  "librsvg2-dev",
  "patchelf",
];

/**
 * 创建用于模拟 pkg-config 检查与 apt 安装流程的系统命令桩。
 * @param {object} options 系统命令结果配置。
 * @param {number[]} options.pkgConfigStatuses 按调用顺序返回的 pkg-config 状态码。
 * @returns {{ calls: Array<object>, runCommand: Function }} 系统命令记录和注入函数。
 */
function createCommandStub({ pkgConfigStatuses }) {
  const calls = [];
  let pkgConfigCallIndex = 0;

  /** 记录构建依赖命令并返回预设进程结果。 */
  async function runCommand(command, args, options) {
    calls.push({
      // 实际执行的命令名称，用于验证依赖检查和安装动作。
      command,
      // 实际传入的命令参数，用于验证目标库和安装包。
      args,
      // 实际传入的进程执行选项，用于保留命令调用契约。
      options,
    });

    if (command === "pkg-config") {
      const status = pkgConfigStatuses[pkgConfigCallIndex] ?? 1;
      pkgConfigCallIndex += 1;
      return {
        // 模拟 pkg-config 或系统命令的进程退出状态。
        status,
        // 模拟命令标准输出，依赖检查成功时为空即可。
        stdout: "",
        // 模拟依赖缺失时的错误输出，辅助生产代码构造错误信息。
        stderr: status === 0 ? "" : "dependency is missing",
      };
    }

    return {
      // 模拟 apt 命令成功完成。
      status: 0,
      // apt 命令的标准输出不影响本阶段业务判断。
      stdout: "",
      // apt 命令的错误输出不影响本阶段业务判断。
      stderr: "",
    };
  }

  return {
    // 记录依赖检查、提权和 apt 命令，供测试验证实际系统操作。
    calls,
    // 注入预设系统命令结果，隔离测试与真实操作系统环境。
    runCommand,
  };
}

/** 判断命令记录是否为指定的 sudo apt-get 子命令。 */
function isSudoAptCommand(call, subcommand) {
  return (
    call.command === "sudo" &&
    call.args[0] === "apt-get" &&
    call.args[1] === subcommand
  );
}

describe("Linux 构建系统依赖自动准备", () => {
  it("依赖已存在时跳过 apt，并清理 Vulkan pkg-config 路径", async () => {
    const env = {
      // 构建进程继承的 pkg-config 搜索路径，其中包含 Vulkan SDK 路径。
      PKG_CONFIG_PATH:
        "/opt/vulkan-sdk/lib/pkgconfig:/usr/lib/pkgconfig:/workspace/lib/pkgconfig",
      // 构建进程继承的 pkg-config 库目录，其中包含 Vulkan SDK 路径。
      PKG_CONFIG_LIBDIR:
        "/opt/vulkan-sdk/lib:/usr/lib/x86_64-linux-gnu/pkgconfig",
      // 与依赖准备无关的构建环境变量，必须继续传递给构建进程。
      OTHER_BUILD_FLAG: "keep-me",
    };
    const commandStub = createCommandStub({
      // 两个目标库首次检查均成功，表示系统依赖已经存在。
      pkgConfigStatuses: [0, 0],
    });

    const result = await ensureLinuxBuildDependencies({
      // 只在 Linux 平台执行系统依赖检查。
      platform: "linux",
      // 注入待清理并继续传递给构建命令的环境变量。
      env,
      // 注入系统命令桩，避免测试触发真实 sudo 或 apt。
      runCommand: commandStub.runCommand,
      // 依赖已存在时不应读取发行版信息。
      readOsRelease: () => {
        throw new Error("readOsRelease should not be called when dependencies exist");
      },
      // 依赖已存在时不应判断 root 权限。
      isRoot: () => {
        throw new Error("isRoot should not be called when dependencies exist");
      },
    });

    const pkgConfigCalls = commandStub.calls.filter(
      (call) => call.command === "pkg-config",
    );
    expect(pkgConfigCalls).toHaveLength(2);
    for (const library of requiredPkgConfigLibraries) {
      expect(pkgConfigCalls.some((call) => call.args.includes(library))).toBe(true);
    }
    expect(commandStub.calls.some((call) => call.command === "sudo")).toBe(false);
    expect(commandStub.calls.some((call) => call.command === "apt-get")).toBe(false);
    expect(result).toEqual({
      // 返回经过 Vulkan SDK 路径清理的构建环境。
      env: {
        // 保留非 Vulkan 的 pkg-config 搜索路径。
        PKG_CONFIG_PATH: "/usr/lib/pkgconfig:/workspace/lib/pkgconfig",
        // 保留非 Vulkan 的 pkg-config 库目录。
        PKG_CONFIG_LIBDIR: "/usr/lib/x86_64-linux-gnu/pkgconfig",
        // 保留其他构建环境变量。
        OTHER_BUILD_FLAG: "keep-me",
      },
      // 依赖已存在，因此没有执行安装。
      installed: false,
    });
  });

  it("Ubuntu 缺少依赖且当前用户非 root 时使用 sudo apt 安装并复查", async () => {
    const commandStub = createCommandStub({
      // 前两次检查失败，apt 安装后两次复查成功。
      pkgConfigStatuses: [1, 1, 0, 0],
    });

    const result = await ensureLinuxBuildDependencies({
      // Linux 平台需要负责系统构建依赖准备。
      platform: "linux",
      // 提供不包含特殊路径的构建环境，验证安装结果仍返回环境对象。
      env: { BUILD_MODE: "release" },
      // 记录 pkg-config 与 apt 命令，验证不会触发真实安装。
      runCommand: commandStub.runCommand,
      // 模拟 Ubuntu 的操作系统识别结果。
      readOsRelease: () => ({
        // 模拟 Debian 系发行版标识，允许进入 apt 安装流程。
        ID: "ubuntu",
      }),
      // 模拟当前进程不是 root，需要使用 sudo 提权。
      isRoot: () => false,
    });

    const updateCall = commandStub.calls.find((call) =>
      isSudoAptCommand(call, "update"),
    );
    const installCall = commandStub.calls.find((call) =>
      isSudoAptCommand(call, "install"),
    );

    expect(updateCall).toBeDefined();
    expect(installCall).toBeDefined();
    expect(commandStub.calls.indexOf(updateCall)).toBeLessThan(
      commandStub.calls.indexOf(installCall),
    );
    expect(installCall.args).toContain("-y");
    for (const packageName of requiredAptPackages) {
      expect(installCall.args).toContain(packageName);
    }
    expect(commandStub.calls.filter((call) => call.command === "pkg-config")).toHaveLength(4);
    expect(result).toEqual({
      // 返回 apt 安装后继续用于构建的环境。
      env: {
        // 构建模式环境变量应继续传递给构建命令。
        BUILD_MODE: "release",
      },
      // 依赖缺失后已完成安装。
      installed: true,
    });
  });

  it("非 Debian/Ubuntu 系统缺少依赖时不调用 apt 并抛出明确错误", async () => {
    const commandStub = createCommandStub({
      // 首个目标库检查失败，足以触发发行版判断。
      pkgConfigStatuses: [1],
    });

    await expect(
      ensureLinuxBuildDependencies({
        // Linux 平台需要检查构建依赖。
        platform: "linux",
        // 提供待返回的构建环境。
        env: { BUILD_MODE: "release" },
        // 记录依赖检查命令，验证不会进入安装流程。
        runCommand: commandStub.runCommand,
        // 模拟 Fedora，验证非 Debian/Ubuntu 系统的错误分支。
        readOsRelease: () => ({
          // 模拟非 Debian/Ubuntu 系发行版标识，验证明确报错分支。
          ID: "fedora",
        }),
        // 发行版不支持时无需依赖 root 权限判断。
        isRoot: () => false,
      }),
    ).rejects.toThrow(/Debian\/Ubuntu|apt/i);

    expect(commandStub.calls.some((call) => call.command === "sudo")).toBe(false);
    expect(commandStub.calls.some((call) => call.command === "apt-get")).toBe(false);
  });

  it("apt 安装后依赖复查仍失败时抛出缺失库名称错误", async () => {
    const commandStub = createCommandStub({
      // 首次检查与安装后的复查均失败，验证错误包含具体目标库名称。
      pkgConfigStatuses: [1, 1, 1, 1],
    });

    await expect(
      ensureLinuxBuildDependencies({
        // Linux 平台需要检查并准备构建依赖。
        platform: "linux",
        // 提供待返回的构建环境。
        env: {
          // 构建模式环境变量应继续传递给构建命令。
          BUILD_MODE: "release",
        },
        // 记录依赖检查和 apt 安装命令。
        runCommand: commandStub.runCommand,
        // 模拟 Ubuntu，允许执行 apt 安装流程。
        readOsRelease: () => ({
          // 模拟 Debian 系发行版标识，允许执行 apt 安装。
          ID: "ubuntu",
        }),
        // 模拟当前用户为 root，不需要 sudo 才能安装。
        isRoot: () => true,
      }),
    ).rejects.toThrow(/libsoup-3\.0|webkit2gtk-4\.1/);

    expect(commandStub.calls.some((call) =>
      isSudoAptCommand(call, "update"),
    )).toBe(false);
    expect(commandStub.calls.some((call) =>
      isSudoAptCommand(call, "install"),
    )).toBe(false);
    expect(commandStub.calls.some((call) =>
      call.command === "apt-get" && call.args[0] === "update",
    )).toBe(true);
    expect(commandStub.calls.some((call) =>
      call.command === "apt-get" && call.args[0] === "install",
    )).toBe(true);
  });

  it("非 Linux 平台不调用任何命令并原样返回环境", async () => {
    const env = {
      // Windows 构建使用的原始 pkg-config 路径必须保持不变。
      PKG_CONFIG_PATH: "C:/VulkanSDK/lib/pkgconfig;C:/pkgconfig",
      // Windows 构建使用的原始 pkg-config 库目录必须保持不变。
      PKG_CONFIG_LIBDIR: "C:/VulkanSDK/lib;C:/pkgconfig/lib",
      // 非依赖相关的环境变量必须保持不变。
      OTHER_BUILD_FLAG: "keep-me",
    };
    const commandStub = createCommandStub({
      // Windows 分支不应读取这些状态，但仍提供默认命令结果以满足接口。
      pkgConfigStatuses: [],
    });

    const result = await ensureLinuxBuildDependencies({
      // Windows 平台不需要 Linux 系统依赖准备。
      platform: "win32",
      // 注入必须原样返回的构建环境。
      env,
      // 注入命令桩，验证完全不会调用系统命令。
      runCommand: commandStub.runCommand,
      // 非 Linux 平台不应读取发行版信息。
      readOsRelease: () => {
        throw new Error("readOsRelease should not be called on non-Linux platforms");
      },
      // 非 Linux 平台不应判断 root 权限。
      isRoot: () => {
        throw new Error("isRoot should not be called on non-Linux platforms");
      },
    });

    expect(commandStub.calls).toEqual([]);
    expect(result).toEqual({
      // 非 Linux 平台原样返回调用方提供的构建环境。
      env,
      // 非 Linux 平台不执行依赖安装。
      installed: false,
    });
  });
});
