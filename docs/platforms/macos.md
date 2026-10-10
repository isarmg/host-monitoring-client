# 在 macOS 上使用 xsoc

适用范围：Apple Silicon arm64。其他平台见[平台入口](../platform-setup.md)。

## 安装并配对

先完成[安装前准备](../platform-setup.md#开始前准备)。从 [1.0.0 Release](https://github.com/isarmg/xsoc/releases/tag/v1.0.0) 下载 `xsoc-1.0.0-macos-arm64-unsigned.pkg` 与 `SHA256SUMS-Darwin-arm64`。

PKG 未签名、未公证。核验来源与摘要后，按系统提供的批准流程安装。

在下载目录核对 PKG 摘要，与 `SHA256SUMS-Darwin-arm64` 中同名行比较：

```sh
shasum -a 256 xsoc-1.0.0-macos-arm64-unsigned.pkg
```

一致后安装并设置：

```sh
sudo installer -pkg ./xsoc-1.0.0-macos-arm64-unsigned.pkg -target /
sudo /usr/local/bin/xsoc service stop
sudo /usr/local/bin/xsoc setup --interactive
sudo /usr/local/bin/xsoc status --check --format json
```

PKG 包含独立 smartctl 7.5，后台读取能力仍取决于设备与服务账户权限。

向导各项输入与成功标准见[向导输入与完成结果](../platform-setup.md#向导输入与完成结果)。随后按[日常使用](../usage.md)核对服务、绑定和最新报告。

## 日志与本机状态

launchd 服务为 `org.sarmg.xsoc`，运行账户为 `_xsoc`。命令入口为 `/usr/local/bin/xsoc`。

```sh
sudo /usr/local/bin/xsoc service status --format json
sudo tail -n 100 /var/log/xsoc.log
```

默认配置为 `/Library/Application Support/xsoc/config.json`。

## 排查本机问题

- 找不到命令：使用本页的 `/usr/local/bin/xsoc` 绝对路径。
- PKG 安装失败：查看 `/var/log/install.log`，核对 Apple Silicon 架构、账户及已有安装路径；使用系统提供的未签名软件批准流程。
- 硬件读数缺失：先停止服务，运行 `sudo /usr/local/bin/xsoc probe --format json` 查看能力诊断，结束后恢复服务。PKG 包含独立 arm64 smartctl 7.5；后台账户能读取哪些设备由权限和驱动决定。GPU 清单和空值含义见[硬件监控](../hardware-monitoring.md)。

修复时重新执行本页已校验 PKG 的 `installer` 命令，保留业务状态。完成后检查版本、服务与业务连接。

## 卸载

```sh
sudo /usr/local/share/xsoc/uninstall.sh
```

普通卸载停止并移除程序和服务，保留配置、身份、凭据及队列。永久退役且确认待发数据可丢弃后，使用 `sudo /usr/local/share/xsoc/uninstall.sh --purge` 清除安装器管理的本地状态；该操作不可撤销。

卸载完成后，对应系统服务应不再登记。

## 构建原生安装包

在 Apple Silicon Mac 的仓库根目录执行。需要 Xcode 命令行工具、Python `3.11+`、Rust `1.99.0` arm64 工具链和系统 PKG 工具：

```sh
XSOC_BUILD_SHA="$(git rev-parse HEAD)" cargo build --locked --release --target aarch64-apple-darwin --bin xsoc
mkdir -p dist
OUTPUT_DIRECTORY="$PWD/dist/smartmontools-macos-arm64" sh packaging/macos/build-smartmontools.sh
SMART_PAYLOAD="$PWD/dist/smartmontools-macos-arm64" \
  BINARY="$PWD/target/aarch64-apple-darwin/release/xsoc" VERSION=1.0.0 \
  OUTPUT="$PWD/dist/xsoc-1.0.0-macos-arm64-unsigned.pkg" sh packaging/macos/build-pkg.sh
```

smartmontools 输出目录必须尚不存在；脚本核对上游源码摘要。PKG 未签名、未公证，不能把构建成功视为系统信任或实机验收。

通用工具链、质量检查和源码说明见[开发指南](../development.md)。原生安装器构建成功后，还需在对应系统验证安装、服务生命周期和真实设备行为。

## 继续使用

[配置](../configuration.md) · [日常使用](../usage.md) · [维护](../administration.md) · [通用排障](../troubleshooting.md) · [命令参考](../cli-compatibility.md)
