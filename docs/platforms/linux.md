# 在 Linux 上使用 xsoc

适用范围：Linux x86_64；Ubuntu 24.04 为原生构建和验证基线。其他平台见[平台入口](../platform-setup.md)。

## 安装并配对

先完成[安装前准备](../platform-setup.md#开始前准备)。从 [1.0.0 Release](https://github.com/isarmg/xsoc/releases/tag/v1.0.0) 下载 `xsoc_1.0.0_amd64.deb`（Debian/Ubuntu）或 `xsoc-1.0.0.x86_64.rpm`（RPM），以及 `SHA256SUMS-Linux-x86_64`。

进入下载目录，先核对安装包摘要：

```sh
sha256sum xsoc_1.0.0_amd64.deb
```

将输出与 `SHA256SUMS-Linux-x86_64` 中对应行比较。随后执行：

```sh
sudo apt install ./xsoc_1.0.0_amd64.deb
sudo xsoc service stop
sudo xsoc setup --interactive
sudo xsoc status --check --format json
```

RPM 系统先执行 `sha256sum xsoc-1.0.0.x86_64.rpm`，与同一校验文件中的 RPM 行比较，一致后改用 `sudo dnf install ./xsoc-1.0.0.x86_64.rpm` 安装；后续设置相同。包管理器安装 smartmontools 等运行依赖。

向导各项输入与成功标准见[向导输入与完成结果](../platform-setup.md#向导输入与完成结果)。随后按[日常使用](../usage.md)核对服务、绑定和最新报告。

## 日志与本机状态

服务为 `xsoc.service`，运行账户为 `xsoc`。查看系统日志：

```sh
sudo journalctl -u xsoc.service -n 100 --no-pager
```

默认配置为 `/etc/xsoc/config.json`，默认状态目录为 `/var/lib/xsoc`。实际 state_dir 以 `config show` 为准。

## 排查本机问题

- 安装失败：查看包管理器错误，核对架构、依赖、已有服务路径和目录权限。
- 服务未运行：检查上述 journal 和 `service status --format json`；完成配对后再启动。
- NVIDIA / GPU 设备不可见：默认 systemd 单元启用 `PrivateDevices=yes`。安装器检测设备后会显示本机适用的组权限与 `/usr/share/xsoc/xsoc-gpu.conf` 启用步骤；确认允许服务访问设备后再采用这些步骤。该 drop-in 会取消私有设备隔离。
- SMART 或 DMI 权限不足：先停止服务，运行 `sudo xsoc probe --format json` 检查对应能力及设备权限，结束后恢复服务；读取要求见[硬件监控](../hardware-monitoring.md)。

同版 DEB 可用 `sudo apt install --reinstall ./xsoc_1.0.0_amd64.deb` 修复，保留业务状态。RPM 使用已校验的同版包执行 `sudo dnf reinstall ./xsoc-1.0.0.x86_64.rpm`。修复后检查版本、服务与业务连接。

## 卸载

```sh
sudo apt remove xsoc
```

RPM 系统使用 `sudo dnf remove xsoc`。普通卸载停止服务并移除程序，保留配置、身份、凭据与待发队列。永久退役且确认待发数据可丢弃后，Debian 可执行 `sudo apt purge xsoc`；RPM 需在卸载前执行 `sudo xsoc-purge --yes`。这些清理操作会永久删除本地状态。

卸载完成后，对应系统服务应不再登记。

## 构建原生安装包

从仓库根目录构建。准备 Rust `1.99.0`、C/C++ 构建工具、`readelf` 与 nFPM `2.47.0`。以下命令通过 Go 安装指定版本 nFPM；已有该版本时可跳过首行，并把 `NFPM_BIN` 改为实际可执行文件路径：

```sh
go install github.com/goreleaser/nfpm/v2/cmd/nfpm@v2.47.0
XSOC_BUILD_SHA="$(git rev-parse HEAD)" cargo build --locked --release --bin xsoc
NFPM_BIN="$(go env GOPATH)/bin/nfpm" sh packaging/linux/build-packages.sh
```

生成 `dist/xsoc_1.0.0_amd64.deb` 和 `dist/xsoc-1.0.0.x86_64.rpm`。构建器核对 Cargo 版本、ELF 架构和版本标记；使用默认 `target/release/xsoc` 路径，不使用自定义 `CARGO_TARGET_DIR`。

通用工具链、质量检查和源码说明见[开发指南](../development.md)。原生安装器构建成功后，还需在对应系统验证安装、服务生命周期和真实设备行为。

## 继续使用

[配置](../configuration.md) · [日常使用](../usage.md) · [维护](../administration.md) · [通用排障](../troubleshooting.md) · [命令参考](../cli-compatibility.md)
