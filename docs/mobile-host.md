# xsoc 移动宿主合同

Android、iOS 和 iPadOS 共用 `src/mobile.rs` 的报告构造接口。本页集中说明数据和职责；各平台工具链见 [Android](platforms/android.md) 与 [iOS / iPadOS](platforms/ios.md)。

## 引入库

xsoc 是源码依赖，未发布到 crates.io。可将受控源码放入宿主工作区，在宿主的 `Cargo.toml` 中使用指向它的路径，并关闭默认特性。例如 xsoc 源码位于宿主包的同级目录时：

```toml
[dependencies]
xsoc = { path = "../xsoc", default-features = false }
```

默认 `desktop`、`nvidia` 特性面向桌面服务；移动宿主只使用不依赖桌面能力的 `mobile` 和报告模型。当前接口是 Rust API，跨语言桥接由宿主实现。

## 构造并交付报告

1. 用 `MobileHostDescriptor` 提供 `host_id`、平台、可选系统版本和架构，传入 `MobileHostAdapter::new`。
2. 每次宿主获得执行时间时，采集应用可见数据，构造 `MobileSample`：采集时间、采样间隔、`SystemSnapshot`、能力诊断和宿主待发批次数。
3. 调用 `prepare_report`，成功后取得 `MobileReportPayload`。`body()` 返回 JSON 字节，`content_type()` 返回 `application/json`；`report()` 可读取校验后的报告结构。
4. 宿主通过自己的 HTTPS 客户端提交报告，并负责凭据加载、证书信任、重定向策略、重试和持久队列。库不保存或附加认证信息。

库每次构造报告时生成新报告 ID。宿主重试同一份报告时应保留原 payload，避免把重试构造成另一份新报告。

## 报告校验

`host_id` 必须是规范的小写连字符 UUID，架构不能为空；否则分别返回 `InvalidHostId` 或 `EmptyArchitecture`。报告内容通过共享报告合同校验，错误以 `InvalidReport` 返回，宿主应保留诊断并修正数据后再提交。

库自动标记整机采集和常驻后台守护进程能力为 `unsupported`。它只构造并限制报告内容，不创建线程、定时器、套接字、后台任务或持久文件。缺少的系统信息保持不可用，不用虚构的数值补齐。

宿主负责 Android Keystore / Apple Keychain、用户授权、沙箱内采集、应用生命周期及系统允许的后台调度。真实设备、权限和投递验收在宿主应用完成，Rust 目标类型检查不覆盖这些行为。

接口源码：[移动适配器](../src/mobile.rs) · [库导出](../src/lib.rs) · [特性定义](../Cargo.toml)。通用开发检查见[开发指南](development.md)。
