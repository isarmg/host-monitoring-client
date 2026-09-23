# Host Monitoring Client 配置指南

本文适用于 `host-monitor` `0.9.36`。以下命令覆盖初始化、修改、校验、配对、启动和诊断；平台安装命令见[平台安装指南](platform-setup.md)。

## 1. 路径与准备

默认配置路径：

| 平台 | 配置文件 |
|---|---|
| Windows | `C:\ProgramData\host-monitor\config.json` |
| Linux | `/etc/host-monitor/config.json` |
| macOS | `/Library/Application Support/host-monitor/config.json` |

先确认安装结果并停止服务，配置写入期间不要让后台进程同时运行：

```sh
host-monitor version --format json
sudo host-monitor service status --format json
sudo host-monitor service stop
```

Windows 请在管理员 PowerShell 中去掉 `sudo`；若 PATH 尚未刷新，使用：

```powershell
$InstallRoot = (Get-ItemProperty 'HKLM:\Software\Host Monitoring\host-monitor').InstallLocation
$Client = Join-Path $InstallRoot 'host-monitor.exe'
& $Client version --format json
& $Client service stop
```

## 2. 初始化和查看配置

首次安装可用默认值初始化，也可交互输入 Server origin：

```sh
sudo host-monitor config init
# 或
sudo host-monitor config init --interactive
sudo host-monitor config show --format json
```

`config show` 会返回 `stored_revision`，并对秘密字段脱敏。请记录这个 revision；它用于防止两个管理员互相覆盖配置。已存在配置时不要再次运行 `config init`。

当前配置的主要字段如下：

```json
{
  "application_version": "0.9.4",
  "endpoint": "https://monitor.example.com/api/v2/host-monitor/report",
  "pairing_endpoint": null,
  "otlp_endpoint": null,
  "otlp_token": null,
  "interval_seconds": 10,
  "slow_interval_seconds": 30,
  "request_timeout_seconds": 10,
  "jitter_percent": 10,
  "state_dir": "/var/lib/host-monitor",
  "spool_max_bytes": 67108864,
  "tls_identity_pem": null,
  "tls_identity_pkcs12": null,
  "tls_identity_password": null,
  "tls_ca_pem": null
}
```

通常只需修改 Server endpoint、采集间隔、队列上限和可选 OTLP 设置。`state_dir` 不能通过普通配置提交迁移；已经配对后，更换 Server endpoint 必须使用 `pair replace`。

常驻 `run` 启动后立即采集第一份报告。首报的 `interval_seconds` 向 Server 声明配置的下次采样周期，供在线状态估算；网络和磁盘速率仍按从采样器初始化到首报的实测时间计算。后续报告的 `interval_seconds` 使用实测采样周期，并限制在协议范围内。

## 3. 修改、校验和提交

最简单的方式是使用受保护编辑器。程序会校验内容并以当前 revision 原子提交：

```sh
sudo env EDITOR="${EDITOR:-vi}" host-monitor config edit
sudo host-monitor config show --format json
```

自动化或变更评审使用候选文件。Linux 示例：

```sh
sudo install -m 0600 /etc/host-monitor/config.json /root/host-monitor.candidate.json
sudoedit /root/host-monitor.candidate.json
sudo host-monitor config validate --file /root/host-monitor.candidate.json --format json
sudo host-monitor config diff --file /root/host-monitor.candidate.json --format json
sudo host-monitor config apply \
  --file /root/host-monitor.candidate.json \
  --expected-revision COPY_STORED_REVISION_HERE \
  --format json
```

Windows 管理员 PowerShell 示例：

```powershell
$Config = "$env:ProgramData\host-monitor\config.json"
$Candidate = "$env:TEMP\host-monitor.candidate.json"
Copy-Item $Config $Candidate
notepad.exe $Candidate
& $Client config validate --file $Candidate --format json
& $Client config diff --file $Candidate --format json
& $Client config apply --file $Candidate --expected-revision COPY_STORED_REVISION_HERE --format json
```

