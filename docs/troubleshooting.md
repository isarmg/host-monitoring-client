# 排查 xsoc 问题

安装器、命令路径和系统日志问题按平台查看：[Linux](platforms/linux.md#排查本机问题)、[Windows](platforms/windows.md#排查本机问题)、[macOS](platforms/macos.md#排查本机问题)。

以下排查各桌面平台共用。先运行 `version --format json`、`service status --format json` 和 `logs --tail 100`。按下表继续；维护命令使用管理员权限。

| 现象 | 检查 | 下一步与完成结果 |
|---|---|---|
| 服务运行但连接未确认 | `status --format json` 和日志中的错误 code | 分别定位网络、认证和业务检查 |
| DNS、TLS 或超时 | `doctor --network --format json` | 核对根地址、DNS、主机时间、系统信任链和证书名称 |
| 配对响应丢失 | `pair status --format json` | `pair resume` 继续同一事务 |
| 授权码轮换后认证失败 | 确认同一实例的新授权码 | 按[维护指南](administration.md)重新绑定，再确认业务连接 |
| 配置修改未生效 | `config show --format json` | 停服后 validate/diff/apply，再启动并检查新修订 |
| `important_state_incompatible` | 查看错误指向的队列、权限与日志 | 停服保留数据，核对安装版本与状态完整性；记录可读后再恢复 |
| 队列持续增长 | `queue status --format json` 与 Server 就绪状态 | 修复网络或服务端写入，确认真实投递后数量下降 |
| 采集成功但报告失败 | 停服务后 `doctor --delivery --format json` | 该命令实际投递；确认 HTTP 202，结束后启动服务 |
| 缺少硬件读数 | `probe --format json` 中能力诊断 | 按[硬件文档](hardware-monitoring.md)检查驱动、smartctl、设备权限 |

`doctor --network` 只检查远端公开入口，报告凭据需通过真实投递验证。远端证书应由实际服务账户的系统信任库信任。

`setup` 超时或 Ctrl+C 可能发生在身份已经保存之后。先读配对和日志结果，再继续原事务，避免丢弃已有状态。提交问题时附软件版本、平台、脱敏 code 和所做检查，保留凭据及原始业务数据在本机。
