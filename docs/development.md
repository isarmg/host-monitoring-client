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

## Linux x86_64 原生包

准备 `readelf` 与 nFPM `2.47.0`。可用已安装的 nFPM，或在有 Go 工具链的环境执行：

```sh
go install github.com/goreleaser/nfpm/v2/cmd/nfpm@v2.47.0
XSOC_BUILD_SHA="$(git rev-parse HEAD)" cargo build --locked --release --bin xsoc
NFPM_BIN="$(go env GOPATH)/bin/nfpm" sh packaging/linux/build-packages.sh
```

生成 `dist/xsoc_1.0.0_amd64.deb` 和 `dist/xsoc-1.0.0.x86_64.rpm`。构建器核对 Cargo 版本、ELF 架构和版本标记；使用默认 `target/release/xsoc` 路径，不使用自定义 `CARGO_TARGET_DIR`。安装、配对和服务操作见[平台指南](platform-setup.md)。

## Windows x64 原生包

在带 MSVC C++ 工具和 .NET SDK 的原生 Windows 环境执行。WiX `4.0.6` 由项目文件固定；需要构建两个二进制并提供完整 smartmontools payload：

```powershell
$env:XSOC_BUILD_SHA = git rev-parse HEAD
cargo build --locked --release --target x86_64-pc-windows-msvc --bin xsoc --bin xsoc-maintenance
.\packaging\windows\fetch-smartmontools.ps1 -OutputDirectory "$env:TEMP\xsoc-smartmontools"
.\packaging\windows\wix\build-msi.cmd 1.0.0 target\x86_64-pc-windows-msvc\release\xsoc.exe target\x86_64-pc-windows-msvc\release\xsoc-maintenance.exe "$env:TEMP\xsoc-smartmontools\payload"
```

MSI 输出在 `packaging/windows/wix/bin/x64/Release/` 下。打包不替代真实安装器生命周期、权限和驱动验收。

## macOS Apple Silicon 原生包

需要 Xcode 命令行工具、Python `3.11+`、Rust arm64 工具链和系统 PKG 工具：

```sh
XSOC_BUILD_SHA="$(git rev-parse HEAD)" cargo build --locked --release --target aarch64-apple-darwin --bin xsoc
mkdir -p dist
OUTPUT_DIRECTORY="$PWD/dist/smartmontools-macos-arm64" sh packaging/macos/build-smartmontools.sh
SMART_PAYLOAD="$PWD/dist/smartmontools-macos-arm64" \
  BINARY="$PWD/target/aarch64-apple-darwin/release/xsoc" VERSION=1.0.0 \
  OUTPUT="$PWD/dist/xsoc-1.0.0-macos-arm64-unsigned.pkg" sh packaging/macos/build-pkg.sh
```

smartmontools 输出目录必须尚不存在；脚本核对上游源码摘要。PKG 未签名、未公证，不能把构建成功视为系统信任或实机验收。

## 仓库与运行边界

本仓库只有一个实际 Rust 包：根 `Cargo.toml`/`Cargo.lock` 约束依赖图，`src/` 保存采集、平台服务、状态与 CLI，`tests/` 保存独立行为和传输验证。协议是独立产品的固定 Git 输入，`packaging/` 保存系统安装器与维护夹具，`scripts/` 保存构建、发行检查，`docs/` 描述当前操作和合同。大型平台模块按职责拆分，模块测试就近保存。

当前使用 xcsc 的私有状态、CLI、有界进程和持久服务日志能力。正常运行只接受当前配置与账户格式；异常状态保全后明确报错。依赖选择和原生边界见[依赖与 unsafe 审查](unsafe-audit.md)，具体公共接口见[公共支撑](common-support.md)。

客户端不提供托盘或本地网页；移动宿主库边界与原生桌面安装包不同，见[硬件监控](hardware-monitoring.md)。配置候选文件、轮换、队列和诊断见[配置指南](configuration.md)。不要把授权码、客户端令牌或 OTLP 令牌写入命令参数、Shell 历史或日志；交互授权码会明文回显，应使用受保护终端。

代码采用 [Apache License 2.0](../LICENSE-APACHE)。
