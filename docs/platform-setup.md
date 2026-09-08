# 按平台安装、配置与升级

适用于 0.9.9，配置与持久身份格式仍为 0.9.4。下载对应系统的 Release 安装包，并对照同页 SHA256SUMS 校验。初次安装只登记服务；完成配对后再启用开机运行。

## Windows 11 x64

从开始菜单以管理员身份打开 PowerShell。下载 MSI 和同一 Release 的 `install-host-monitor.ps1` 到同一目录：

```powershell
cd "$env:USERPROFILE\Downloads"
Get-FileHash .\host-monitor-0.9.9-x64.msi -Algorithm SHA256
powershell.exe -NoProfile -ExecutionPolicy Bypass -File .\install-host-monitor.ps1 -Msi .\host-monitor-0.9.9-x64.msi
$client = "$env:ProgramFiles\host-monitor\host-monitor.exe"
& $client config init --interactive
& $client pair --server https://monitor.example.com --interactive
& $client service enable --now
& $client status
& $client doctor --network
```

将示例 Server 地址替换为你的 Host Monitoring Server，在交互提示中输入管理台创建的授权码。无需将程序目录加入 PATH。配置位于 `C:\ProgramData\host-monitor\config.json`；凭据与队列由安装器保护，服务使用 LocalService。

已有安装直接再次运行上述安装脚本。脚本验证 MSI 产品，自动处理早期无升级标识的 MSI 登记，等待并重试安装器忙碌（1618），强制修复同版本程序文件。新版 MSI 支持后续原位升级，并修复正确指向本产品、但文件已丢失的残留服务。正常覆盖及卸载保留配置、身份、队列。安装完成后检查 `service status`，按需要重新 `service enable --now`。

手动强制修复同一 MSI：

```powershell
msiexec.exe /i "$PWD\host-monitor-0.9.9-x64.msi" REINSTALL=ALL REINSTALLMODE=amus /l*v "$env:TEMP\host-monitor-repair.log"
```

安装脚本会打印详细日志目录。原生维护失败另写入 `C:\ProgramData\host-monitor.maintenance-diagnostic-0.9.9.txt`（管理员读取）。退出码 3010 表示需要重启完成文件替换。修复不会接管指向其他程序的同名服务，也不会追踪重解析点或删除未知数据。

## Linux x86_64

Debian/Ubuntu 下载 DEB；使用 APT 处理依赖并覆盖旧包：

```sh
sudo apt install ./host-monitor_0.9.9_amd64.deb
sudo host-monitor config init --interactive
sudo host-monitor pair --server https://monitor.example.com --interactive
sudo host-monitor service enable --now
sudo host-monitor status
sudo host-monitor doctor --network
```

RPM 系统使用 `sudo dnf install ./host-monitor-0.9.9.x86_64.rpm`；同版损坏重装可用 `sudo rpm -Uvh --replacepkgs ./host-monitor-0.9.9.x86_64.rpm`。DEB 同版重装使用 `sudo apt install --reinstall ./host-monitor_0.9.9_amd64.deb`。不要添加忽略依赖的参数。

配置在 `/etc/host-monitor/config.json`，服务账户为 `host-monitor`。更新程序前执行 `sudo host-monitor service stop`，安装后检查配置，再 `sudo host-monitor service enable --now`。兼容旧版账户所有权标记会被识别；包管理器保留修改过的配置，不会清除身份和待发送队列。

诊断：`systemctl status host-monitor.service`、`sudo journalctl -u host-monitor.service -n 100 --no-pager`。若 APT 上一次配置被打断，先重装上述包，再执行 `sudo dpkg --configure -a` 完成待配置包。

## macOS Apple Silicon

只提供 arm64；不支持 Intel Mac。在 Release 下载 unsigned PKG；安装包尚未签名、公证，可在系统允许的安装确认界面批准该已校验文件。

```sh
sudo installer -pkg ./host-monitor-0.9.9-macos-arm64-unsigned.pkg -target /
sudo /usr/local/bin/host-monitor config init --interactive
sudo /usr/local/bin/host-monitor pair --server https://monitor.example.com --interactive
sudo /usr/local/bin/host-monitor service enable --now
sudo /usr/local/bin/host-monitor status
sudo /usr/local/bin/host-monitor doctor --network
```

配置在 `/Library/Application Support/host-monitor/config.json`，服务账户 `_hostmonitor`，系统 LaunchDaemon 为 `org.sarmg.hostmonitor`。更新前停止服务，重复上述 `installer` 命令可覆盖兼容旧版程序及修复当前版，不必删除状态或账户。完成后按需要启用服务。安装日志在 `/var/log/install.log`；用 `sudo launchctl print system/org.sarmg.hostmonitor` 检查登记。

## 修改设置与常见配对问题

已有有效配置无须重复初始化或配对。先 `config show --format json` 查看脱敏设置和修订；修改候选文件后使用 `config validate --file <绝对路径>`、`config diff --file <绝对路径>`、`config apply --file <绝对路径> --expected-revision <当前修订>`，写操作前停止服务。

Server 必须使用系统信任的 HTTPS 证书；证书过期、名称不匹配或企业 CA 未装入系统信任库时，先修复证书。授权码过期需从 Server 管理台重新申请；网络中断后先用 `pair resume` 核对原事务。`status` 显示未配对时不应反复安装或删除身份文件。

强制覆盖仅替换安装器管理的程序和服务文件，不绕过状态格式、路径所有权或配置校验；不需要用 `PURGE=1` 解决普通安装问题。
