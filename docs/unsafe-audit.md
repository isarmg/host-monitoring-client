# 依赖和 unsafe 审查

项目名称已按当前名称规范化；历史条目的版本、提交与当时验收事实保持不变。当前依赖基线按依赖清单和锁文件中的实际输入记录。

本候选使用 Rust 1.99.0、rand 0.10、SHA-2 0.11，保留适用的 reqwest 0.13、sysinfo 0.39、Windows SDK 0.62/0.61 与 NVML 0.12。普通、构建、开发依赖和 `Cargo.lock` 均无 rusqlite；本产品的当前 JSON 配置与有界报告队列没有引入 SQLite。xcsc 固定 1.0.0 / c45e48e93e360542c2e1db6c6441a9e29b344b03；xcsc::log 随单体 xcsc 固定 1.0.0 / c45e48e93e360542c2e1db6c6441a9e29b344b03。

| 保留的边界 | 必要性和约束 | 实际收敛与验证 |
|---|---|---|
| Windows SCM 与服务回调 | 操作原生系统服务和报告状态；SDK 返回的结构必须有正确对齐、长度及所有权。 | 回调使用安全 `extern` 函数并包含 `panic` 边界；配置缓冲区最多 8 KiB，字符串指针必须位于返回缓冲区内，UTF-16 对齐、终止和编码均检查。服务命令最多 8192 个 UTF-16 单元、参数最多 128 个，LocalFree 通过 RAII 覆盖错误出口。 |
| 行政安装器 ACL、目录身份、原子日志与维护动作 | 管理员维护既有系统安装树、SCM 私有目录及凭据角色，普通运行时私有文件 API 不具有修复权限。 | 常规文件打开改用安全 OpenOptions/File；原生权限描述符、链接计数、原子替换与权限授予保留最小 SDK 调用。句柄、祖先与安全描述符均活到同步调用结束，拒绝重解析点、多链接和错误角色。运行期状态、锁、SID 已全部收敛到共享实现。 |
| Windows GPU ADLX / IGCL | 官方只读 SDK 提供 C ABI、虚函数表与厂商句柄，安全 Rust 无对应系统功能。 | 固定权威头文件 ABI，只解析只读函数，固定 System32 DLL 路径并保持 DLL 生命周期；枚举、输出结构和字符串有上限，IGCL `Default` 改用显式字段初始化。设备异常输出映射 `unknown`，不当作不支持。模拟 ABI 与结构断言不能替代真实显卡验证。 |
| Windows 元数据、进程、磁盘与 SMART | 读取系统原生硬件及进程属性。 | 缓冲区先定预算再分配，检查实际返回长度、对齐和句柄生命周期；普通网络和文件业务使用安全 Rust。 |
| macOS 网络硬件及接口状态 | 公共 SystemConfiguration/CoreFoundation 与 BSD getifaddrs 无安全 Rust 标准库对应功能。 | 只读 Ethernet/IEEE80211 清单及接口标志；CF Copy 结果和 getifaddrs 分配各自 RAII 单次释放；字符串固定 UTF-8 缓冲区、枚举上限、借用不超出所属数组生命周期。原生 Mac 测试与低权限服务真实上报验证。 |
| macOS 后台显示器清单 | launchd 账户的系统报告不暴露图形会话显示器；通过公共 IOKit 只读读取连接设备的 DisplayAttributes。 | 仅在系统报告无显示器时读取型号与厂商/产品 ID；不读取序列号。CF 属性先检查类型，借用不超出拥有的快照；IO/CF 句柄 RAII 单次释放，最多 64 个条目、固定字符串缓冲区。服务账户实机验证。 |
| Linux 发行版本段属性 | 打包检查必须在不运行目标程序时读取固定版本字节。`link_section` 是 Rust 要求的 `unsafe` 属性契约。 | 唯一产品段仅存不可变 NUL 终止的版本数据，不包含可执行指令、指针或用户内容。 |
| 平台行为测试的原生调用 | 需要真实 ACL、目录联接、SCM 和 ABI 结果，而不是伪造权限。 | 夹具限制临时树、固定服务名和受控系统对象，重复检查拒绝后原始字节及 ACL。 |

测试的 Unix SIGTERM 已改用安全 rustix::process::kill_process 和从 Child 取得的类型化 PID，不再直接调用 libc::kill。macOS 网络采集的原生边界集中在 `collectors/macos_network.rs`，不执行系统命令或私有 API。

删除重复 `state_lock` 和 `windows_maintenance_gate`，运行期文件锁与原子操作使用 xcsc 的单一实现。普通文件的原始 CreateFileW/FromRawHandle 调用已由标准库所有权替代；IGCL 零初始化由安全显式 `Default` 替代。原生函数指针和行政 SDK 调用仍需要 `unsafe`，不能通过移除权限检查或硬件功能取得零 `unsafe`。Linux 和交叉检查只证明各自覆盖范围，Windows SCM、MSI 和私有状态必须由最终源码的原生 CI 验证。

## 当前候选工程约束

