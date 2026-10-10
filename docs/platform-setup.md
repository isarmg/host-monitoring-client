# 安装并配对 xsoc

本页可按所用平台直接操作。准备 [xsos 管理员](https://github.com/isarmg/xsos/blob/main/docs/instance-management.md)提供的 HTTPS 根地址和实例授权码，使用管理员终端完成安装与设置。

从 [1.0.0 Release](https://github.com/isarmg/xsoc/releases/tag/v1.0.0)下载本机原生安装包及本平台 `SHA256SUMS-*`。先核对摘要，再安装。以下文件名对应该发行页。

| 平台 | 原生包 | 服务与账户 |
|---|---|---|
| Windows 11 x64 | `xsoc-1.0.0-x64.msi` | `xsoc` / LocalService |
| Linux x86_64 | `xsoc_1.0.0_amd64.deb` 或 `xsoc-1.0.0.x86_64.rpm` | `xsoc.service` / `xsoc` |
| macOS Apple Silicon | `xsoc-1.0.0-macos-arm64-unsigned.pkg` | `org.sarmg.xsoc` / `_xsoc` |

Linux 原生构建与验证基线为 Ubuntu 24.04。macOS PKG 未签名、未公证，核验来源与摘要后，按系统提供的批准流程安装。

## Windows

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

## Linux

进入下载目录，先核对安装包摘要：

```sh
sha256sum xsoc_1.0.0_amd64.deb
```

将输出与 `SHA256SUMS-Linux-x86_64` 中对应行比较。随后执行：

```sh
sudo apt install ./xsoc_1.0.0_amd64.deb
sudo xsoc service stop
sudo xsoc setup --interactive
sudo xsoc status --check --format json
```

RPM 系统改用 `sudo dnf install ./xsoc-1.0.0.x86_64.rpm` 安装，后续设置相同。包管理器安装 smartmontools 等运行依赖。

## macOS

在下载目录核对 PKG 摘要，与`SHA256SUMS-Darwin-arm64`中同名行比较：

```sh
shasum -a 256 xsoc-1.0.0-macos-arm64-unsigned.pkg
```

一致后安装并设置：

```sh
sudo installer -pkg ./xsoc-1.0.0-macos-arm64-unsigned.pkg -target /
sudo /usr/local/bin/xsoc service stop
sudo /usr/local/bin/xsoc setup --interactive
sudo /usr/local/bin/xsoc status --check --format json
```

PKG 包含独立 smartctl 7.5，后台读取能力仍取决于设备与服务账户权限。

## 向导输入与完成结果

向导依次询问服务端 HTTPS 根地址和实例授权码。首次设置会创建缺少的配置，随后询问开机自启（默认 Yes），启动服务并验证连接。

授权码在终端中明文回显，请在受保护终端输入。将 HTTPS 地址填为根地址，例如 `https://monitor.example.com`。远程证书应被服务账户的系统信任库信任且匹配域名。

成功后到 xsos 管理台确认该主机收到新报告。配对中断时先查看 `xsoc pair status --format json`，再用 `pair resume` 继续现有事务。

## 后续维护与卸载

服务启停、配置修改和授权码更新见[日常维护](https://github.com/isarmg/xsoc/blob/main/docs/administration.md)；连接问题见[排查问题](https://github.com/isarmg/xsoc/blob/main/docs/troubleshooting.md)。

普通卸载停止并移除程序和服务，保留配置、身份、凭据和待发队列：

- Windows：在“已安装的应用”中卸载 xsoc。
- Debian/Ubuntu：`sudo apt remove xsoc`。
- RPM：`sudo dnf remove xsoc`。
- macOS：`sudo /usr/local/share/xsoc/uninstall.sh`。

卸载后系统服务应不再登记。永久清除本地状态会丢失身份及未投递报告，只在退役确认后使用平台清理选项，见日常维护。
