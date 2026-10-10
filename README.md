# xsoc

xsoc 是 xsos 的只读主机监控客户端，主动通过 HTTPS 上报遥测，并在断网时使用有界本地队列重试。

## 项目功能

- 采集 CPU、内存、磁盘健康、网络、硬件传感器和可用的显卡指标
- 配对监控服务端，管理上报队列，并提供状态与诊断命令
- 以系统服务在后台运行，无需开放入站端口

## 适用平台

Windows 11 x64、Linux x86_64（systemd，提供 DEB/RPM）和 macOS Apple Silicon（arm64）。Linux 原生发行在 Ubuntu 24.04 构建和验证。

## 快速部署

从 [Release](https://github.com/isarmg/xsoc/releases) 下载本机平台的安装包及对应 `SHA256SUMS-*`，核对 SHA-256 后安装。准备 xsos 管理员提供的 HTTPS 地址与实例授权码。

Linux（Debian/Ubuntu；RPM 系统改用 `sudo dnf install ./xsoc-1.0.0.x86_64.rpm`）：

```sh
sudo apt install ./xsoc_1.0.0_amd64.deb
sudo xsoc service stop
sudo xsoc setup --interactive
sudo xsoc status --check --format json
```

macOS Apple Silicon：

```sh
sudo installer -pkg ./xsoc-1.0.0-macos-arm64-unsigned.pkg -target /
sudo /usr/local/bin/xsoc service stop
sudo /usr/local/bin/xsoc setup --interactive
sudo /usr/local/bin/xsoc status --check --format json
```

Windows：运行 `xsoc-1.0.0-x64.msi` 完成安装，再打开新的管理员 PowerShell：

```powershell
xsoc service stop
xsoc setup --interactive
xsoc status --check --format json
```

向导完成配对、设置开机自启（默认 Yes）、启动服务并验证连接。最后到 xsos 管理页确认主机收到新报告。授权码会在终端明文回显，请使用受保护终端；不要将秘密放进命令参数。macOS 包未签名、未公证，校验后按系统批准流程安装。

## 编译部署

准备 Rust `1.99.0`、Git 和平台 C/C++ 构建工具；Linux 打包还需 nFPM `2.47.0`、`readelf`，首次安装会由包管理器安装 systemd、smartmontools 等运行依赖。以 Linux x86_64 为例：

```sh
git clone https://github.com/isarmg/xsoc.git
cd xsoc
XSOC_BUILD_SHA="$(git rev-parse HEAD)" cargo build --locked --release --bin xsoc
sh packaging/linux/build-packages.sh
sudo apt install ./dist/xsoc_1.0.0_amd64.deb
sudo xsoc service stop
sudo xsoc setup --interactive
sudo xsoc status --check --format json
```

DEB/RPM 输出到 `dist/`。Windows 使用 MSVC 工具链构建 `xsoc` 和 `xsoc-maintenance` 后由 WiX 打包；macOS 使用 Apple Silicon 原生工具链和 `packaging/macos/build-pkg.sh` 生成 PKG。各平台打包输入与完整命令见详细文档。

[详细文档](docs/README.md)
