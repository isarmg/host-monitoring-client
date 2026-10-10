# 在 Windows 上使用 xsoc

适用范围：Windows 11 x64。其他平台见[平台入口](../platform-setup.md)。

## 安装并配对

先完成[安装前准备](../platform-setup.md#开始前准备)。从 [1.0.0 Release](https://github.com/isarmg/xsoc/releases/tag/v1.0.0) 下载 `xsoc-1.0.0-x64.msi` 与 `SHA256SUMS-Windows-AMD64`。

在管理员 PowerShell 的下载目录中计算 SHA-256，与校验文件内同名行比较：

```powershell
Get-FileHash .\xsoc-1.0.0-x64.msi -Algorithm SHA256
```

一致后安装并等待向导完成：

```powershell
$msi = (Resolve-Path .\xsoc-1.0.0-x64.msi).Path
$install = Start-Process msiexec.exe -ArgumentList "/i `"$msi`" /norestart /l*v `"$env:TEMP\xsoc-install.log`"" -Wait -PassThru
$install.ExitCode
```

退出码 0 表示成功，3010 表示成功但需重启；其他代码查看安装日志。安装完成后使用实际安装路径启动向导：

```powershell
$installRoot = (Get-ItemProperty 'HKLM:\Software\sarmg\xsoc').InstallLocation
$client = Join-Path $installRoot 'xsoc.exe'
& $client version --format json
& $client service stop
& $client setup --interactive
& $client status --check --format json
```

MSI 允许选择本机安装目录，安装全部组件并登记系统服务；安装事务结束后再配对。新终端可直接使用命令名。

向导各项输入与成功标准见[向导输入与完成结果](../platform-setup.md#向导输入与完成结果)。随后按[日常使用](../usage.md)核对服务、绑定和最新报告。

## 日志与本机状态

系统服务为 `xsoc`，运行账户为 LocalService。继续使用上面的 `$client` 路径读取日志：

```powershell
& $client service status --format json
& $client logs --tail 100 --format json
```

持久 JSON 日志默认每份 8 MiB，活动文件加四份归档共 40 MiB。`logs --follow --format ndjson --timeout 60s` 可有界跟踪；日志损坏或已过保留窗口会明确报错。

默认配置为 `C:\ProgramData\xsoc\config.json`。

## 排查本机问题

- 找不到命令：重新打开终端，或按本页安装步骤读取 `InstallLocation`，使用 `$client` 调用。
- MSI 失败：打开 `$env:TEMP\xsoc-install.log`，核对已有安装、服务路径和权限。退出码 3010 表示安装成功但需要重启。
- 服务启动失败：查看 SCM 服务退出码、系统事件和 `xsoc logs`。远程 HTTPS 的证书必须受到实际服务账户信任。
- AMD / Intel GPU 缺少读数：检查驱动及[Windows 厂商遥测](../windows-gpu-vendors.md)；MSI 自带 smartctl，设备访问仍取决于驱动和 LocalService 权限。

需要修复时用已校验的同版 MSI 修复程序与服务文件；完成后检查版本、服务和业务连接。

## 卸载

在 Windows“已安装的应用”中卸载 xsoc。普通卸载停止并移除系统服务，保留配置、身份、凭据与待发队列。永久退役且确认数据可丢弃后才使用 MSI 的 `PURGE=1` 清理选项；它会永久删除安装器管理的本地状态。

卸载完成后，对应系统服务应不再登记。

## 构建原生安装包

在原生 Windows x64 的仓库根目录执行，准备 Rust `1.99.0` MSVC 工具链、MSVC C++ 工具和 .NET SDK。WiX `4.0.6` 由项目文件固定；需要构建两个二进制并提供完整 smartmontools payload：

```powershell
$env:XSOC_BUILD_SHA = git rev-parse HEAD
cargo build --locked --release --target x86_64-pc-windows-msvc --bin xsoc --bin xsoc-maintenance
.\packaging\windows\fetch-smartmontools.ps1 -OutputDirectory "$env:TEMP\xsoc-smartmontools"
.\packaging\windows\wix\build-msi.cmd 1.0.0 target\x86_64-pc-windows-msvc\release\xsoc.exe target\x86_64-pc-windows-msvc\release\xsoc-maintenance.exe "$env:TEMP\xsoc-smartmontools\payload"
```

MSI 输出在 `packaging/windows/wix/bin/x64/Release/` 下。打包不替代真实安装器生命周期、权限和驱动验收。

通用工具链、质量检查和源码说明见[开发指南](../development.md)。原生安装器构建成功后，还需在对应系统验证安装、服务生命周期和真实设备行为。

## 继续使用

[配置](../configuration.md) · [日常使用](../usage.md) · [维护](../administration.md) · [通用排障](../troubleshooting.md) · [命令参考](../cli-compatibility.md)
