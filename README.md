# Host Monitoring Client

`host-monitor` `0.9.26` 是 Host Monitoring 的只读主机遥测客户端。它采集 CPU、内存、磁盘、网络和可选 NVIDIA 指标，通过 HTTPS 主动上报到 Server，并在网络不可用时使用有界本地队列重试。

当前支持 Windows x64、Linux x64 和 macOS Apple Silicon。Client 不开放入站端口，也不提供托盘或本地 Web；安装包和系统服务能力以对应 Release 为准。

## 配置概览

安装后先确认版本并停止后台服务，再初始化配置：

```sh
host-monitor version --format json
sudo host-monitor service stop
sudo host-monitor config init
sudo host-monitor config show --format json
```

交互式编辑会在保存前校验配置：

```sh
sudo host-monitor config edit
```

随后使用 Server 管理页生成的实例授权码配对，并启动服务：

```sh
sudo host-monitor pair --interactive
sudo host-monitor pair status --format json
sudo host-monitor service enable
sudo host-monitor service start
host-monitor status --check --format json
```

自动化配置、候选文件的 `validate/diff/apply`、授权码轮换、队列处理和诊断命令见[完整配置指南](docs/configuration.md)。不要把授权码、Client token 或 OTLP token 放进命令参数、Shell 历史或日志。

默认配置位置和安装步骤按平台不同，见[平台安装指南](docs/platform-setup.md)。

## 开发验证

```sh
cargo +1.98.0 fmt --all -- --check
cargo +1.98.0 clippy --locked --all-targets --all-features -- -D warnings
cargo +1.98.0 test --locked --all-features
```

## 文档

- [文档总览](docs/README.md)
- [完整配置指南](docs/configuration.md)
- [平台安装与升级](docs/platform-setup.md)
- [CLI 兼容矩阵](docs/releases/cli-compatibility.md)

代码采用 [Apache License 2.0](LICENSE-APACHE)。
