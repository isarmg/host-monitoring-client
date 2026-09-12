# 按平台安装、配置与升级

适用于 0.9.11，配置与持久身份格式仍为 0.9.4。每个平台下载对应的单个原生 Release 安装包，并对照同页 SHA256SUMS 校验。安装器检查平台、架构、权限、旧版本状态并注册服务，随后使用 `setup` 完成配对、服务策略和连接验证。

## Windows 11 x64

从开始菜单以管理员身份打开 PowerShell，下载并校验 Release MSI。普通安装不要使用 `/qn`，MSI 提交部署后会在交互终端中直接启动 `setup --interactive`：

```powershell
cd "$env:USERPROFILE\Downloads"
Get-FileHash .\host-monitor-0.9.11-x64.msi -Algorithm SHA256
msiexec.exe /i .\host-monitor-0.9.11-x64.msi /norestart
$client = "$env:ProgramFiles\host-monitor\host-monitor.exe"
& $client doctor --network
```

如果必须使用 `/qn` 或其他无终端方式，安装器只完成部署和服务登记；随后在管理员终端显式运行 `& "$env:ProgramFiles\host-monitor\host-monitor.exe" setup --interactive`。配对失败不会让 MSI 回滚已提交的安装。

将示例 Server 地址替换为你的 Host Monitoring Server，在交互提示中输入管理台创建的授权码。MSI 会把 `C:\Program Files\host-monitor` 事务性追加到机器 PATH；新终端可直接运行 `host-monitor`，修复与升级不重复添加，卸载会移除该安装器拥有的 PATH 项。配置位于 `C:\ProgramData\host-monitor\config.json`；凭据与队列由安装器保护，服务使用 LocalService。

已有安装直接再次运行同一 MSI；原生安装器处理升级、修复、降级检查和服务登记，并保留配置、身份、队列及启动意图。需要再次设置时运行 `host-monitor setup`：有效身份会复用，未完成事务会恢复，不会强制重新配对。

手动强制修复同一 MSI：

```powershell
msiexec.exe /i "$PWD\host-monitor-0.9.11-x64.msi" REINSTALL=ALL REINSTALLMODE=amus /l*v "$env:TEMP\host-monitor-repair.log"
```

原生维护失败另写入 `C:\ProgramData\host-monitor.maintenance-diagnostic-0.9.11.txt`（管理员读取）。退出码 3010 表示需要重启完成文件替换。修复不会接管指向其他程序的同名服务，也不会追踪重解析点或删除未知数据。

## Linux x86_64

Debian/Ubuntu 下载 DEB；使用 APT 处理依赖并覆盖旧包：

```sh
sudo apt install ./host-monitor_0.9.11_amd64.deb
sudo host-monitor setup
```

RPM 系统使用 `sudo dnf install ./host-monitor-0.9.11.x86_64.rpm`；同版损坏重装可用 `sudo rpm -Uvh --replacepkgs ./host-monitor-0.9.11.x86_64.rpm`。DEB 同版重装使用 `sudo apt install --reinstall ./host-monitor_0.9.11_amd64.deb`。不要添加忽略依赖的参数。

配置在 `/etc/host-monitor/config.json`，服务账户为 `host-monitor`。安装器会保留已有配置和身份；安装后运行 `sudo host-monitor setup`，按提示选择启动策略并验证连接。兼容旧版账户所有权标记会被识别；包管理器保留修改过的配置，不会清除身份和待发送队列。

诊断：`systemctl status host-monitor.service`、`sudo journalctl -u host-monitor.service -n 100 --no-pager`。若 APT 上一次配置被打断，先重装上述包，再执行 `sudo dpkg --configure -a` 完成待配置包。

## macOS Apple Silicon

只提供 arm64；不支持 Intel Mac。在 Release 下载 unsigned PKG；安装包尚未签名、公证，可在系统允许的安装确认界面批准该已校验文件。

```sh
sudo installer -pkg ./host-monitor-0.9.11-macos-arm64-unsigned.pkg -target /
sudo /usr/local/bin/host-monitor setup
```

配置在 `/Library/Application Support/host-monitor/config.json`，服务账户 `_hostmonitor`，系统 LaunchDaemon 为 `org.sarmg.hostmonitor`。重复上述 `installer` 命令可覆盖兼容旧版程序及修复当前版，不必删除状态或账户；随后运行 `sudo /usr/local/bin/host-monitor setup`。安装日志在 `/var/log/install.log`；用 `sudo launchctl print system/org.sarmg.hostmonitor` 检查登记。

## 修改设置与常见配对问题

`setup` 按配置、配对、服务注册、启动策略、运行状态和连接顺序执行后置验证。交互终端会逐步显示 `verified`；JSON 失败响应中的 `error.step` 指明失败关卡，`error.code` 和 `error.message` 给出稳定原因，操作系统服务命令失败时 `error.detail` 保留经过控制字符清理和长度限制的原始诊断。请求连接验证但服务未运行会直接失败，不再静默跳过后仍报告完成。

已有有效配置无须重复初始化或配对。先 `config show --format json` 查看脱敏设置和修订；修改候选文件后使用 `config validate --file <绝对路径>`、`config diff --file <绝对路径>`、`config apply --file <绝对路径> --expected-revision <当前修订>`，写操作前停止服务。

Server 必须使用系统信任的 HTTPS 证书；证书过期、名称不匹配或企业 CA 未装入系统信任库时，先修复证书。授权码过期需从 Server 管理台重新申请；网络中断后再次运行 `setup` 会核对并恢复原事务，也可用 `pair status`、`pair resume` 精确检查。`status` 显示未配对时不应反复安装或删除身份文件。

强制覆盖仅替换安装器管理的程序和服务文件，不绕过状态格式、路径所有权或配置校验；不需要用 `PURGE=1` 解决普通安装问题。
