# 配置 xsoc

首次部署直接使用[安装指南](platform-setup.md)的 `setup --interactive`；下面用于按步骤配置、审阅修改或自动化。修改配置、配对和凭据前先停止服务，验证后恢复运行。

命令总表见[命令参考](cli-compatibility.md)。所有 `REPLACE_...`、`COPY_STORED_REVISION_HERE` 和 `CURRENT_HOST_UUID` 都要替换为实际值。秘密输入放在受保护文件或交互提示中。

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

首次安装可用默认值初始化，也可交互输入服务端源站：

```sh
sudo xsoc config init --interactive
sudo xsoc config show --format json
```

`config show` 会返回 `stored_revision`，并对秘密字段脱敏。请记录这个提交修订；它用于防止两个管理员互相覆盖配置。已存在配置时不要再次运行 `config init`。

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

常用字段的范围：

| 字段 | 默认值 | 范围与作用 |
|---|---|---|
| `interval_seconds` | 10 | 大于 0，乘以最大 jitter 系数后不超过 3600 秒 |
| `slow_interval_seconds` | 30 | 至少等于基础采集周期 |
| `jitter_percent` | 10 | 0–50，分散采样请求 |
| `request_timeout_seconds` | 10 | 1–300 秒 |
| `spool_max_bytes` | 67108864 | 1–256 MiB，限制本地待发报告 |
| `smart.enabled` | true | 启用只读磁盘健康采集 |
| `smart.interval_seconds` | 300 | 60–86400 秒 |
| `smart.executable` | null | 可选 smartctl 绝对路径，默认按平台发现 |

SMART 设备要求与 TLS/OTLP 数据含义见[硬件监控](hardware-monitoring.md)和下文。

通常只需修改服务端地址、采集间隔和队列上限。OTLP 需要启用 otlp 特性的构建，标准默认构建未启用，见[开发指南](development.md)。`state_dir` 不能通过普通配置提交迁移；已经配对后，更换服务端地址必须使用 `pair replace`。

OTLP 的网络和磁盘字节计数保留操作系统累计值，使用 monotonic cumulative Sum。报告没有提供每个设备计数器的准确起点，因此 `StartTimeUnixNano` 为 0（未知），遵循 [OTLP 对未知起点的定义](https://github.com/open-telemetry/opentelemetry-proto/blob/main/opentelemetry/proto/metrics/v1/metrics.proto)。不会用整数 uptime 推算一个随采样抖动变化的启动时间；补传也保留原报告的采集时刻。下游应支持未知起点，并根据累计值下降识别重启或设备计数器重置；若重置后计数已超过前值，当前报告协议无法判断这次重置。

OTLP HTTP 200 的 Protobuf 响应会检查 `partial_success`：拒收数据点时记录 `xsoc.otlp.partial_success` 和数量；只含警告时记录 `xsoc.otlp.collector_warning`。具体原因请查询 Collector 日志，客户端不回显服务端自由文本。依照 [OTLP 部分成功规则](https://opentelemetry.io/docs/specs/otlp/#partial-success-1)，部分成功不重试，以免重复已接收的数据点；可选 OTLP 导出不改变主服务端上报的确认状态。

常驻 `run` 启动后立即采集第一份报告。首报的 `interval_seconds` 向服务端声明配置的下次采样周期，供在线状态估算；网络和磁盘速率仍按从采样器初始化到首报的实测时间计算。后续报告的 `interval_seconds` 使用实测采样周期，并限制在协议范围内。休眠或进程暂停使实测周期超过协议上限时，网络和磁盘速率仍使用完整实测时间，不因报告周期被截断而虚高。

## 3. 修改、校验和提交

最简单的方式是使用受保护编辑器。程序会校验内容并以当前提交修订原子提交：

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

Windows 使用受保护的编辑流程，避免另建含凭据的普通临时副本：

```powershell
$env:EDITOR = 'notepad.exe'
& $Client config edit
& $Client config show --format json
```

提交前再次执行 `config show`；提交修订已改变时，重新生成候选文件，不要绕过冲突检查。不要用脱敏后的 `config show` 输出覆盖真实配置，否则秘密字段会丢失。

## 4. 首次配对

在服务端管理页创建实例并复制授权码。有人值守时直接运行：

```sh
sudo xsoc pair --interactive
sudo xsoc pair status --format json
```

授权码在交互终端明文回显；请在受保护终端输入，自动化使用下述受保护 stdin。

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
sudo rm -f /root/xsoc-bootstrap.json
```

配对完成后删除临时输入文件；文件系统或存储介质可能保留其副本，按所在系统的秘密存储策略处理。

网络中断或响应丢失后先恢复原事务：

```sh
sudo xsoc pair status --format json
sudo xsoc pair resume
```

服务端数据丢失但要保留原主机 UUID 和待发队列时，由管理员先为同一 UUID 准备新授权码，再执行：

```sh
sudo xsoc pair recover --interactive
```

只有明确放弃旧绑定时才更换实例。先检查并尽量排空队列：

```sh
sudo xsoc queue status --format json
sudo xsoc queue drain --timeout 10m --format json
sudo xsoc pair replace \
  --confirm-replace \
  --expected-binding CURRENT_HOST_UUID \
  --interactive
```

若旧服务端已永久丢失、队列无法投递，先归档而不是删除队列：

```sh
sudo xsoc queue archive --reason server-state-lost --format json
```

## 5. 启动和验证

```sh
sudo xsoc service enable
sudo xsoc service start
sudo xsoc service status --format json
sudo xsoc status --check --format json
sudo xsoc doctor --network --format json
```

`status --format json` 的 `checks.report_auth.status` 表示本机上报凭据的读取结果，`credential_present` 表示是否找到可用凭据；不会显示凭据内容。`doctor --format json` 在 `checks` 数组中以 `id=credential` 返回同一检查。

`doctor --network` 只验证当前服务端的公开存活接口 `GET /healthz`（成功为 HTTP 204），不发送上报凭据；`doctor --delivery` 会使用当前凭据产生真实投递。需要区分采集与投递问题时：

```sh
sudo xsoc service stop
sudo xsoc probe --format json
sudo xsoc once --format json
sudo xsoc service start
sudo xsoc logs --tail 100
```

最后在服务端管理页确认实例在线且收到新报告。仅有服务进程运行不代表业务已成功。
