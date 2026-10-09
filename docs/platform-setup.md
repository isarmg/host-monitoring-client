# xsoc 分平台部署与维护

适用于 `xsoc 1.0.0`。请按所在平台从安装一直执行到业务验收；后续重新配对、服务启停和卸载也在同一平台章节中。安装器的状态兼容、修复和日志细节见本文末尾；配置字段与自动化输入见[配置指南](configuration.md)。

## 阅读前准备

1. 在 [Client Releases](https://github.com/isarmg/xsoc/releases) 选择对应版本，下载本平台安装包和同版 `SHA256SUMS-*`。本文文件名以 1.0.0 为例，安装其他版本时应同时替换包名和配套文档。
2. 请 Server 管理员创建主机实例，提供 Server 的 HTTPS 根地址和实例授权码。客户端主动向外连接，不需要为 Client 开放入站端口。
3. 确认机器时间、DNS、网络和 Server 证书正常。授权码在交互终端中明文回显，请使用受保护终端；不要把码或 token 拼进命令参数。
4. Windows 使用管理员 PowerShell；Linux/macOS 使用可执行 `sudo` 的终端。下文 `#` 开头是命令解释，可与命令一起粘贴。`CURRENT_HOST_UUID` 必须替换成 `pair status` 中的实际 Host UUID，不能照抄。

| 平台 | 程序/服务 | 配置与默认持久状态 |
|---|---|---|
| Windows 11 x64 | MSI 选择的目录；SCM 服务 `xsoc`，账户 LocalService | `C:\ProgramData\xsoc\config.json`；默认状态在该目录 |
| Linux x86_64 | `xsoc`；systemd `xsoc.service`，账户 `xsoc` | `/etc/xsoc/config.json`；默认状态 `/var/lib/xsoc` |
| macOS Apple Silicon | `/usr/local/bin/xsoc`；launchd `org.sarmg.xsoc`，账户 `_xsoc` | `/Library/Application Support/xsoc/config.json`；默认状态在该目录 |

配置中的 `state_dir` 决定实际身份、凭据和队列位置；自定义路径要以脱敏 `config show` 为准。`--format json` 便于查看结构化结果，不能使一条失败命令变成成功。

### 服务与业务状态的区别

- `service status`：查看操作系统是否登记服务、进程是否运行以及开机策略。
- `pair status`：查看本机配对绑定及未完成事务，不代表 Server 已接受报告。
- `status --check`：检查当前 Client 状态及连接相关检查结果。
- `doctor --network`：访问公开 `GET /healthz`，正常为 HTTP 204，不使用报告凭据。
- `doctor --delivery`、`once`：会产生真实采集/投递，必须停掉后台服务后再执行，避免两份运行实例同时操作队列。

## Windows 11 x64

### 1. 安装与版本确认

进入存放安装包的目录，对比哈希与同版校验文件中的对应行；一致后安装。下面等待 MSI 退出，防止服务尚未登记就开始设置。

```powershell
# 切换到下载目录；如果下载到了别处，请替换此路径。
Set-Location "$env:USERPROFILE\Downloads"
# 计算 MSI 的 SHA-256；人工对比同版 SHA256SUMS-Windows-* 中该文件的哈希。
Get-FileHash .\xsoc-1.0.0-x64.msi -Algorithm SHA256
# 安装全部组件，等待向导完成，并将安装日志写到临时目录；不自动重启机器。
$msi = (Resolve-Path .\xsoc-1.0.0-x64.msi).Path
$install = Start-Process msiexec.exe -ArgumentList "/i `"$msi`" /norestart /l*v `"$env:TEMP\xsoc-install.log`"" -Wait -PassThru
# 显示安装结果：0 成功，3010 成功但需重启；其他码先查安装日志。
$install.ExitCode
```

安装成功后执行下一组。MSI 支持选择本机安装目录；当前终端可能尚未刷新 PATH，因此先通过注册表取真实路径。

```powershell
# 获取安装器记录的程序目录，并组合可执行文件路径；& 用来执行变量中的程序路径。
$installRoot = (Get-ItemProperty 'HKLM:\Software\xsos\xsoc').InstallLocation
$client = Join-Path $installRoot 'xsoc.exe'
# 确认安装的版本、平台和状态格式。
& $client version --format json
# 检查后台服务登记和启动策略。
& $client service status --format json
```

### 2. 首次配对和启动

```powershell
# 停止可能运行的旧服务，为设置、配置和身份写入留出独占窗口。
& $client service stop
# 运行完整交互向导：初始化/复用配置、输入 Server 和授权码、配对、设置自启、启动并验证。
& $client setup --interactive
# 查看本机绑定及事务状态，核对 Host UUID。
& $client pair status --format json
# 核对服务是否 running，开机策略是否符合向导选择。
& $client service status --format json
# 查看业务检查结果；最后到 Server 管理页确认该主机有新报告。
& $client status --check --format json
```

首次安装不需要先手工 `config init`；`setup` 会处理缺少的配置。开机自启提示默认 Yes，按 Enter 接受；无论是否选择自启，配对后都会立即启动并验证连接。成功判据是向导完成、服务运行、Server 收到新报告。超时或中断可能发生在身份已保存之后，先检查 `pair status`。

### 3. 重新配对与恢复

先由管理员提供同一实例的新授权码，再执行：

```powershell
# 停止服务并查看当前绑定，保留输出里的 Host UUID。
& $client service stop
& $client pair status --format json
# 恢复原 Host UUID 的绑定，适用于授权码轮换、凭据失效或 Server 状态恢复。
& $client pair recover --interactive
# 配对确认后启动服务，再验证业务状态。
& $client pair status --format json
& $client service start
& $client status --check --format json
```

只遇到请求中断、响应丢失时，优先 `& $client pair resume --interactive` 恢复原事务；再次运行 `setup` 也会按现有状态选择恢复。更换实例/Server 并放弃旧绑定，请执行本文“更换绑定与队列处理”，不要删除身份或队列文件。

### 4. 服务查看、启停与自启

每条命令按实际需要单独执行，不要将整组依次执行。

```powershell
# 查看服务登记、运行状态和开机策略。
& $client service status --format json
# 立即启动；不改变开机策略。
& $client service start
# 立即停止；不改变开机策略。
& $client service stop
# 先停再启动，常用于修复后重新加载运行状态。
& $client service restart
# 设为开机自动启动；不立即启动当前已停止的服务。
& $client service enable
# 取消开机自启，改为手动启动；不立即停止当前进程。
& $client service disable
# 同时取消自启并立即停止。
& $client service disable --now
# 用 Windows 原生命令核对服务状态和程序路径。
Get-Service -Name xsoc
sc.exe qc xsoc
```

### 5. 问题诊断

```powershell
# 查看脱敏配置，不要用 Get-Content 输出原始配置和凭据。
& $client config show --format json
# 只检查 Server 的公开健康入口，定位 DNS、TLS、网络问题。
& $client doctor --network --format json
# 查看最近 100 条后台持久日志。
& $client logs --tail 100 --format json
# 获取 SCM 服务路径和启动配置；启动早期失败也应查看事件查看器的 System/SCM 事件。
sc.exe qc xsoc
# 停服后单独检查本机采集，再产生一次真实投递，区分采集问题和报告认证问题。
& $client service stop
& $client probe --format json
& $client doctor --delivery --format json
# 诊断完成后恢复后台运行。
& $client service start
```

`probe` 不联网；`doctor --delivery` 会发送报告，成功应核对真实 HTTP 202 和 Server 新报告。若安装失败，检查 `$env:TEMP\xsoc-install.log`，以及管理员可读的 `C:\ProgramData\xsoc.maintenance-diagnostic-0.11.0.txt`。

### 6. 升级、修复与卸载

本章后续维护若在新 PowerShell 会话中进行，先重新执行第 1 步读取注册表的 `$installRoot` / `$client` 两行。涉及修复或卸载时，再用 `$msi = (Resolve-Path .\xsoc-1.0.0-x64.msi).Path` 指向与已安装产品相匹配的 MSI；文件在其他目录时用其实际完整路径。不要沿用指向另一版本包的变量。

升级使用目标版本 MSI，校验后重复第 1 步安装；保留配置、身份、队列与启动意图。仅修复同版文件时：

```powershell
# 重新安装当前 MSI 的所有组件并记录修复日志；等待结束后检查 ExitCode。
$repair = Start-Process msiexec.exe -ArgumentList "/i `"$msi`" REINSTALL=ALL REINSTALLMODE=amus /norestart /l*v `"$env:TEMP\xsoc-repair.log`"" -Wait -PassThru
$repair.ExitCode
```

保留状态的普通卸载：

```powershell
# 移除程序、服务和安装器拥有的 PATH 项，保留配置、身份、凭据和待发队列。
$remove = Start-Process msiexec.exe -ArgumentList "/x `"$msi`" /norestart /l*v `"$env:TEMP\xsoc-uninstall.log`"" -Wait -PassThru
$remove.ExitCode
# 卸载成功后应找不到服务；“不存在”是本项验收的预期结果。
Get-Service -Name xsoc -ErrorAction SilentlyContinue
```

也可在“设置 → 应用 → 已安装的应用”卸载。若明确永久退役，先在 Server 处理实例并确认无需保留待发报告，再使用以下**替代普通卸载**的命令；`PURGE=1` 会永久清除本地状态，不能用于普通排障：

```powershell
# 永久卸载并清除安装器管理的本地状态；仅在确认数据可丢弃时执行。
$purge = Start-Process msiexec.exe -ArgumentList "/x `"$msi`" PURGE=1 /norestart /l*v `"$env:TEMP\xsoc-purge.log`"" -Wait -PassThru
$purge.ExitCode
```

## Linux x86_64（systemd）

### 1. 安装

Debian/Ubuntu 在下载目录执行：

```sh
# 确认本机为 x86_64，并计算 DEB 哈希；对比 SHA256SUMS-Linux-* 中对应文件行。
uname -m
sha256sum ./xsoc_1.0.0_amd64.deb
# 用 APT 安装本地包并解决依赖；./ 表示文件路径。
sudo apt install ./xsoc_1.0.0_amd64.deb
# 确认安装版本和服务登记。
xsoc version --format json
sudo xsoc service status --format json
```

RPM 系统使用以下安装命令，替代 APT 安装：

```sh
# 计算 RPM 哈希并与同版校验文件比较。
sha256sum ./xsoc-1.0.0.x86_64.rpm
# 使用 DNF 安装本地 RPM 并解决依赖。
sudo dnf install ./xsoc-1.0.0.x86_64.rpm
```

### 2. 首次配对和启动

```sh
# 停止后台采集，避免与设置过程争用配置和队列。
sudo xsoc service stop
# 完整设置：输入 HTTPS Server 根地址和授权码，确认开机策略，自动启动并验证连接。
sudo xsoc setup --interactive
# 查看本机绑定、服务状态及业务检查。
sudo xsoc pair status --format json
sudo xsoc service status --format json
sudo xsoc status --check --format json
# 核对 systemd 的开机自启结果：enabled 为已启用，disabled 为未启用。
systemctl is-enabled xsoc.service
```

自启提示按 Enter 默认为 Yes。最后在 Server 管理页确认相同 UUID 的主机收到新报告。

### 3. 重新配对

```sh
# 停服并确认现有身份。
sudo xsoc service stop
sudo xsoc pair status --format json
# 使用同一实例的新授权码恢复原 Host UUID；保留待发队列。
sudo xsoc pair recover --interactive
# 核对保存结果，恢复服务并检查业务状态。
sudo xsoc pair status --format json
sudo xsoc service start
sudo xsoc status --check --format json
```

响应丢失先使用 `sudo xsoc pair resume --interactive`；更换实例见下文队列处理。`important_state_incompatible` 时保全状态，不通过重装绕过检查。

### 4. 服务管理

下列是独立操作，按需选择：

```sh
# 查看 systemd 状态、最近日志及 CLI 结构化服务状态；q 退出分页器。
systemctl status xsoc.service
sudo xsoc service status --format json
# 分别为立即启动、停止、重启；不改变开机自启。
sudo xsoc service start
sudo xsoc service stop
sudo xsoc service restart
# 分别为启用/取消开机自启；不改变当前运行状态。
sudo xsoc service enable
sudo xsoc service disable
# 同时启用自启并立即启动；停用全部运行时用 disable --now。
sudo xsoc service enable --now
```

### 5. 诊断

```sh
# 查看脱敏配置与公开网络入口，不发送报告凭据。
sudo xsoc config show --format json
sudo xsoc doctor --network --format json
# 查看最近 100 条该服务日志；--no-pager 直接输出，不进入分页器。
sudo journalctl -u xsoc.service -n 100 --no-pager
# 实时跟踪日志；Ctrl+C 结束查看，不停止服务。
sudo journalctl -u xsoc.service -f
# 停服后检查本机采集、待发队列和一次真实投递。
sudo xsoc service stop
sudo xsoc probe --format json
sudo xsoc queue status --format json
sudo xsoc doctor --delivery --format json
# 完成诊断后重新启动。
sudo xsoc service start
```

### 6. 升级、修复、卸载

升级使用新版本 DEB/RPM，重复安装命令。以下重装和卸载命令按包管理器选择，不要全部执行：

```sh
# Debian/Ubuntu：修复当前版安装文件，保留业务状态。
sudo apt install --reinstall ./xsoc_1.0.0_amd64.deb
# RPM：替换当前版包文件进行修复，不绕过依赖检查。
sudo rpm -Uvh --replacepkgs ./xsoc-1.0.0.x86_64.rpm
# Debian/Ubuntu：普通卸载程序与服务，保留身份、配置和队列。
sudo apt remove xsoc
# RPM：普通卸载程序与服务，保留业务状态。
sudo dnf remove xsoc
# 卸载后刷新后的 unit 应不再 loaded；旧失败状态可能仍在 systemd 中留存。
systemctl show xsoc.service --property=LoadState,ActiveState
```

永久清除前先处理 Server 实例和待发队列。Debian 的 `sudo apt purge xsoc` 会永久清除本地配置、身份、凭据、队列和可安全确认的安装器账户；RPM 普通卸载不清理状态，可在卸载前调用 `sudo xsoc-purge --yes` 明确清除，再 `sudo dnf remove xsoc`。自定义状态目录须另行核对，不要假定默认清理覆盖它。不要把 `purge` 当作重新配对的前置步骤。

## macOS Apple Silicon

### 1. 安装

仅支持 arm64 Mac。PKG 未签名、未公证，按系统提供的批准入口允许已核验的安装包运行。

```sh
# 确认 Apple Silicon；输出应为 arm64。
uname -m
# 计算 PKG 哈希，对比 SHA256SUMS-Darwin-* 中对应文件行。
shasum -a 256 ./xsoc-1.0.0-macos-arm64-unsigned.pkg
# 安装到系统卷，创建专用账户并登记 LaunchDaemon。
sudo installer -pkg ./xsoc-1.0.0-macos-arm64-unsigned.pkg -target /
# 使用绝对路径确认版本与系统服务登记。
/usr/local/bin/xsoc version --format json
sudo /usr/local/bin/xsoc service status --format json
```

### 2. 配对、验收

```sh
# 停服后运行完整设置，输入 Server 和授权码，自启提示按 Enter 默认为 Yes。
sudo /usr/local/bin/xsoc service stop
sudo /usr/local/bin/xsoc setup --interactive
# 分别核对本机绑定、服务运行与业务检查结果。
sudo /usr/local/bin/xsoc pair status --format json
sudo /usr/local/bin/xsoc service status --format json
sudo /usr/local/bin/xsoc status --check --format json
```

最后在 Server 管理页确认新报告。PKG 自带 smartctl 7.5，无需为 SMART 采集先安装 Homebrew；专用服务账户不自动获得更高权限。

### 3. 重新配对

```sh
# 停止 LaunchDaemon，查看已有身份。
sudo /usr/local/bin/xsoc service stop
sudo /usr/local/bin/xsoc pair status --format json
# 用同一实例的新授权码恢复原 Host UUID 和报告队列。
sudo /usr/local/bin/xsoc pair recover --interactive
# 核对结果后恢复运行并验证。
sudo /usr/local/bin/xsoc pair status --format json
sudo /usr/local/bin/xsoc service start
sudo /usr/local/bin/xsoc status --check --format json
```

仅事务中断时，优先 `sudo /usr/local/bin/xsoc pair resume --interactive`。

### 4. 服务管理

按需单独执行：

```sh
# CLI 查看登记、运行和自启状态；launchctl 查看原生进程、退出码等信息。
sudo /usr/local/bin/xsoc service status --format json
sudo launchctl print system/org.sarmg.xsoc
# 立即启动、停止、重启；CLI 会处理 launchd 装载与卸载。
sudo /usr/local/bin/xsoc service start
sudo /usr/local/bin/xsoc service stop
sudo /usr/local/bin/xsoc service restart
# 设置或取消开机自启，不改变当前运行状态。
sudo /usr/local/bin/xsoc service enable
sudo /usr/local/bin/xsoc service disable
# 同时取消自启并停止当前服务。
sudo /usr/local/bin/xsoc service disable --now
```

`service stop` 后 `launchctl print` 可能提示找不到已卸载的 job；这时用 CLI 检查，`installed: true`、`loaded: false`、`state: stopped` 是可解释的停止状态。不要用 `kill` 停止受 launchd 管理的进程，以免被自动拉起。

### 5. 诊断

```sh
# 查看脱敏配置和公开网络检查。
sudo /usr/local/bin/xsoc config show --format json
sudo /usr/local/bin/xsoc doctor --network --format json
# 查看最近 100 条客户端日志；-f 实时跟踪，Ctrl+C 只结束查看。
sudo tail -n 100 /var/log/xsoc.log
sudo tail -f /var/log/xsoc.log
# 安装失败时查看系统安装日志。
sudo tail -n 100 /var/log/install.log
# 停服后执行单次真实投递验证，结束后恢复后台运行。
sudo /usr/local/bin/xsoc service stop
sudo /usr/local/bin/xsoc doctor --delivery --format json
sudo /usr/local/bin/xsoc service start
```

### 6. 升级、卸载

再次安装已校验的同版/新版 PKG 可修复或升级，保留业务状态；随后检查版本、服务策略和 Server 新报告。

```sh
# 普通卸载程序与 LaunchDaemon，保留配置、凭据、队列、日志、专用账户和维护助手。
sudo /usr/local/share/xsoc/uninstall.sh
# 卸载后应找不到 LaunchDaemon；该错误是本项验收的预期结果。
sudo launchctl print system/org.sarmg.xsoc
```

永久退役且不再需要本地数据时，在 Server 先撤销/退役实例，然后使用 `sudo /usr/local/share/xsoc/uninstall.sh --purge`；脚本会要求确认永久清理。它清除保留的状态、日志、可确认所有权的账户和包收据。普通卸载后仍保留此助手，允许后续决定清理。

## 更换绑定与队列处理（所有桌面平台）

`recover` 用于保留旧 Host UUID；`replace` 用于明确放弃旧绑定。先停服务，再 `queue status` 查看待发数量，旧 Server 可用时通过 `queue drain --timeout 10m` 尽量投递。旧 Server 永久丢失且无法投递时，先归档旧队列，不能直接删除：

```sh
# 以下为 Linux 示例；Windows 用 & $client，macOS 用 sudo /usr/local/bin/xsoc。
sudo xsoc service stop
# 查看当前绑定，取得 CURRENT_HOST_UUID 的实际值。
sudo xsoc pair status --format json
# 查看队列；drain 最多等待 10 分钟，会尝试真实投递。
sudo xsoc queue status --format json
sudo xsoc queue drain --timeout 10m --format json
# 仅旧 Server 永久丢失且不能 drain 时使用，原子归档供后续审查，不视为已送达。
sudo xsoc queue archive --reason server-state-lost --format json
# 显式确认替换，并核对旧绑定，防止替换错误的主机身份。
sudo xsoc pair replace --confirm-replace --expected-binding CURRENT_HOST_UUID --interactive
# 成功后重新启动并验收新实例。
sudo xsoc service start
sudo xsoc status --check --format json
```

上面的 `drain` 与 `archive` 是按结果选择的两条路径；排空成功后无需归档。替换失败时保留输出和原状态，先核对管理员提供的新实例授权码。

## 常见问题的处理顺序

| 现象 | 先做什么 | 完成判据 |
|---|---|---|
| 找不到命令 | Windows 重新开终端或取注册表路径；macOS 用绝对路径；Linux 检查包是否安装 | `version` 显示预期版本 |
| 服务运行但主机离线 | 先看日志和 `doctor --network`，再停服做 `doctor --delivery` | 真正投递被接受且管理页有新报告 |
| TLS/DNS/超时 | 核对 Server origin、DNS、机器时间、证书链与域名；网络恢复后复查 | 公开健康检查通过 |
| 授权码轮换/旧凭据拒绝 | 停服，取得同一实例新码，执行 `pair recover` | 保留 UUID，真实投递成功 |
| 配对请求响应丢失 | `pair status` 检查后 `pair resume`，或再次完整 `setup` | 原事务完成，避免创建重复身份 |
| `pairing_state_incompatible` | 保全资料，用新码 `pair recover`；账户文件可归档，Host UUID/队列保留 | 当前格式绑定被验证 |
| `important_state_incompatible` | 停服并保全队列、身份及日志，核实兼容性 | 重要状态可读且身份一致后再恢复 |
| 服务路径/权限不符 | 查系统服务登记和安装日志，用同版原生包修复 | 登记路径与当前程序、默认配置匹配 |
| 修改配置后未生效 | 停服，按配置指南 validate/diff/apply，再启动 | 新 revision 保存且业务验收通过 |

## 安装器、兼容性与后台日志补充

适用于 1.0.0，配置与持久身份格式为 1.0.0。每个平台下载对应的单个原生 Release 安装包，并对照同页 SHA256SUMS 校验。安装器检查平台、架构、权限、已安装版本状态并注册服务。安装完成后的初始化、配对和诊断命令见[完整配置指南](configuration.md)。

### Windows 11 x64

Windows 的只读队列检查使用与客户端服务相同的私有目录权限策略；不会因服务 SID 的合法授权而误判队列不安全，也不会修改目录 ACL。检查仍拒绝链接、未知文件名和超限的队列。

Windows 硬件清单将 PnP/WMI 设备标识统一为 ASCII 大写，先保留完整标识，再按协议的文本上限生成稳定 ID。显示名称仍按文本上限裁剪；显示器与声音设备的合并使用同一稳定 ID，避免长标识共享前缀导致误合并或重复上报。此调整不修改报告 schema 1 或本地配对身份。

从开始菜单以管理员身份打开 PowerShell，下载并校验 Release MSI。MSI 不在安装事务中启动配对；安装完成后由同一个管理员终端显式运行 `setup --interactive`：

```powershell
cd "$env:USERPROFILE\Downloads"
Get-FileHash .\xsoc-1.0.0-x64.msi -Algorithm SHA256
msiexec.exe /i .\xsoc-1.0.0-x64.msi /norestart
$installRoot = (Get-ItemProperty 'HKLM:\Software\xsos\xsoc').InstallLocation
$client = Join-Path $installRoot 'xsoc.exe'
& $client doctor --network
```

MSI 安装页允许选择任意本机安装目录，所有组件都会安装，不提供功能开关。安装与修复不会清空 `config.json`、身份、凭据或待发送采集队列，并会执行账户兼容性准备。该准备不读取授权码，也不发起网络配对：它只检查本机账户格式；发现未知或损坏的配对、授权、绑定或凭据文件时，先验证 Host UUID 和 telemetry spool，再把不兼容账户文件归档为唯一名称。Host UUID 和待发送队列不在归档范围内。向导始终显示完成页或失败页；安装后运行 `& $client setup --interactive` 完成配对。

将示例 Server 地址替换为你的 xsos，在交互提示中输入管理台创建的授权码。授权码使用普通文本提示并在终端中明文回显，不提供遮罩或隐藏切换。`setup` 的开机自启提示默认 yes，直接按 Enter 后由 Windows 服务管理器设置自动启动。MSI 会把用户选择的目录事务性追加到机器 PATH；新终端可直接运行 `xsoc`，修复与升级不重复添加，卸载会移除该安装器拥有的 PATH 项。配置位于 `C:\ProgramData\xsoc\config.json`；凭据与队列由安装器保护，服务使用 LocalService。

已有安装直接再次运行同一 MSI；原生安装器处理升级、修复、降级检查和服务登记，并保留配置、身份、队列及启动意图。安装器完成兼容性准备后，`xsoc setup` 会把仅有本地身份、缺少完整绑定以及“账户文件已归档但 Host UUID 仍在”的状态自动选择为 recovery，并在真实终端询问新的 Server 授权码。远程核验成功的绑定可复用，未完成事务可以继续。

手动强制修复同一 MSI：

```powershell
msiexec.exe /i "$PWD\xsoc-1.0.0-x64.msi" REINSTALL=ALL REINSTALLMODE=amus /l*v "$env:TEMP\xsoc-repair.log"
```

原生维护失败另写入 `C:\ProgramData\xsoc.maintenance-diagnostic-0.11.0.txt`（管理员读取）。退出码 3010 表示需要重启完成文件替换。修复不会接管指向其他程序的同名服务，也不会追踪重解析点。无人值守部署无需传入功能选择参数，所有组件与账户兼容性准备均会执行，现有状态会保留。

### Linux x86_64

Debian/Ubuntu 下载 DEB；使用 APT 处理依赖并覆盖旧包：

```sh
sudo apt install ./xsoc_1.0.0_amd64.deb
sudo xsoc setup
```

RPM 系统使用 `sudo dnf install ./xsoc-1.0.0.x86_64.rpm`；同版损坏重装可用 `sudo rpm -Uvh --replacepkgs ./xsoc-1.0.0.x86_64.rpm`。DEB 同版重装使用 `sudo apt install --reinstall ./xsoc_1.0.0_amd64.deb`。不要添加忽略依赖的参数。

配置在 `/etc/xsoc/config.json`，服务账户为 `xsoc`。安装器会保留已有配置和身份；安装后运行 `sudo xsoc setup`，开机自启提示默认 yes，直接按 Enter 即通过 systemd 启用 `xsoc.service`。配对完成后会自动启动服务并验证连接。可用 `systemctl is-enabled xsoc.service` 核对自启状态。兼容旧版账户所有权标记会被识别；包管理器保留修改过的配置，不会清除身份和待发送队列。

诊断：`systemctl status xsoc.service`、`sudo journalctl -u xsoc.service -n 100 --no-pager`。若 APT 上一次配置被打断，先重装上述包，再执行 `sudo dpkg --configure -a` 完成待配置包。

### macOS Apple Silicon

只提供 arm64；不支持 Intel Mac。在 Release 下载 unsigned PKG；安装包尚未签名、公证，可在系统允许的安装确认界面批准该已校验文件。

```sh
sudo installer -pkg ./xsoc-1.0.0-macos-arm64-unsigned.pkg -target /
sudo /usr/local/bin/xsoc setup
```

配置在 `/Library/Application Support/xsoc/config.json`，服务账户 `_xsoc`，系统 LaunchDaemon 为 `org.sarmg.xsoc`。重复上述 `installer` 命令可覆盖兼容旧版程序及修复当前版，不必删除状态或账户；随后运行 `sudo /usr/local/bin/xsoc setup`。安装日志在 `/var/log/install.log`；用 `sudo launchctl print system/org.sarmg.xsoc` 检查登记。

Mac 安装包随附独立 smartctl 7.5，无需先安装 Homebrew。对应许可证与源码位于 `/usr/local/share/xsoc/smartmontools`；服务继续使用 `_xsoc`，不自动提升权限。

本地构建 PKG 时先编译已固定校验值的 SMART 源码：

```sh
OUTPUT_DIRECTORY="$PWD/dist/smartmontools-macos-arm64" sh packaging/macos/build-smartmontools.sh
SMART_PAYLOAD="$PWD/dist/smartmontools-macos-arm64" BINARY="$PWD/target/aarch64-apple-darwin/release/xsoc" VERSION=1.0.0 sh packaging/macos/build-pkg.sh
```

离线构建可通过 `SOURCE_ARCHIVE` 指定原版 `smartmontools-7.5.tar.gz`，校验要求不变。

### 修改设置与常见配对问题

`setup` 按配置、配对、服务注册、启动策略、运行状态和连接顺序执行后置验证。交互安装仅询问是否开机自启，默认 yes；无需分别选择立即启动和连接验证，二者始终执行。交互终端明确区分 `local_binding_found`、`remotely_verified` 和 `stale_binding`，连接等待每五秒显示一次进度。只有 Server 接受凭据以及真实上报收到 HTTP 202 后才会显示相应 `verified`。JSON 失败响应中的 `error.step` 指明失败关卡，`error.code` 和脱敏 `error.detail` 给出稳定原因。连接检查使用严格外层超时；超时或 Ctrl+C 会返回非成功结果，但不会回滚已经提交的配对。

已有有效配置无须重复初始化或配对。先 `config show --format json` 查看脱敏设置和修订；修改候选文件后使用 `config validate --file <绝对路径>`、`config diff --file <绝对路径>`、`config apply --file <绝对路径> --expected-revision <当前修订>`，写操作前停止服务。

Server 必须使用系统信任的 HTTPS 证书；证书过期、名称不匹配或企业 CA 未装入系统信任库时，先修复证书。
每个实例的授权码长期有效，只有管理员显式轮换授权码或取消实例时才失效；轮换后旧 Client credential 会被
撤销，必须使用新授权码重新运行配对。一次配对请求自身有短期事务超时，这不等于实例授权码过期；网络中断后
再次运行 `setup` 会核对并恢复仍有效的同一事务，也可用 `pair status`、`pair resume` 精确检查。授权码轮换或 Server 数据库重建导致旧凭据失效时，`setup` 会自动选择 `pair recover` 并提示输入新授权码；该流程保留原 Host UUID 和待发送队列。Server 允许恢复不存在的 UUID，或恢复属于同一实例、已撤销且没有有效凭据的 UUID。也可显式运行 `pair recover --interactive`。若管理员明确放弃旧身份，先运行 `queue archive --reason server-state-lost` 原子归档旧队列，再使用 `pair replace`，不得删除队列或把旧报告改属新 UUID。`status`
显示未配对时不应反复安装或删除身份文件。

当前运行只接受配置与账户格式 1.0.0。旧格式配置保持原样并拒绝启动；管理员须保全旧配置并通过当前配置流程重建和核验，不应直接改格式字段。其它格式或损坏的账户资料会返回 `pairing_state_incompatible`；创建新授权码后运行 `pair recover --interactive`，Client 会先验证采集队列，再归档旧账户文件并保留 Host UUID。若队列不可读、含隔离记录或身份不匹配，则返回 `important_state_incompatible` 并保持所有数据原样，不能通过重装或重新配对绕过。

强制覆盖仅替换安装器管理的程序和服务文件，不绕过状态格式、路径所有权或配置校验；不需要用 `PURGE=1` 解决普通安装问题。

### Windows 后台诊断

SCM 在私有状态校验后创建 `配置 state_dir 下的 logs`，使用共享 typed sink 写入 `xsoc.jsonl`。最多保留活动文件和四份归档，每份 8 MiB，总上限 40 MiB。服务账户首次创建此目录；管理员查询不会先替服务建立日志目录。ACL 拒绝普通用户，已有不安全对象不修复。服务启动失败且日志 sink 尚不可用时，可同时查看 Windows SCM 的服务退出代码。

```powershell
xsoc logs --tail 100 --format json
xsoc logs --since 2026-10-07T00:00:00Z --level warn --format json
xsoc logs --follow --format ndjson --timeout 60s
```

可用 `--instance-id`、`--event`、`--request-id` 和 `--task-id` 精确筛选。后台整体启动事件具有服务 scope，只有确实属于某个已知实例的事件才带 instance_id。日志源损坏、超限或 follow 游标已从保留窗口移除时明确失败，避免把丢失记录显示为空成功。
