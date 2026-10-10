# xsoc

`xsoc` `1.0.0` 是 xsos 的只读主机遥测客户端。它采集 CPU、内存、磁盘健康、网络、硬件传感器、Mac 显卡清单及 NVIDIA / AMD / Intel 的可用显卡指标，通过 HTTPS 主动上报到服务端，并在网络不可用时使用有界本地队列重试。

`1.0.0` 使用 xcsc 1.0.0 的私有状态、CLI 和有界进程机制，以及 xcsc 包内 xcsc::log 1.0.0 的后台服务持久日志；正常运行只接受当前配置与账户格式，其他格式保全后明确报错。Rust 固定 1.99.0，依赖由受控 Git 来源和根锁文件记录，发行按最终源码执行原生 CI。改动见[发行说明](docs/releases/1.0.0.md)。

当前支持 Windows x64、Linux x64 和 macOS Apple Silicon。客户端不开放入站端口，也不提供托盘或本地网页；安装包和系统服务能力以对应发行版本为准。

## 配置概览

以下命令以 Linux/macOS 终端为例；Windows 请在管理员 PowerShell 中运行相同的 `xsoc` 命令，不使用 `sudo`。具体安装路径见[平台安装指南](docs/platform-setup.md)。

安装后先确认版本并停止后台服务，再初始化配置：

```sh
xsoc version --format json
sudo xsoc service stop
sudo xsoc config init
sudo xsoc config show --format json
```

交互式编辑会在保存前校验配置：

```sh
sudo xsoc config edit
```

随后使用服务端管理页生成的实例授权码配对，并启动服务：

```sh
sudo xsoc pair --interactive
sudo xsoc pair status --format json
sudo xsoc service enable
sudo xsoc service start
xsoc status --check --format json
```

交互配对时，实例授权码按普通文本输入并在终端中明文显示；没有遮罩、隐藏切换或二次显示模式。使用后仍不会
写入日志或命令参数，并继续由可清零内存缓冲区保存。

自动化配置、候选文件的 `validate/diff/apply`、授权码轮换、队列处理和诊断命令见[完整配置指南](docs/configuration.md)。不要把授权码、客户端令牌或 OTLP 令牌放进命令参数、Shell 历史或日志。

默认配置位置和安装步骤按平台不同，见[平台安装指南](docs/platform-setup.md)。

## 开发验证

```sh
cargo +1.99.0 fmt --all -- --check
cargo +1.99.0 clippy --locked --all-targets --all-features -- -D warnings
cargo +1.99.0 test --locked --all-features
```

## 文档

- [文档总览](docs/README.md)
- [完整配置指南](docs/configuration.md)
- [分平台部署、配对、服务管理与卸载](docs/platform-setup.md)
- [CLI 兼容矩阵](docs/releases/1.0.0.md)

代码采用 [Apache License 2.0](LICENSE-APACHE)。

硬件监控扩展、平台支持与当前协议要求见 [硬件监控说明](docs/hardware-monitoring.md)。Windows x64 的 AMD / Intel 只读采集见 [ADLX / IGCL 说明](docs/windows-gpu-vendors.md)。

## 仓库布局

本仓库只有一个实际 Rust 包，根 Cargo.toml/Cargo.lock 约束依赖图，src 包含采集器、平台服务、状态和 CLI，tests 保存独立行为与传输验证。协议依赖是独立产品的固定 Git 输入，不是本仓库子包。packaging 保存系统安装器及维护夹具，scripts 保存构建与发行检查，docs 描述当前操作和合同。大型平台模块按实际职责拆分，测试放在模块旁。

依赖选择与保留的原生边界见[依赖和 unsafe 审查](docs/unsafe-audit.md)。

当前发布版本：**1.0.0**。参见 [1.0.0 发布说明](docs/releases/1.0.0.md)。

CLI 参数、输出与兼容性约定见 [CLI 兼容性](docs/cli-compatibility.md)。

公共支撑的职责、单体依赖、平台边界与验证方法见[公共支撑说明](docs/common-support.md)。