候选准备发行；正式状态以 Git 标签、最终源码工作流和发行产物为准。Rust 1.99.0 是截至 2026-10-07 的当前正式版；Tokio 选择稳定的 ~1.53.2，兼容补丁由根 Cargo.lock 锁定。`unsafe` 函数内的原始解引用和外部函数调用必须放进显式 `unsafe` 块（`unsafe_op_in_unsafe_fn = deny`）。这项约束检查操作边界，不替代原生 ABI、权限与生命周期验证。正式输入和用户数据身份分开记录，不通过发行号推导持久状态。

## 统一规范验收边界

| 适用条款 | 当前实现与本轮验收 | 真实限制 |
|---|---|---|
| 2–5、19：职责、目录与身份 | 单包保留 `src/collectors`、`pairing`、`monitor_app`、`windows` 等既有职责；固定产品协议来源，公共私有目录、锁、CLI 与进程预算消费 xcsc，业务报告不进入公共层。xsos-protocol 1.0.0 固定官方源 3b5c437a80424c717a20ffbad07c3e8d01cd83ae；软件 1.0.0、配置/账户 1.0.0、报告结构 1 和 Windows 状态标签分别表达。 | Windows SDK、GPU 设备和安装器属于产品必要平台差异，不能任意统一成纯 Rust。 |
| 6–9、11–14：CLI、安全与状态 | 核心 CLI、通过标准输入读取秘密、文件→环境→显式输入、初始化和运行分离；旧账户明确拒绝且 `status` 不写文件。`pair recover` 先只读核验主机 UUID 和待发送队列再归档账户，不把旧报告换属新实例。报告队列和 SMART/SDK 读取有预算。 | 旧格式配置不自动迁移；需保全旧文档、按当前配置流程重建和校验。 |
| 15：日志 | 中立的类型化事件、实例 ID、脱敏、日志过滤及有界轮转；Windows 受保护的私有状态锚与精确继承的日志目录/叶分工明确。 | Windows SCM、DACL 和真实显卡行为须最终源码原生 CI/设备证明。 |
| 20–23：发行和验收 | 工作流固定官方 SHA、运行器和超时；只有依赖验证任务的标签推送发行使用 `contents:write`；打包验证版本、源码、摘要和原生安装方式。 | Mac Rust/Clippy 及安装器故障模拟不代替 Linux 包、MSI/SCM、Android/iOS 嵌入或 OTLP 真实 Collector 路径。 |

正常运行的 0.9.3 历史接收分支已删除。当前结构测试继续拒绝缺字段、未知字段和其他格式；旧账户 status 用例核对原始字节不变并给出显式恢复命令。

Windows 服务日志新增消费共享 `WindowsLogAccess::for_service`：LocalService 属主、精确服务 SID 的 0x1301bf、SYSTEM/Administrators FA 与 OWNER RIGHTS RC，拒绝宽泛服务账户数据权限。产品仅声明自身 SCM 身份，不复制原生 ACL/日志实现。维护器和公共 CLI 查询同一 `.jsonl.1` 物理名；真实 Windows SCM/MSI 执行以本轮修复后的最终源码 CI 验收。

Windows 0.10.4/0.11.4 消费修复删除全部临时 NT/SCM 令牌、EventLog 原生探针和额外 SDK 特性，恢复普通 CI 先检查/测试后包装的顺序。轮转逻辑、已认证的受保护锚与服务继承均消费共享层；产品只维护 MSI 的保留日志属主及事务状态合同，已有行政安全描述符读取器用于更窄的 LocalService 属主核对，精确 ACL 仍由公共文件系统层判断。实际 SCM 小规模留存证明仅调用安全的公共 API；源码 77214 写入两次成功证明后完整 MSI 仍失败。该临时辅助程序、开关、PowerShell 脚本和错误分类已全部删除，正式不带探针的源码必须重验。

有现存安全日志时，维护器持有整个私有目录链及日志目录的固定句柄，保留状态根的受保护 SYSTEM 属主与精确四项服务锚；其他业务子对象在卸载时转为受保护的仅管理员权限。已有快照完整路径集校验和先父目录后子对象的恢复支持原始继承 DACL，不能因日志不带保护标记而省略回滚。实际 SDK 继承传播、完整保留/重装/修复及 LocalService 操作必须由本版真实 MSI 证明，源码和 GNU 交叉 Clippy 检查不能替代。

行政 ACL 设置函数使用持有且不允许 DELETE 共享的精确句柄调用 SetSecurityInfo；GetSecurityDescriptorOwner/GetSecurityDescriptorDacl 仅借用唯一 SDK 分配对象的指针，直到同步设置函数返回。属主存在、DACL 存在且非 NULL 都是硬前提，Win32 错误码直接核对；随后同一持有目标回读 P/U、属主和全部精确 ACE，API 成功不能代替物理权限生效。行政 ACL 是安装事务职责，公共文件系统层继续负责运行期权限校验；管理员持有合法日志目录时不重复写安全描述符，以免传播。
快照预检与设置函数共用带安全描述符借用生命周期的属主/DACL 访问辅助函数；解析失败或读取函数失败都由唯一 RAII 分配对象释放，完整恢复计划在任何对象修改前拒绝属主缺失或 NULL DACL。有效空 ACL 不拒绝；其原生 SDK 边界测试只能由 Windows CI 实跑，GNU 编译不替代执行。
