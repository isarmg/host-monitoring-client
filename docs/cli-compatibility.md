# xsoc 命令参考

使用 `xsoc --help` 查看当前可用命令，`xsoc version --format json` 查看软件与协议身份。Linux 维护命令使用 sudo，Windows 使用管理员 PowerShell，macOS 使用 `sudo /usr/local/bin/xsoc`。

## 按目的选择命令



| 命令 | 用途与影响 |
|---|---|
| `version --format json` | 只读确认软件、平台、配置/账户格式与协议身份 |
| `config init [--interactive]` | 首次创建配置；交互模式输入服务端，不用于重置已有身份 |
| `config show --format json` | 只读输出脱敏配置与 `stored_revision`，供查看和并发写入校验 |
| `config edit` | 调用 VISUAL/EDITOR 编辑候选配置，保存前校验并按修订提交；先停服 |
| `config validate --file PATH` | 校验候选文件内容，不应用候选配置 |
| `config diff --file PATH` | 比较候选与当前配置，供提交前审阅 |
| `config apply --file PATH --expected-revision REVISION` | 原子提交通过校验的候选，修订不匹配则拒绝；先停服 |
| `setup` | 串联配置、配对/恢复、服务策略、启动和真实连接验证 |
| `pair --interactive` / `pair --input-stdin` | 单独配对，不替代完整服务部署；秘密由终端或受保护 stdin 提供 |
| `pair status` / `pair resume` | 分别查看本地事务、恢复响应丢失的既有事务 |
| `pair recover` | 用当前授权恢复原主机 UUID，保留待发队列 |
| `pair replace --confirm-replace --expected-binding UUID` | 明确放弃旧绑定，先排空或归档原队列，并核对预期旧身份 |
| `queue status` / `queue inspect` | 只读检查队列容量/内容摘要，不表示报告已送达 |
| `queue drain --timeout 10m` | 尝试真实投递已有队列，最多等待指定时间 |
| `queue archive --reason server-state-lost` | 原子归档无法向已丢失服务端投递的旧队列，保留审查资料 |
| `probe` / `once` | 分别只采集不联网、单次采集并尝试投递；真实投递前先停后台服务 |
| `doctor --network` / `doctor --delivery` | 分别检查公开健康端点、用当前凭据做真实投递 |
| `logs --tail 100` | 读取最近日志，不改变服务运行状态 |

`--file`/`--config` 使用实际绝对路径，`REVISION` 来自最新 `config show`，不能照抄占位符。`--input-stdin` 从标准输入读取严格 JSON；`--non-interactive` 禁止额外交互；`--timeout` 限定操作等待；`--format json` 改变输出格式，不改变操作是否写入/联网。详细服务启停语义见分平台指南。



## 服务操作

| 命令 | 效果 |
|---|---|
| `service status --format json` | 查看服务登记、运行状态与自启策略 |
| `service start` / `service stop` / `service restart` | 立即启停，不改变自启策略 |
| `service enable` / `service disable` | 设置开机策略，不改变当前进程 |
| `service disable --now` | 取消自启并停止服务 |

`status --check` 同时通过退出码表达检查结果，`--format json` 提供结构化内容；自动化应检查退出码和稳定错误 code。日志字段与平台路径见[维护指南](administration.md)。
