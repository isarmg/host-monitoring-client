# xsoc 配置指南

首次部署或日常维护请先阅读[分平台全流程指南](platform-setup.md)：按本机平台完成安装、配对、重新配对、服务/后台任务查看与启停、诊断和卸载，命令旁均说明用途。本文详细说明配置字段和业务操作。

本文适用于 `xsoc` `1.0.0`。以下命令覆盖初始化、修改、校验、配对、启动和诊断；平台安装命令见[平台安装指南](platform-setup.md)。

## 命令用途与执行边界

| 命令 | 用途与影响 |
|---|---|
| `version --format json` | 只读确认软件、平台、配置/账户格式与协议身份 |
| `config init [--interactive]` | 首次创建配置；交互模式输入 Server，不用于重置已有身份 |
| `config show --format json` | 只读输出脱敏配置与 `stored_revision`，供查看和并发写入校验 |
| `config edit` | 调用 VISUAL/EDITOR 编辑候选配置，保存前校验并按修订提交；先停服 |
| `config validate --file PATH` | 校验候选文件内容，不应用候选配置 |
| `config diff --file PATH` | 比较候选与当前配置，供提交前审阅 |
| `config apply --file PATH --expected-revision REVISION` | 原子提交通过校验的候选，修订不匹配则拒绝；先停服 |
| `setup` | 串联配置、配对/恢复、服务策略、启动和真实连接验证 |
| `pair --interactive` / `pair --input-stdin` | 单独配对，不替代完整服务部署；秘密由终端或受保护 stdin 提供 |
| `pair status` / `pair resume` | 分别查看本地事务、恢复响应丢失的既有事务 |
| `pair recover` | 用当前授权恢复原 Host UUID，保留待发队列 |
| `pair replace --confirm-replace --expected-binding UUID` | 明确放弃旧绑定，先排空或归档原队列，并核对预期旧身份 |
| `queue status` / `queue inspect` | 只读检查队列容量/内容摘要，不表示报告已送达 |
| `queue drain --timeout 10m` | 尝试真实投递已有队列，最多等待指定时间 |
| `queue archive --reason server-state-lost` | 原子归档无法向已丢失 Server 投递的旧队列，保留审查资料 |
| `probe` / `once` | 分别只采集不联网、单次采集并尝试投递；真实投递前先停后台服务 |
| `doctor --network` / `doctor --delivery` | 分别检查公开健康端点、用当前凭据做真实投递 |
| `logs --tail 100` | 读取最近日志，不改变服务运行状态 |

`--file`/`--config` 使用实际绝对路径，`REVISION` 来自最新 `config show`，不能照抄占位符。`--input-stdin` 从标准输入读取严格 JSON；`--non-interactive` 禁止额外交互；`--timeout` 限定操作等待；`--format json` 改变输出格式，不改变操作是否写入/联网。详细服务启停语义见分平台指南。


## 1. 路径与准备

默认配置路径：

| 平台 | 配置文件 |
|---|---|
| Windows | `C:\ProgramData\xsoc\config.json` |
| Linux | `/etc/xsoc/config.json` |
| macOS | `/Library/Application Support/xsoc/config.json` |

先确认安装结果并停止服务，配置写入期间不要让后台进程同时运行：

```sh
xsoc version --format json
sudo xsoc service status --format json
sudo xsoc service stop
```

Windows 请在管理员 PowerShell 中去掉 `sudo`；若 PATH 尚未刷新，使用：

```powershell
$InstallRoot = (Get-ItemProperty 'HKLM:\Software\sarmg\xsoc').InstallLocation
$Client = Join-Path $InstallRoot 'xsoc.exe'
& $Client version --format json
& $Client service stop
```

## 2. 初始化和查看配置

首次安装可用默认值初始化，也可交互输入 Server origin：

```sh
sudo xsoc config init
# 或
sudo xsoc config init --interactive
sudo xsoc config show --format json
```

`config show` 会返回 `stored_revision`，并对秘密字段脱敏。请记录这个 revision；它用于防止两个管理员互相覆盖配置。已存在配置时不要再次运行 `config init`。

当前配置的主要字段如下：

```json
{
  "application_version": "1.0.0",
  "endpoint": "https://monitor.example.com/api/v1/xsoc/report",
  "pairing_endpoint": null,
  "otlp_endpoint": null,
  "otlp_token": null,
  "interval_seconds": 10,
  "slow_interval_seconds": 30,
  "request_timeout_seconds": 10,
  "jitter_percent": 10,
  "state_dir": "/var/lib/xsoc",
  "spool_max_bytes": 67108864,
  "tls_identity_pem": null,
  "tls_identity_pkcs12": null,
  "tls_identity_password": null,
  "tls_ca_pem": null
}
```

通常只需修改 Server endpoint、采集间隔、队列上限和可选 OTLP 设置。`state_dir` 不能通过普通配置提交迁移；已经配对后，更换 Server endpoint 必须使用 `pair replace`。

