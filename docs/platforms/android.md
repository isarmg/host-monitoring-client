# Android 宿主集成 xsoc

本页面向开发 Android 原生应用的集成者。xsoc 提供宿主驱动的 Rust 报告构造库，当前 CI 检查 `aarch64-linux-android`；仓库没有可安装的 APK、Android 前台服务或现成 JNI 绑定。

## 检查 Rust 目标

在仓库根目录使用 Rust `1.99.0`。以下与 Android 库 CI 的检查范围一致，可在 Linux 开发主机执行：

```sh
rustup target add --toolchain 1.99.0 aarch64-linux-android
cargo +1.99.0 check --locked -p xsoc --target aarch64-linux-android --lib --no-default-features
```

成功表示无桌面特性的 Rust 库通过该目标的类型检查。这一步不链接 Android 应用，也不生成 APK；原生宿主的 NDK、JNI/FFI 桥接和应用打包由宿主项目负责。

## 接入 Android 宿主

1. 在宿主 Rust 层关闭 xsoc 的默认特性，按[移动宿主合同](../mobile-host.md)使用 `MobileHostAdapter`。
2. 将平台设为 `MobilePlatform::Android`，由宿主提供稳定身份和应用沙箱可见的采样结果。
3. 由 Android 宿主管理权限、前后台调度、HTTPS 传输和 Keystore 凭据存储。
4. 在真机验证前后台切换、权限被撤回及网络恢复后的报告投递；报告构造库本身不会持续运行或重试发送。

## 排查集成问题

- 提示缺少 Rust 目标：使用与检查命令相同的 `1.99.0` 工具链添加 `aarch64-linux-android`。
- 编入桌面服务或采集依赖：检查宿主依赖是否设置 `default-features = false`，以及其他依赖是否重新启用了 xsoc 的 `desktop`、`nvidia` 或 `otlp` 特性。
- 报告被拒绝：按[移动宿主合同](../mobile-host.md#报告校验)处理身份与字段错误；网络及系统调度错误由 Android 宿主诊断。

[移动宿主合同](../mobile-host.md) · [iOS / iPadOS](ios.md) · [共用开发检查](../development.md) · [平台入口](../platform-setup.md)
