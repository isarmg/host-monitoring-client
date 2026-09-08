# 纯命令行改造（未发布）

公开入口为产品 CLI，长期运行由 SCM、systemd 或 launchd 管理。删除托盘、网页控制服务和登录自启入口；服务端集中管理网页保留。没有版本号或持久状态格式的暗中迁移。

管理员先停 Client 服务，再进行配对、配置提交或凭据更新。外层维护锁先于运行实例/状态事务锁获取，冲突有界失败；查询不创建锁和缺失状态。现有身份、凭据、Host 队列和 Sunshine 执行记录继续使用原格式。

## 公共用法

- `version` / `--version`；机器输出增加 `--format json`。
- `config init`，`config show`，`config validate --file /absolute/candidate.json`。
- `config diff --file /absolute/candidate.json`。
- `config apply --file /absolute/candidate.json --expected-revision REVISION`。
- `pair --interactive` 或 `pair --input-stdin --non-interactive --format json`。
- `pair status`，`pair resume`。恢复必须找到已有事务。
- `status`，`status --watch --format ndjson --timeout 5m`，`status --check`。
- `doctor` 只读；`doctor --network` 显式访问公共健康端点，报告当前 CLI 账户的信任环境。
- `service status|start|stop|restart|enable|disable`；`enable/disable --now` 同时操作当前运行状态。
- Linux：`logs --tail 100`、`logs --since '2026-09-07'`、`logs --follow --format ndjson`。

`--output` 是 `--format` 的兼容别名。默认不输出终端控制序列。输入 JSON 最多 64 KiB，拒绝未知字段；禁止把授权码、密码或长期凭据作为参数。交互秘密输入不回显。JSON 有固定 schema 和错误代码；超时后先查询已有事务，不重新生成身份。

配置修订与生效修订分别展示。Linux/macOS 的只读 Unix socket 使用受保护目录、对端身份、固定请求、版本与大小限制，绑定进程世代及身份；旧绑定摘要不能作为新绑定事实。无法获得运行事实时明确返回 unavailable/unknown。`status --check` 不会把未证实的健康状态当作成功。

## 发布限制

这是可审阅的开发改造，**尚不能认定更新手册全部验收完成**：

- Windows 已加入受管理员/服务 ACL 保护的只读命名管道、对端进程与服务镜像验证及有界读写；尚待 Windows 原生压力与对抗验收。
- Windows 日志读取使用 SCM 生命周期事件，macOS 读取服务文件日志；跟随按日志游标去重，Windows/macOS 路径仍待原生验收。macOS 的 `--since` 接受 UTC ISO 日期/时间，无法给无时间戳的旧文本行补造时间。
- 尚未完成 Windows/macOS 原生安装、无人登录启动、SCM/launchd 策略、服务账户证书信任、故障回滚和真实 Sunshine 联调验收。
- macOS 安装脚本/CI 是候选实现，没有签名、公证或已通过原生验收的发行承诺。
- 现有产品原本拒绝跨版本覆盖；本次没有伪造升级路径。MSI 已加入仅针对已知托盘文件、快捷方式和 Run 登记的清理项；跨版本升级/回滚矩阵仍需要在 `sarmg-upgrade` 中完成并验收。卸载默认保留状态。
- Sunshine macOS 协议用带出处的 vendored additive patch 保持仓库可独立构建。必须先部署匹配的 Server，再发布其不可变协议依赖；不能直接发布给不识别新枚举的旧 Server。

这些限制不能仅靠 Linux 上交叉编译消除，不能把本文件当作发行批准或实机通过记录。