提交前再次执行 `config show`；revision 已改变时，重新生成候选文件，不要绕过冲突检查。不要用脱敏后的 `config show` 输出覆盖真实配置，否则秘密字段会丢失。

## 4. 首次配对

在 Server 管理页创建实例并复制授权码。有人值守时直接运行：

```sh
sudo host-monitor pair --interactive
sudo host-monitor pair status --format json
```

所有交互配对入口（首次配对、替换和恢复）使用同一个 `Authorization code (visible)` 普通文本提示。输入或
粘贴的授权码会在终端中明文回显，不提供遮罩、隐藏切换或特殊显示流程；CLI 仍不会把授权码写入日志、结果
JSON 或命令参数。

自动化必须通过 stdin 提交严格 JSON：

```json
{
  "server": "https://monitor.example.com",
  "authorization_code": "REPLACE_WITH_INSTANCE_AUTHORIZATION_CODE"
}
```

Linux 上创建、编辑并使用受保护文件：

```sh
sudo install -m 0600 /dev/null /root/host-monitor-bootstrap.json
sudoedit /root/host-monitor-bootstrap.json
sudo sh -c 'exec host-monitor pair --input-stdin --non-interactive --format json < /root/host-monitor-bootstrap.json'
sudo host-monitor pair status --format json
sudo shred -u /root/host-monitor-bootstrap.json
```

若存储介质不保证 `shred` 有效，请在确认配对成功后直接删除该临时文件，并按介质的安全擦除策略处理。不要把授权码作为参数或环境变量。

网络中断或响应丢失后先恢复原事务：

```sh
sudo host-monitor pair status --format json
sudo host-monitor pair resume --interactive
```

Server 数据丢失但要保留原 Host UUID 和待发队列时，由管理员先为同一 UUID 准备新授权码，再执行：

```sh
sudo host-monitor pair recover --interactive
```

同一命令也用于 `pairing_state_incompatible`：Client 会先只读检查 spool，再把不兼容的 `pairing-state.json`、`auth-state.json`、`active-binding.json` 和 `client-token` 归档为唯一名称。`host-id` 与 `spool/` 永远不在账户归档列表中。若返回 `important_state_incompatible`，应停止恢复并保全 spool，使用兼容 Client 恢复或归档供人工审查，不要删除文件后重试。

只有明确放弃旧绑定时才更换实例。先检查并尽量排空队列：

```sh
sudo host-monitor queue status --format json
sudo host-monitor queue drain --timeout 10m --format json
sudo host-monitor pair replace \
  --confirm-replace \
  --expected-binding CURRENT_HOST_UUID \
  --interactive
```

若旧 Server 已永久丢失、队列无法投递，先归档而不是删除队列：

```sh
sudo host-monitor queue archive --reason server-state-lost --format json
```

## 5. 启动和验证

```sh
sudo host-monitor service enable
sudo host-monitor service start
sudo host-monitor service status --format json
host-monitor status --check --format json
host-monitor doctor --network --format json
sudo host-monitor doctor --delivery --format json
```

`status --format json` 的 `checks.report_auth.status` 表示本机上报凭据的读取结果，`credential_present` 表示是否找到可用凭据；不会显示凭据内容。`doctor --format json` 在 `checks` 数组中以 `id=credential` 返回同一检查。

`doctor --network` 只验证公开网络入口；`doctor --delivery` 会使用当前凭据产生真实投递。需要区分采集与投递问题时：

```sh
host-monitor probe --format json
sudo host-monitor once --format json
sudo host-monitor logs --tail 100
```

最后在 Server 管理页确认实例在线且收到新报告。仅有服务进程运行不代表业务已成功。

## 6. 安全注意事项

- Server 必须使用系统信任且名称匹配的 HTTPS 证书。
- 配置、身份、队列和临时 Bootstrap 文件只允许管理员或服务账号读取。
- `config show` 可用于工单；原始配置、授权码、Client token 和 OTLP token 不得进入工单或日志。
- 修改配置、配对或凭据前停止服务；完成验证后再恢复服务。
