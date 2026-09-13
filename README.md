# Host Monitoring Client

跨平台只读主机遥测客户端。公开入口是 `host-monitor`，后台由操作系统服务管理器运行，无托盘、本机网页或浏览器启动入口。集中管理仍在独立的 Host Monitoring Server 中。

当前纯 CLI 与系统服务版本为 `0.9.14`；用法和验收限制见 [CLI 改造说明](docs/releases/cli-unreleased.md)。安装产物未签名、未公证，实机与升级验收边界见发行说明。

支持 Windows x64、Linux x64 和 macOS Apple Silicon（arm64）。不再为 Intel macOS 适配、运行 CI 或提供发行包。

详细的 Windows、Linux、macOS 安装命令、配置路径、覆盖修复与日志说明见 [各平台安装配置指南](docs/platform-setup.md)。

## 部署与配对

每个平台只发布一个原生安装包文件：DEB/RPM、MSI 或 PKG。安装器检查平台、架构、权限并注册低权限系统服务；有交互终端的首次安装会直接调用唯一的 `setup` 命令，完成交互式配对、开机启动选择、立即启动和连接验证。Linux 使用 `host-monitor` 低权限账户，macOS 使用 `_hostmonitor`，Windows 保持 LocalService。Unix 写命令使用 `sudo`，Windows 使用已提权终端；静默或无终端安装会保留进度，随后显式运行 `setup`。

```sh
host-monitor setup
```

`setup` 会在已存在有效身份时复用身份，在未完成的配对事务上执行 `pair resume`；配对失败不会回滚安装或删除进度。配置、配对持久化、服务注册、启动策略、运行状态与连接均逐关卡复查，只有当前关卡验证成功才进入下一关；失败结果包含稳定的 `error.code`、`error.step`、可读 `error.message`，服务管理器失败还包含受限长度的 `error.detail`。默认配置位置：Linux `/etc/host-monitor/config.json`，macOS `/Library/Application Support/host-monitor/config.json`，Windows ProgramData 下 `host-monitor/config.json`。`--config` 可以选择绝对配置路径；服务命令只能操作与已安装服务注册一致的配置。

自动化通过 stdin 交付单个 JSON 文档：字段为 `server` 和 `authorization_code`。例如部署器启动 `host-monitor setup --input-stdin --non-interactive --format json` 后写入受保护输入；非交互模式默认完成服务启用、启动和验证，不要在 Shell 参数或日志中拼接秘密。

`pair resume` 只核对已有事务。`pair replace --confirm-replace --expected-binding <当前 host_id> --interactive` 明确替换绑定，要求旧待发送/隔离队列均为空。先通过 `queue status`、`queue inspect`、`queue drain --timeout 60s` 检查或排空；不会自动删除旧数据或换身份发送。

## 配置和诊断

```sh
host-monitor config show --format json
host-monitor config validate --file /absolute/candidate.json
host-monitor config diff --file /absolute/candidate.json
host-monitor service stop
host-monitor config apply --file /absolute/candidate.json --expected-revision REVISION
host-monitor service start
host-monitor status --check
```

`config show/diff` 脱敏，提交核对修订并原子持久化。已绑定的 Server 地址及状态目录不能通过普通配置提交迁移。保留当前配置/状态版本检查，不通过修改版本字段绕过升级工具。

`probe` 仅采集、不创建持久身份。`once` 是独占会话内的真实交付。`doctor` 默认本地只读，`doctor --network` 主动探测，`doctor --delivery` 显式真实交付。`status --watch --format ndjson` 的 Ctrl+C 只退出观察。

## 构建验证

```sh
cargo fmt --all -- --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked
cargo check --locked --no-default-features
```

测试中的本地 HTTP/TLS 和 IPC 需要创建本机监听端点的权限。保留 Linux DEB/RPM、Windows 原生服务维护程序和 macOS LaunchDaemon 打包基础。默认卸载保留身份与队列；在 Server 退役设备后再安排受控的数据处置。

版本维度、源提交及升级/回退边界见 [CLI 兼容矩阵](docs/releases/cli-compatibility.md)。
