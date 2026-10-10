# xsoc 开发与验证

## 工具链与质量检查

使用 Rust `1.99.0`、Git、平台 C/C++ 构建工具和 Python `3.11+`。依赖由受控 Git 来源和根锁文件记录；正式发行须按最终源码执行对应平台原生 CI。

```sh
cargo +1.99.0 fmt --all -- --check
cargo +1.99.0 clippy --locked --all-targets --all-features -- -D warnings
cargo +1.99.0 test --locked --all-features
python3 scripts/check-xcsc-source.py
python3 scripts/check-workflow-supply-chain.py
```

## 按平台构建

原生依赖、命令和输出路径见对应平台页：

- [Linux 原生包](platforms/linux.md#构建原生安装包)
- [Windows MSI](platforms/windows.md#构建原生安装包)
- [macOS PKG](platforms/macos.md#构建原生安装包)
- [Android 宿主库检查](platforms/android.md#检查-rust-目标)
- [iOS / iPadOS 宿主库检查](platforms/ios.md#检查-rust-目标)

## 仓库与运行边界

本仓库只有一个实际 Rust 包：根 `Cargo.toml`/`Cargo.lock` 约束依赖图，`src/` 保存采集、平台服务、状态与 CLI，`tests/` 保存独立行为和传输验证。协议是独立产品的固定 Git 输入，`packaging/` 保存系统安装器与维护夹具，`scripts/` 保存构建、发行检查，`docs/` 描述当前操作和合同。大型平台模块按职责拆分，模块测试就近保存。

当前使用 xcsc 的私有状态、CLI、有界进程和持久服务日志能力。正常运行只接受当前配置与账户格式；异常状态保全后明确报错。依赖选择和原生边界见[依赖与 unsafe 审查](unsafe-audit.md)，具体公共接口见[公共支撑](common-support.md)。

客户端不提供托盘或本地网页；移动宿主库接口见[移动宿主合同](mobile-host.md)。配置候选文件、轮换、队列和诊断见[配置指南](configuration.md)。不要把授权码、客户端令牌或 OTLP 令牌写入命令参数、Shell 历史或日志；交互授权码会明文回显，应使用受保护终端。

代码采用 [Apache License 2.0](../LICENSE-APACHE)。

## 编辑文档

面向使用者按安装、配置、正常使用、维护和排障组织内容；完整字段与输出集中在参考页。示例写明平台和权限，预期结果紧跟操作，秘密与数据清理提示放在对应步骤。参照 [GNU 手册建议](https://www.gnu.org/prep/standards/html_node/GNU-Manuals.html)。检查链接和命令后，运行受影响的安装包文档检查。

## 可选 OTLP

默认特性为 desktop、nvidia。需要 OTLP 的自建客户端可执行：

```sh
XSOC_BUILD_SHA="$(git rev-parse HEAD)" cargo build --locked --release --features otlp --bin xsoc
```

OTLP 在主服务确认报告后尽力导出，使用独立目标和凭据。移动端构建与宿主职责见上面的独立平台入口。