OTLP 的网络和磁盘字节计数保留操作系统累计值，使用 monotonic cumulative Sum。报告没有提供每个设备计数器的准确起点，因此 `StartTimeUnixNano` 为 0（未知），遵循 [OTLP 对未知起点的定义](https://github.com/open-telemetry/opentelemetry-proto/blob/main/opentelemetry/proto/metrics/v1/metrics.proto)。不会用整数 uptime 推算一个随采样抖动变化的启动时间；补传也保留原报告的采集时刻。下游应支持未知起点，并根据累计值下降识别重启或设备计数器重置；若重置后计数已超过前值，当前报告协议无法判断这次重置。

常驻 `run` 启动后立即采集第一份报告。首报的 `interval_seconds` 向 Server 声明配置的下次采样周期，供在线状态估算；网络和磁盘速率仍按从采样器初始化到首报的实测时间计算。后续报告的 `interval_seconds` 使用实测采样周期，并限制在协议范围内。休眠或进程暂停使实测周期超过协议上限时，网络和磁盘速率仍使用完整实测时间，不因报告周期被截断而虚高。

## 3. 修改、校验和提交

最简单的方式是使用受保护编辑器。程序会校验内容并以当前 revision 原子提交：

```sh
sudo env EDITOR="${EDITOR:-vi}" xsoc config edit
sudo xsoc config show --format json
```

自动化或变更评审使用候选文件。Linux 示例：

```sh
sudo install -m 0600 /etc/xsoc/config.json /root/xsoc.candidate.json
sudoedit /root/xsoc.candidate.json
sudo xsoc config validate --file /root/xsoc.candidate.json --format json
sudo xsoc config diff --file /root/xsoc.candidate.json --format json
sudo xsoc config apply \
  --file /root/xsoc.candidate.json \
  --expected-revision COPY_STORED_REVISION_HERE \
  --format json
```

Windows 管理员 PowerShell 示例：

```powershell
$Config = "$env:ProgramData\xsoc\config.json"
$Candidate = "$env:TEMP\xsoc.candidate.json"
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
sudo xsoc pair --interactive
sudo xsoc pair status --format json
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
sudo install -m 0600 /dev/null /root/xsoc-bootstrap.json
sudoedit /root/xsoc-bootstrap.json
sudo sh -c 'exec xsoc pair --input-stdin --non-interactive --format json < /root/xsoc-bootstrap.json'
sudo xsoc pair status --format json
sudo shred -u /root/xsoc-bootstrap.json
```

若存储介质不保证 `shred` 有效，请在确认配对成功后直接删除该临时文件，并按介质的安全擦除策略处理。不要把授权码作为参数或环境变量。

网络中断或响应丢失后先恢复原事务：

```sh
sudo xsoc pair status --format json
sudo xsoc pair resume --interactive
```

Server 数据丢失但要保留原 Host UUID 和待发队列时，由管理员先为同一 UUID 准备新授权码，再执行：

```sh
sudo xsoc pair recover --interactive
```

同一命令也用于 `pairing_state_incompatible`：Client 会先只读检查 spool，再把不兼容的 `pairing-state.json`、`auth-state.json`、`active-binding.json` 和 `client-token` 归档为唯一名称。`host-id` 与 `spool/` 永远不在账户归档列表中。若返回 `important_state_incompatible`，应停止恢复并保全 spool，使用兼容 Client 恢复或归档供人工审查，不要删除文件后重试。

只有明确放弃旧绑定时才更换实例。先检查并尽量排空队列：

```sh
sudo xsoc queue status --format json
sudo xsoc queue drain --timeout 10m --format json
sudo xsoc pair replace \
  --confirm-replace \
  --expected-binding CURRENT_HOST_UUID \
  --interactive
```

若旧 Server 已永久丢失、队列无法投递，先归档而不是删除队列：

```sh
sudo xsoc queue archive --reason server-state-lost --format json
```

## 5. 启动和验证

```sh
sudo xsoc service enable
sudo xsoc service start
sudo xsoc service status --format json
xsoc status --check --format json
xsoc doctor --network --format json
sudo xsoc doctor --delivery --format json
```

`status --format json` 的 `checks.report_auth.status` 表示本机上报凭据的读取结果，`credential_present` 表示是否找到可用凭据；不会显示凭据内容。`doctor --format json` 在 `checks` 数组中以 `id=credential` 返回同一检查。

`doctor --network` 只验证当前 Server 的公开存活接口 `GET /healthz`（成功为 HTTP 204），不发送上报凭据；`doctor --delivery` 会使用当前凭据产生真实投递。需要区分采集与投递问题时：

```sh
xsoc probe --format json
sudo xsoc once --format json
sudo xsoc logs --tail 100
```

最后在 Server 管理页确认实例在线且收到新报告。仅有服务进程运行不代表业务已成功。

## 6. 安全注意事项

- Server 必须使用系统信任且名称匹配的 HTTPS 证书。
- 配置、身份、队列和临时 Bootstrap 文件只允许管理员或服务账号读取。
- `config show` 可用于工单；原始配置、授权码、Client token 和 OTLP token 不得进入工单或日志。
- 修改配置、配对或凭据前停止服务；完成验证后再恢复服务。

## 运行日志

常驻 Client 使用 xcsc 内部 `xcsc::log` 实现输出 UTC JSON 行到 stderr；由 systemd、launchd 或 Windows 服务宿主管理收集。Host 投递、授权和队列事件带 canonical Host `instance_id`，报告使用 `request_id` 关联，失败使用稳定 `error_code`。普通日志不输出任意 error chain、配置或授权 URL。日志字段/输出预算或写入失败会被记录并在下一轮采样检查时以 `LOGGING_UNAVAILABLE` 退出；服务管理器可据此定位日志目标故障。
