# Host Monitoring Client

独立的只读主机遥测客户端。Server 与管理 Web 位于
[host-monitoring-server](https://github.com/isarmg/host-monitoring-server)，本仓库不构建 Server。
协议通过完整 Git 提交固定依赖；Foundation Client SDK 采用精确版本与提交，不需要相邻仓库。

桌面客户端支持 Linux、Windows、macOS；不带默认桌面能力的库另外检查 Android/iOS 目标。
具体支持平台与安装步骤见 `packaging/linux`、`packaging/windows`、`packaging/macos`。
可执行文件名称为 `host-monitor`；Windows 另有维护和托盘程序。

```sh
cargo test --locked -p host-monitor
cargo clippy --locked -p host-monitor --all-targets -- -D warnings
cargo check --locked -p host-monitor --no-default-features --all-targets
sh packaging/linux/tests/test-lifecycle.sh
sh packaging/linux/tests/test-build-packages.sh
```

客户端通过配对获得自己的设备凭据，默认验证 HTTPS；不开放通用远程执行功能。
本次拆分取消产品旧命名，不提供旧状态兼容或迁移。升级和恢复仍以 `sarmg-upgrade` 的支持矩阵为准。
