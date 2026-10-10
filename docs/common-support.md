# xsoc 的公共支撑职责与单体依赖

本项目属于 **Client**，Rust 公共支撑只依赖 **`xcsc` 一个包**。公共代码已物理迁入该包的 `src/` 内部模块；下游没有继续依赖原来的子 crate，也没有通过一个外壳包间接安装它们。

| 公共包 | 唯一使用方 | 平台范围 | 分发方式 |
|---|---|---|---|
| `xcss` | Server | Linux x86_64 / AMD64，GNU libc，`x86_64-unknown-linux-gnu` | 一个 Rust Cargo package |
| `@xcss/web` | Server 的管理 Web 构建 | 构建机为 Linux x64 / glibc；生成页面由浏览器访问 | 一个 npm 包、一个 `xcss-web-1.0.0.tgz` |
| `xcsc` | Client，包括升级工具 `xssc` | 按内部模块支持 Linux、Windows、macOS、Android、iOS；产品实际支持以其部署文档和发行附件为准 | 一个 Rust Cargo package |

包名、仓库名、软件版本和内部模块是不同概念。`xcss::log` 或 `xcsc::runtime` 表示同一个包内的模块，不代表一个独立依赖包。版本号使用 `1.0.0`；产品维护的数据合同、UUIDv4、IPv4/IPv6、第三方软件版本及系统 API 名称保持其真实含义。

## 依赖如何固定

根或子目录 `Cargo.toml` 固定官方 Git URL、完整 40 位 `rev` 和 `version = "=1.0.0"`；`Cargo.lock` 固定该源与依赖摘要。两者必须一起更新。临时本地覆盖仅用于联调，正式 CI 与发行构建使用固定的官方源码。

消费产品的业务协议包可以共享严格的数据类型，但必须保持独立：Client 依赖协议类型时，依赖图也不能间接带入 `xcss`、Server HTTP、管理员后台或 Server 生命周期实现。

## Client 集成

Rust 通过 `xcsc::runtime`、`xcsc::cli`、`xcsc::fs_safety`、`xcsc::secret`、`xcsc::secret_envelope`、`xcsc::secure_xml` 和 `xcsc::log` 等内部模块取得公共能力。桌面 CLI、移动 FFI 和平台文件保护各自保留原有边界。

普通 Client 不安装 `@xcss/web`，也不依赖 `xcss`。桌面、Android 和 iOS 的安装、配对、重新配对、启动停止、状态、诊断和卸载，仍使用本项目部署文档中对应平台的实际入口；公共库本身没有独立服务、配对码或卸载器。

- `mobile-ffi` 启用移动 C ABI；`jni` 同时启用它并提供 Android JNI 入口。C 数据布局、状态码、结果释放规则和原生符号不能因模块迁移而改变。
- `tracing` 启用 Client 的结构化日志适配；日志实现及错误类型来自 `xcsc` 自身。
- 升级工具需要的 `offline-maintenance` 是同一个 `xcsc` 包内的显式 Linux 维护能力。普通移动依赖不编译此 SQLite 维护模块，也不获得 Server HTTP 或管理员运行时。

从仓库根目录执行 `cargo tree --locked -e normal` 可以查看全部实际运行依赖，确认公共代码只来自 `xcsc` 且没有直接或间接 `xcss`。`cargo metadata --locked --no-deps --format-version 1` 列出本项目 Cargo 包、目标与声明的依赖，便于核对具体 `rev` 和 feature；后者不替代对完整传递依赖的检查。

## 更新与排查

公共包升级需要一起更新源码 revision、Cargo 锁、调用代码、平台门禁和发行输入。Server 还需更新单个 Web tarball 的 URL 与真实 SHA-512 integrity，并重新构建整个内嵌资源快照。

出现找不到包内模块、旧子包、未匹配的 Git revision 或锁文件完整性错误时，先核对上述清单是否来自同一次发布。不能加入旧名称 alias 或跳过平台/完整性检查。最终是否可部署，以本项目当前源码的 CI 和实际发行制品验收为准；历史小包的检查结果不能代替单体重构后的验收。
