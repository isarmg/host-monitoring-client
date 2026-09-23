# 按平台安装与升级

适用于 0.9.34，配置与持久身份格式仍为 0.9.4。每个平台下载对应的单个原生 Release 安装包，并对照同页 SHA256SUMS 校验。安装器检查平台、架构、权限、旧版本状态并注册服务。安装完成后的初始化、配对和诊断命令见[完整配置指南](configuration.md)。

## Windows 11 x64

从开始菜单以管理员身份打开 PowerShell，下载并校验 Release MSI。MSI 不在安装事务中启动配对；安装完成后由同一个管理员终端显式运行 `setup --interactive`：

```powershell
cd "$env:USERPROFILE\Downloads"
Get-FileHash .\host-monitor-0.9.34-x64.msi -Algorithm SHA256
msiexec.exe /i .\host-monitor-0.9.34-x64.msi /norestart
$installRoot = (Get-ItemProperty 'HKLM:\Software\Host Monitoring\host-monitor').InstallLocation
$client = Join-Path $installRoot 'host-monitor.exe'
& $client doctor --network
```

MSI 的自定义安装页允许选择任意本机安装目录，并分别选择是否保留 `config.json`、是否保留身份/凭据/待发送采集队列，以及是否执行“Prepare incompatible account data for Setup (recommended)”。三项默认选中。最后一项不读取授权码，也不发起网络配对：它只检查本机账户格式；发现未知或损坏的配对、授权、绑定或凭据文件时，先验证 Host UUID 和 telemetry spool，再把不兼容账户文件归档为唯一名称。Host UUID 和待发送队列不在归档范围内。取消前两项会在最终提交阶段清理对应类别。向导始终显示完成页或失败页；安装后只需运行 `& $client setup --interactive`。

将示例 Server 地址替换为你的 Host Monitoring Server，在交互提示中输入管理台创建的授权码。授权码使用普通文本提示并在终端中明文回显，不提供遮罩或隐藏切换。MSI 会把用户选择的目录事务性追加到机器 PATH；新终端可直接运行 `host-monitor`，修复与升级不重复添加，卸载会移除该安装器拥有的 PATH 项。配置位于 `C:\ProgramData\host-monitor\config.json`；凭据与队列由安装器保护，服务使用 LocalService。

已有安装直接再次运行同一 MSI；原生安装器处理升级、修复、降级检查和服务登记，并保留配置、身份、队列及启动意图。安装器完成兼容性准备后，`host-monitor setup` 会把仅有本地身份、缺少完整绑定以及“账户文件已归档但 Host UUID 仍在”的状态自动选择为 recovery，并在真实终端询问新的 Server 授权码。远程核验成功的绑定可复用，未完成事务可以继续。

手动强制修复同一 MSI：

```powershell
msiexec.exe /i "$PWD\host-monitor-0.9.34-x64.msi" REINSTALL=ALL REINSTALLMODE=amus /l*v "$env:TEMP\host-monitor-repair.log"
```

原生维护失败另写入 `C:\ProgramData\host-monitor.maintenance-diagnostic-0.9.34.txt`（管理员读取）。退出码 3010 表示需要重启完成文件替换。修复不会接管指向其他程序的同名服务，也不会追踪重解析点。无人值守部署用 `ADDLOCAL=ALL` 保留两类状态并启用兼容性准备；只有部署系统明确删去 `PreserveConfiguration`、`PreserveData` 或 `PrepareSetup` 功能时才关闭对应行为。

## Linux x86_64

Debian/Ubuntu 下载 DEB；使用 APT 处理依赖并覆盖旧包：

```sh
sudo apt install ./host-monitor_0.9.34_amd64.deb
sudo host-monitor setup
```

RPM 系统使用 `sudo dnf install ./host-monitor-0.9.34.x86_64.rpm`；同版损坏重装可用 `sudo rpm -Uvh --replacepkgs ./host-monitor-0.9.34.x86_64.rpm`。DEB 同版重装使用 `sudo apt install --reinstall ./host-monitor_0.9.34_amd64.deb`。不要添加忽略依赖的参数。

