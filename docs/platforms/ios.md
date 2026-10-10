# iOS / iPadOS 宿主集成 xsoc

本页面向开发 iPhone 和 iPad 原生应用的集成者。xsoc 提供宿主驱动的 Rust 报告构造库，当前 CI 检查 arm64 真机和 Apple Silicon 模拟器目标；仓库没有可安装的 IPA、Xcode 应用工程或现成 Swift 绑定。

## 检查 Rust 目标

在配置了 Xcode 开发环境的 Apple Silicon Mac 上，从仓库根目录执行。Rust 版本为 `1.99.0`；Apple 移动库 CI 使用 macOS 26：

```sh
rustup target add --toolchain 1.99.0 aarch64-apple-ios aarch64-apple-ios-sim
cargo +1.99.0 check --locked -p xsoc --target aarch64-apple-ios --lib --no-default-features
cargo +1.99.0 check --locked -p xsoc --target aarch64-apple-ios-sim --lib --no-default-features
```

前一条检查真机目标，后一条检查 Apple Silicon 模拟器目标。成功表示 Rust 库通过相应目标的类型检查；宿主仍需完成 Swift/FFI 桥接、链接、签名、权限配置和应用打包。

## 接入 iPhone 或 iPad 宿主

1. 在宿主 Rust 层关闭 xsoc 的默认特性，按[移动宿主合同](../mobile-host.md)使用 `MobileHostAdapter`。
2. iPhone 设置 `MobilePlatform::Ios`，iPad 设置 `MobilePlatform::IpadOs`。两者使用相同 Rust 目标，由宿主指定报告中的产品身份。
3. 由 Apple 宿主管理沙箱内采样、权限、应用生命周期、HTTPS 传输和 Keychain 凭据存储。
4. 在真机检查后台执行窗口、挂起与恢复、权限变化和网络恢复后的报告投递。

## 排查集成问题

- 提示缺少 Rust 目标：确认目标安装在同一个 `1.99.0` 工具链中，真机和模拟器目标不可混用。
- 应用无法链接或签名：检查宿主 Xcode 工程的 SDK、目标架构、桥接和签名；库的 `cargo check` 不包含这些步骤。
- 报告的系统身份不符：检查传入的是 `Ios` 还是 `IpadOs`，不要仅从 Rust 的 `target_os = "ios"` 推断设备类别。
- 后台没有继续上报：检查宿主是否获得系统执行时间；该库不会创建后台服务或计划任务。

[移动宿主合同](../mobile-host.md) · [Android](android.md) · [共用开发检查](../development.md) · [平台入口](../platform-setup.md)
