import { readFileSync } from 'node:fs';
import { delimiter } from 'node:path';
import { spawnSync } from 'node:child_process';

const requiredPkgConfigTargets = [
  {
    // 需要通过 pkg-config 验证的 libsoup 业务库名称。
    library: 'libsoup-3.0',
    // 需要通过 pkg-config 验证的 libsoup 最低版本。
    version: '3.0',
  },
  {
    // 需要通过 pkg-config 验证的 WebKitGTK 业务库名称。
    library: 'webkit2gtk-4.1',
    // 需要通过 pkg-config 验证的 WebKitGTK 最低版本。
    version: '2.4',
  },
];

const requiredAptPackages = [
  // GTK 基础开发头文件和链接库。
  'libglib2.0-dev',
  // GTK 3 开发头文件和链接库。
  'libgtk-3-dev',
  // WebKitGTK 4.1 开发头文件和链接库。
  'libwebkit2gtk-4.1-dev',
  // Linux 系统托盘指示器开发库。
  'libayatana-appindicator3-dev',
  // SVG 图标处理开发库。
  'librsvg2-dev',
  // Tauri Linux 打包所需的 ELF 修补工具。
  'patchelf',
];

/**
 * 将命令执行结果或命令异常转换为包含原始诊断信息的业务错误文本。
 * @param {object|Error} commandFailure 命令返回结果或启动异常。
 * @returns {string} 保留退出码、标准输出、标准错误和异常消息的诊断文本。
 */
function formatCommandFailure(commandFailure) {
  const details = [];

  if (commandFailure && typeof commandFailure === 'object') {
    if ('status' in commandFailure) {
      details.push(`status=${commandFailure.status}`);
    }
    if (commandFailure.message) {
      details.push(`error=${commandFailure.message}`);
    }
    if (commandFailure.stdout) {
      details.push(`stdout=${commandFailure.stdout}`);
    }
    if (commandFailure.stderr) {
      details.push(`stderr=${commandFailure.stderr}`);
    }
  } else if (commandFailure !== undefined && commandFailure !== null) {
    details.push(String(commandFailure));
  }

  return details.length > 0 ? details.join('; ') : '无可用的命令诊断信息';
}

/**
 * 为本地 Linux Tauri 构建准备系统开发依赖，并清理 Vulkan SDK 对 pkg-config 的路径污染。
 * @param {object} options 依赖检查与系统命令执行依赖的可注入配置。
 * @param {string} options.platform 当前运行平台标识。
 * @param {Record<string, string>} options.env 构建子进程继承的环境变量。
 * @param {Function} options.runCommand 执行系统命令并返回进程结果的函数。
 * @param {Function} options.readOsRelease 读取当前 Linux 发行版信息的函数。
 * @param {Function} options.isRoot 判断当前进程是否具有 root 权限的函数。
 * @returns {Promise<{env: Record<string, string>, installed: boolean}>} 构建环境和安装结果。
 */
