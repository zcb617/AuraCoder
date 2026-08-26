# CUA Driver SDK Windows 集成 Spike

这个 Spike 只验证 AuraCoder 需要的第一条技术链路，不启动 MCP，不修改用户全局配置：

1. 从 AuraCoder resources 相对路径加载官方 `cua_driver_sdk.dll`。
2. 读取并校验 C ABI 版本。
3. 在当前进程内创建 Cua Driver runtime。
4. 读取 runtime 元数据和工具目录。
5. 发起一次 `get_screen_size` 调用，记录真实运行结果。
6. 调用异步 shutdown，释放 runtime 和动态库。

## 官方包

- 版本：`cua-driver-rs-v0.19.3`
- Windows 包：`cua-driver-rs-0.19.3-windows-x86_64.zip`
- 下载地址：`https://github.com/trycua/cua/releases/download/cua-driver-rs-v0.19.3/cua-driver-rs-0.19.3-windows-x86_64.zip`
- SHA256：`e48b0117e343cec2577fc12693c741e094f389f8d4aef91e06284960bb03bce1`
- 包内必须至少存在：`cua_driver_sdk.dll`、`cua_driver_abi.h`，以及该版本声明的运行时组件。

## 编译和执行

在 Windows x86_64 环境执行：

```powershell
rustc --edition 2021 spikes/cua-driver-sdk/main.rs -o spikes/cua-driver-sdk-spike.exe
& spikes/cua-driver-sdk-spike.exe src-tauri/resources/cua-driver/windows-x86_64/cua_driver_sdk.dll
```

动态库依赖必须与 `cua_driver_sdk.dll` 放在同一个目录。程序会把每一步结果打印到标准输出；只有最后出现 `SPIKE_PASSED` 才算 Spike 通过。

## 结果判定

- `ABI` 失败：官方库与头文件版本或架构不匹配，阻断后续集成。
- `CREATE` 失败：runtime 或 Windows 依赖不可用，阻断业务层开发。
- `TOOLS` 失败：不能建立稳定工具目录，阻断引擎适配器开发。
- `GET_SCREEN_SIZE` 失败：记录具体错误；如果是 Windows 权限问题，先完成权限处理再继续。
- `SHUTDOWN` 失败：不能进入业务层，避免产生残留 runtime 或 worker。
