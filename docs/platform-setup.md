# xsoc 平台安装入口

按主机平台打开对应指南。每页集中说明该平台的安装、源码构建、日志、常见问题和卸载；通用向导输入与完成结果在本页下方。

## Linux

[打开 Linux 指南](https://github.com/isarmg/xsoc/blob/main/docs/platforms/linux.md)：Linux x86_64 的 DEB / RPM；Ubuntu 24.04 为构建与验证基线。

## Windows

[打开 Windows 指南](https://github.com/isarmg/xsoc/blob/main/docs/platforms/windows.md)：Windows 11 x64 的 MSI、管理员 PowerShell 和系统服务。

## macOS

[打开 macOS 指南](https://github.com/isarmg/xsoc/blob/main/docs/platforms/macos.md)：Apple Silicon arm64 的未签名 PKG、launchd 和本机日志。

## 移动宿主库

- [Android 宿主集成](https://github.com/isarmg/xsoc/blob/main/docs/platforms/android.md)：aarch64 Rust 库检查与 Android 宿主职责
- [iOS / iPadOS 宿主集成](https://github.com/isarmg/xsoc/blob/main/docs/platforms/ios.md)：arm64 真机和模拟器 Rust 库检查

移动端提供供原生应用接入的报告构造库；桌面服务安装步骤不适用于移动宿主。

## 开始前准备

准备 [xsos 管理员](https://github.com/isarmg/xsos/blob/main/docs/instance-management.md)提供的 HTTPS 根地址和实例授权码，使用管理员终端完成安装与设置。

下载入口：[1.0.0 Release](https://github.com/isarmg/xsoc/releases/tag/v1.0.0)。各平台指南列出准确的原生包和校验文件名。安装前核对摘要；已发布包的源码身份以对应 manifest 和 `version` 输出为准。

## 向导输入与完成结果

向导依次询问服务端 HTTPS 根地址和实例授权码。首次设置会创建缺少的配置，随后询问开机自启（默认 Yes），启动服务并验证连接。

授权码在终端中明文回显，请在受保护终端输入。将 HTTPS 地址填为根地址，例如 `https://monitor.example.com`。远程证书应被服务账户的系统信任库信任且匹配域名。

成功后到 xsos 管理台确认该主机收到新报告。配对中断时先查看 `xsoc pair status --format json`，再用 `pair resume` 继续现有事务。

## 后续维护与卸载

完成安装后查看[日常使用](https://github.com/isarmg/xsoc/blob/main/docs/usage.md)、[配置指南](https://github.com/isarmg/xsoc/blob/main/docs/configuration.md)和[日常维护](https://github.com/isarmg/xsoc/blob/main/docs/administration.md)。连接与配对问题见[通用排障](https://github.com/isarmg/xsoc/blob/main/docs/troubleshooting.md)，安装器修复和卸载步骤见上方对应平台指南。