export async function ensureLinuxBuildDependencies({
  // 未显式传入平台时使用当前进程的平台。
  platform = process.platform,
  // 未显式传入环境时使用当前进程环境。
  env = process.env,
  // 使用 spawnSync 执行同步系统命令，并统一以 UTF-8 读取输出。
  runCommand = (command, args, options) => {
    const result = spawnSync(command, args, {
      ...options,
      // 命令输出需要以 UTF-8 字符串参与依赖失败诊断。
      encoding: 'utf8',
    });

    if (result.error) {
      // 保留命令启动失败时可能已经产生的原始输出，供调用层输出诊断信息。
      result.error.stdout = result.stdout ?? '';
      result.error.stderr = result.stderr ?? '';
      throw result.error;
    }

    return {
      // 子进程退出码用于判断依赖检查或 apt 操作是否成功。
      status: result.status,
      // 子进程标准输出用于保留原始命令诊断信息。
      stdout: result.stdout ?? '',
      // 子进程标准错误用于保留原始命令诊断信息。
      stderr: result.stderr ?? '',
    };
  },
  // 默认读取 Linux 系统发行版描述文件。
  readOsRelease = () => readFileSync('/etc/os-release', 'utf8'),
  // 默认依据当前进程的 uid 判断是否可以直接执行 apt-get。
  isRoot = () => typeof process.getuid === 'function' && process.getuid() === 0,
} = {}) {
  if (platform !== 'linux') {
    return {
      // 非 Linux 平台不改变调用方传入的构建环境。
      env,
      // 非 Linux 平台不执行 Linux 依赖安装。
      installed: false,
    };
  }

  const cleanedEnv = { ...env };
  for (const variableName of ['PKG_CONFIG_PATH', 'PKG_CONFIG_LIBDIR']) {
    const variableValue = cleanedEnv[variableName];
    if (typeof variableValue !== 'string') {
      continue;
    }

    const filteredPaths = variableValue
      .split(delimiter)
      .filter((pathEntry) => {
        // 空路径项和 Vulkan SDK 路径都不能继续影响本次构建的 pkg-config 搜索范围。
        return pathEntry.length > 0 && !pathEntry.toLowerCase().includes('vulkan');
      });

    if (filteredPaths.length === 0) {
      delete cleanedEnv[variableName];
    } else {
      // 仅保留非 Vulkan SDK 路径，继续传递给依赖检查和最终构建子进程。
      cleanedEnv[variableName] = filteredPaths.join(delimiter);
    }
  }

  const missingLibraries = [];
  const pkgConfigDiagnostics = [];
  for (const target of requiredPkgConfigTargets) {
    let checkResult;
    try {
      checkResult = await runCommand(
        'pkg-config',
        // 使用参数数组表达“库名至少达到版本”的检查，避免通过 shell 拼接命令。
        [`--atleast-version=${target.version}`, target.library],
        {
          // pkg-config 检查必须使用已清理 Vulkan SDK 路径的环境。
          env: cleanedEnv,
        },
      );
    } catch (error) {
      throw new Error(
        `Linux 构建依赖检查 ${target.library} >= ${target.version} 启动失败：${formatCommandFailure(error)}`,
        {
          // 记录导致依赖检查启动失败的原始异常。
          cause: error,
        },
      );
    }

    if (checkResult?.error) {
      throw new Error(
        `Linux 构建依赖检查 ${target.library} >= ${target.version} 启动失败：${formatCommandFailure(checkResult)}`,
        {
          // 记录命令执行器返回的原始启动异常。
          cause: checkResult.error,
        },
      );
    }

    if (checkResult?.status !== 0) {
      missingLibraries.push(target.library);
      pkgConfigDiagnostics.push(
        `${target.library} >= ${target.version}: ${formatCommandFailure(checkResult)}`,
      );
    }
  }

  if (missingLibraries.length === 0) {
    return {
      // 依赖已经存在时返回清理后的构建环境。
      env: cleanedEnv,
      // 依赖已经存在时没有执行 apt 安装。
      installed: false,
    };
  }

  let osRelease = await readOsRelease();
  if (typeof osRelease === 'string') {
    const parsedOsRelease = {};
    for (const line of osRelease.split(/\r?\n/)) {
      const separatorIndex = line.indexOf('=');
      if (separatorIndex <= 0) {
        continue;
      }

      const key = line.slice(0, separatorIndex);
      let value = line.slice(separatorIndex + 1).trim();
      if (
        (value.startsWith('"') && value.endsWith('"')) ||
        (value.startsWith("'") && value.endsWith("'"))
      ) {
        value = value.slice(1, -1);
      }
      parsedOsRelease[key] = value;
    }
    osRelease = parsedOsRelease;
  }

  const distributionIdentifiers = [osRelease?.ID, osRelease?.ID_LIKE]
    .filter((identifier) => typeof identifier === 'string')
    .join(' ')
    .toLowerCase();
  const secondaryDistributionIdentifier = 'ubuntu';
  if (
    !distributionIdentifiers.includes('debian') &&
    !distributionIdentifiers.includes(secondaryDistributionIdentifier)
  ) {
    const supportedDistributionNames = 'Debian/Ubuntu';
    throw new Error(
      `Linux 构建依赖缺失：${missingLibraries.join(', ')}。当前系统不是 ${supportedDistributionNames}，无法自动使用 apt 安装；原始 pkg-config 信息：${pkgConfigDiagnostics.join(' | ')}`,
    );
  }

  const aptCommand = isRoot() ? 'apt-get' : 'sudo';
  const aptCommandPrefix = aptCommand === 'sudo' ? ['apt-get'] : [];

  let updateResult;
  try {
    updateResult = await runCommand(
      aptCommand,
      [...aptCommandPrefix, 'update'],
      {
        // apt 子进程同样继承清理后的构建环境，避免继续传递 Vulkan SDK 路径。
        env: cleanedEnv,
        // apt 保留终端输入以支持 sudo 密码，同时捕获标准输出和标准错误。
        stdio: ['inherit', 'pipe', 'pipe'],
      },
    );
  } catch (error) {
    throw new Error(
      `Linux 构建依赖安装失败，缺少 ${missingLibraries.join(', ')}；apt-get update 启动失败：${formatCommandFailure(error)}`,
      {
        // 记录 apt-get update 启动失败的原始异常。
        cause: error,
      },
    );
  }
  if (updateResult?.error || updateResult?.status !== 0) {
    throw new Error(
      `Linux 构建依赖安装失败，缺少 ${missingLibraries.join(', ')}；apt-get update 执行失败：${formatCommandFailure(updateResult)}`,
      {
        // 记录 apt-get update 返回的原始启动异常。
        cause: updateResult?.error,
      },
    );
  }

  let installResult;
  try {
    installResult = await runCommand(
      aptCommand,
      [...aptCommandPrefix, 'install', '-y', ...requiredAptPackages],
      {
        // apt 安装过程继续使用同一份清理后的子进程环境。
        env: cleanedEnv,
        // apt 保留终端输入以支持 sudo 密码，同时捕获标准输出和标准错误。
        stdio: ['inherit', 'pipe', 'pipe'],
      },
    );
  } catch (error) {
    throw new Error(
      `Linux 构建依赖安装失败，缺少 ${missingLibraries.join(', ')}；apt-get install 启动失败：${formatCommandFailure(error)}`,
      {
        // 记录 apt-get install 启动失败的原始异常。
        cause: error,
      },
    );
  }
  if (installResult?.error || installResult?.status !== 0) {
    throw new Error(
      `Linux 构建依赖安装失败，缺少 ${missingLibraries.join(', ')}；apt-get install 执行失败：${formatCommandFailure(installResult)}`,
      {
        // 记录 apt-get install 返回的原始启动异常。
        cause: installResult?.error,
      },
    );
  }

  const remainingLibraries = [];
  const recheckDiagnostics = [];
  for (const target of requiredPkgConfigTargets) {
    let recheckResult;
    try {
      recheckResult = await runCommand(
        'pkg-config',
        // 安装后使用与首次检查相同的安全参数数组复查目标库版本。
        [`--atleast-version=${target.version}`, target.library],
        {
          // 安装后的复查必须继续使用清理后的环境。
          env: cleanedEnv,
        },
      );
    } catch (error) {
      throw new Error(
        `Linux 构建依赖安装后复查失败，缺少 ${target.library}：${formatCommandFailure(error)}`,
        {
          // 记录安装后复查启动失败的原始异常。
          cause: error,
        },
      );
    }

    if (recheckResult?.error) {
      throw new Error(
        `Linux 构建依赖安装后复查失败，缺少 ${target.library}：${formatCommandFailure(recheckResult)}`,
        {
          // 记录复查命令返回的原始启动异常。
          cause: recheckResult.error,
        },
      );
    }

    if (recheckResult?.status !== 0) {
      remainingLibraries.push(target.library);
      recheckDiagnostics.push(
        `${target.library} >= ${target.version}: ${formatCommandFailure(recheckResult)}`,
      );
    }
  }

  if (remainingLibraries.length > 0) {
    throw new Error(
      `Linux 构建依赖安装后仍缺少 ${remainingLibraries.join(', ')}：${recheckDiagnostics.join(' | ')}`,
    );
  }

  return {
    // apt 安装和复查成功后返回清理后的构建环境。
    env: cleanedEnv,
    // 明确标识本次调用执行过依赖安装。
    installed: true,
  };
}
