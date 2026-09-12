> 此文件保留早期 CLI 验收记录。当前覆盖安装与修复行为见 [0.9.10](0.9.10.md) 和 [平台指南](../platform-setup.md)。

# 纯命令行改造（v0.9.6 预发布）

当前支持 Windows x64、Linux x64、macOS Apple Silicon。Intel macOS 已从适配、CI 和发行范围移除；下文早期验证记录仅供追溯。

公开入口为产品 CLI，长期运行由 SCM、systemd 或 launchd 管理。删除托盘、网页控制服务和登录自启入口；服务端集中管理网页保留。没有版本号或持久状态格式的暗中迁移。

管理员先停 Client 服务，再进行配对、配置提交或凭据更新。外层维护锁先于运行实例/状态事务锁获取，冲突有界失败；查询不创建锁和缺失状态。现有身份、凭据、Host 队列和 Sunshine 执行记录继续使用原格式。

## 公共用法

- `version` / `--version`；机器输出增加 `--format json`。
- `config init`，`config show`，`config validate --file /absolute/candidate.json`。
- `config diff --file /absolute/candidate.json`。
- `config apply --file /absolute/candidate.json --expected-revision REVISION`。
- `pair --interactive` 或 `pair --input-stdin --non-interactive --format json`。
- `pair status`，`pair resume`。恢复必须找到已有事务。
- `setup`：安装后的唯一交互入口，串联配对、服务启动策略和连接验证；非交互部署使用受保护 stdin。
- `status`，`status --watch --format ndjson --timeout 5m`，`status --check`。
- `doctor` 只读；`doctor --network` 显式访问公共健康端点，报告当前 CLI 账户的信任环境。
- `service status|start|stop|restart|enable|disable`；`enable/disable --now` 同时操作当前运行状态。
- Linux：`logs --tail 100`、`logs --since '2026-09-07'`、`logs --follow --format ndjson`。

`--output` 是 `--format` 的兼容别名。默认不输出终端控制序列。输入 JSON 最多 64 KiB，拒绝未知字段；禁止把授权码、密码或长期凭据作为参数。交互秘密输入不回显。JSON 有固定 schema 和错误代码；超时后先查询已有事务，不重新生成身份。

配置修订与生效修订分别展示。Linux/macOS 的只读 Unix socket 使用受保护目录、对端身份、固定请求、版本与大小限制，绑定进程世代及身份；旧绑定摘要不能作为新绑定事实。无法获得运行事实时明确返回 unavailable/unknown。`status --check` 不会把未证实的健康状态当作成功。

## 验证与发行边界

已发布到 `codex/client-cli-completion-20260907` 验证分支，详见 [精确基线与兼容矩阵](cli-compatibility.md)。代码提交 `2d17630915bb3f1c3fb3b11023b8c922cd271984` 的[八个 CI 作业全部通过](https://github.com/isarmg/host-monitoring-client/actions/runs/34198794898)：Windows MSVC/MSI、Linux DEB/RPM、macOS Intel/Apple Silicon PKG、OTLP 端到端及三个移动库边界作业。各桌面平台安装产物及源码身份/校验和已上传到该 CI 页面。

Windows 原生验收覆盖安装后停止、显式服务启动、经过校验的只读 IPC、离线业务健康不夸报、运行中配置维护冲突、停服务后原子提交、LocalService 读取管理员提交的配置、启用/关闭开机运行、卸载保留状态、重装及单独清除。macOS 两种架构通过原生安装、保留状态重装和清除；安装故障回滚另有自动化回归。

Windows 新文件继承已验证的私有目录 ACL，并在写入前验证权限，避免要求 LocalService 重设 DACL。维护使用保留文件名称保护的字节范围锁，允许安装器只读检查元数据；管道 ACL 验证连接者权限，客户端核验服务进程和绑定。启动失败通过 SCM 返回固定分类及数字系统错误码，不输出秘密状态。

配置和配对状态格式固定为历史 `0.9.4`，已与程序版本分离。此次不重写身份和队列，也不创建未经验证的历史迁移边。`sarmg-upgrade` 的 Server 恢复命令不适用于 Client。

Windows 日志读取使用 SCM 生命周期事件，macOS 读取服务文件日志。macOS 的 `--since` 接受 UTC ISO 日期/时间，无法给没有时间戳的旧文本行补造时间。

验证使用一次性 runner 与合成离线身份，没有证明全部真实 Server 配对、网络故障及长时间运行场景。本次 GitHub Release 标记为预发布，安装产物未签名。发布者签名、公证、生产 Server 部署、重启后无人登录的物理设备验收仍未执行。