配置在 `/etc/host-monitor/config.json`，服务账户为 `host-monitor`。安装器会保留已有配置和身份；安装后运行 `sudo host-monitor setup`，按提示选择启动策略并验证连接。兼容旧版账户所有权标记会被识别；包管理器保留修改过的配置，不会清除身份和待发送队列。

诊断：`systemctl status host-monitor.service`、`sudo journalctl -u host-monitor.service -n 100 --no-pager`。若 APT 上一次配置被打断，先重装上述包，再执行 `sudo dpkg --configure -a` 完成待配置包。

## macOS Apple Silicon

只提供 arm64；不支持 Intel Mac。在 Release 下载 unsigned PKG；安装包尚未签名、公证，可在系统允许的安装确认界面批准该已校验文件。

```sh
sudo installer -pkg ./host-monitor-0.9.34-macos-arm64-unsigned.pkg -target /
sudo /usr/local/bin/host-monitor setup
```

配置在 `/Library/Application Support/host-monitor/config.json`，服务账户 `_hostmonitor`，系统 LaunchDaemon 为 `org.sarmg.hostmonitor`。重复上述 `installer` 命令可覆盖兼容旧版程序及修复当前版，不必删除状态或账户；随后运行 `sudo /usr/local/bin/host-monitor setup`。安装日志在 `/var/log/install.log`；用 `sudo launchctl print system/org.sarmg.hostmonitor` 检查登记。

## 修改设置与常见配对问题

`setup` 按配置、配对、服务注册、启动策略、运行状态和连接顺序执行后置验证。交互终端明确区分 `local_binding_found`、`remotely_verified` 和 `stale_binding`，连接等待每五秒显示一次进度。只有 Server 接受凭据以及真实上报收到 HTTP 202 后才会显示相应 `verified`。JSON 失败响应中的 `error.step` 指明失败关卡，`error.code` 和脱敏 `error.detail` 给出稳定原因。连接检查使用严格外层超时；超时或 Ctrl+C 会返回非成功结果，但不会回滚已经提交的配对。

已有有效配置无须重复初始化或配对。先 `config show --format json` 查看脱敏设置和修订；修改候选文件后使用 `config validate --file <绝对路径>`、`config diff --file <绝对路径>`、`config apply --file <绝对路径> --expected-revision <当前修订>`，写操作前停止服务。

Server 必须使用系统信任的 HTTPS 证书；证书过期、名称不匹配或企业 CA 未装入系统信任库时，先修复证书。
每个实例的授权码长期有效，只有管理员显式轮换授权码或取消实例时才失效；轮换后旧 Client credential 会被
撤销，必须使用新授权码重新运行配对。一次配对请求自身有短期事务超时，这不等于实例授权码过期；网络中断后
再次运行 `setup` 会核对并恢复仍有效的同一事务，也可用 `pair status`、`pair resume` 精确检查。Server 数据库重建且旧凭据失效时，`setup` 会自动选择 `pair recover` 并提示输入新授权码；该流程保留原 Host UUID 和待发送队列，Server 仅在该 UUID 不存在时允许恢复。也可显式运行 `pair recover --interactive`。若管理员明确放弃旧身份，先运行 `queue archive --reason server-state-lost` 原子归档旧队列，再使用 `pair replace`，不得删除队列或把旧报告改属新 UUID。`status`
显示未配对时不应反复安装或删除身份文件。

0.9.3 的配置、配对、绑定与授权文档会被当前 Client 透明读取。真正未知或损坏的账户资料会返回 `pairing_state_incompatible`；创建新授权码后运行 `pair recover --interactive`，Client 会先验证采集队列，再归档旧账户文件并保留 Host UUID。若队列不可读、含隔离记录或身份不匹配，则返回 `important_state_incompatible` 并保持所有数据原样，不能通过重装或重新配对绕过。

强制覆盖仅替换安装器管理的程序和服务文件，不绕过状态格式、路径所有权或配置校验；不需要用 `PURGE=1` 解决普通安装问题。
