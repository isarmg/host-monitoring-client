# 依赖和 unsafe 审查

项目名称已按当前名称规范化；历史条目的版本、提交与当时验收事实保持不变。当前依赖基线另按 manifest 和 lock 中的实际输入记录。

本候选使用 Rust 1.99.0、rand 0.10、SHA-2 0.11，保留适用的 reqwest 0.13、sysinfo 0.39、Windows SDK 0.62/0.61 与 NVML 0.12。normal/build/dev/Cargo.lock 均无 rusqlite；本产品的当前 JSON 配置与有界报告队列没有引入 SQLite。xcsc 固定 1.0.0 / 00770c007912b276f5bb1075abfefe3c31026276；xcsc::log 随单体 xcsc 固定 1.0.0 / 00770c007912b276f5bb1075abfefe3c31026276。

| 保留的边界 | 必要性和约束 | 实际收敛与验证 |
|---|---|---|
| Windows SCM 与 service 回调 | 操作原生系统服务和报告状态；SDK 返回的结构必须有正确对齐、长度及所有权。 | 回调使用安全 extern 函数并包含 panic 边界；配置缓冲区最多 8 KiB，字符串指针必须位于返回缓冲区内，UTF-16 对齐、终止和编码均检查。服务命令最多 8192 个 UTF-16 单元、参数最多 128 个，LocalFree 通过 RAII 覆盖错误出口。 |
| 行政安装器 ACL、目录身份、原子日志与维护动作 | 管理员维护既有系统安装树、SCM 私有目录及凭据角色，普通 runtime 私有文件 API 不具有修复权限。 | 常规文件打开改用安全 OpenOptions/File；原生权限描述符、link count、原子替换与权限授予保留最小 SDK 调用。句柄、祖先与安全描述符均活到同步调用结束，拒 reparse、多链接和错误角色。运行期状态、锁、SID 已全部收敛到共享实现。 |
| Windows GPU ADLX / IGCL | 官方只读 SDK 提供 C ABI / vtable 与 vendor 句柄，安全 Rust 无对应系统功能。 | 固定权威头文件 ABI，只解析只读函数，固定 System32 DLL 路径并保持 DLL 生命周期；枚举、输出结构和字符串有上限，IGCL Default 改用显式字段初始化。设备异常输出映射 unknown，不当作不支持。模拟 ABI 与结构断言不能替代真实显卡验证。 |
| Windows 元数据、进程、磁盘与 SMART | 读取系统原生硬件及进程属性。 | 缓冲区先定预算再分配，检查实际返回长度、对齐和句柄生命周期；普通网络和文件业务使用安全 Rust。 |
| macOS 网络硬件及接口状态 | 公共 SystemConfiguration/CoreFoundation 与 BSD getifaddrs 无安全 Rust 标准库对应功能。 | 只读 Ethernet/IEEE80211 清单及接口标志；CF Copy 结果和 getifaddrs 分配各自 RAII 单次释放；字符串固定 UTF-8 缓冲区、枚举上限、借用不超出所属数组生命周期。原生 Mac 测试与低权限服务真实上报验证。 |
| macOS 后台显示器清单 | launchd 账户的系统报告不暴露图形会话显示器；通过公共 IOKit 只读读取连接设备的 DisplayAttributes。 | 仅在系统报告无显示器时读取型号与厂商/产品 ID；不读取序列号。CF 属性先检查类型，借用不超出拥有的快照；IO/CF 句柄 RAII 单次释放，最多 64 个条目、固定字符串缓冲区。服务账户实机验证。 |
| Linux 发行版本 section 属性 | 打包检查必须在不运行目标程序时读取固定版本字节。link_section 是 Rust 要求的 unsafe 属性契约。 | 唯一产品 section 仅存不可变 NUL 终止的版本数据，不包含可执行指令、指针或用户内容。 |
| 平台行为测试的原生调用 | 需要真实 ACL、junction、SCM 和 ABI 结果，而不是伪造权限。 | 夹具限制临时树、固定服务名和受控系统对象，重复检查拒绝后原始字节及 ACL。 |

测试的 Unix SIGTERM 已改用安全 rustix::process::kill_process 和从 Child 取得的类型化 PID，不再直接调用 libc::kill。macOS 网络采集的原生边界集中在 `collectors/macos_network.rs`，不执行系统命令或私有 API。

删除重复 state_lock 和 windows_maintenance_gate，运行期文件锁与原子操作使用 xcsc 的单一实现。普通文件 raw CreateFileW/FromRawHandle 已由标准库所有权替代；IGCL 零初始化由安全显式 Default 替代。原生函数指针和行政 SDK 调用仍需要 unsafe，不能通过移除权限检查或硬件功能取得零 unsafe。Linux 和交叉检查只证明各自覆盖范围，Windows SCM、MSI 和私有状态必须由最终 Source 的原生 CI 验证。

## 当前候选工程约束

候选准备发行；正式状态以 Git tag、最终 Source 工作流和 Release 产物为准。Rust 1.99.0 是截至 2026-10-07 的当前正式版；Tokio 选择稳定的 ~1.53.2，兼容补丁由根 Cargo.lock 锁定。unsafe function 内的原始解引用和 foreign 调用必须放进显式 unsafe 块（unsafe_op_in_unsafe_fn = deny）。这项约束检查操作边界，不替代原生 ABI、权限与生命周期验证。正式输入和用户数据身份分开记录，不通过发行号推导持久状态。

## 统一规范验收边界

| 适用条款 | 当前实现与本轮验收 | 真实限制 |
|---|---|---|
| 2–5、19：职责、目录与身份 | 单包保留 src/collectors、pairing、monitor_app、windows 等既有职责；固定产品协议 source，公共私有目录、锁、CLI 与进程预算消费 xcsc，业务报告不进入公共层。xsos-protocol 1.0.0 固定官方源 6458cb63bc1b868156ebb26eecdc09d563026df2；软件1.0.0、配置/账户1.0.0、report schema1 和 Windows 状态标签分别表达。 | Windows SDK、GPU设备和安装器属于产品必要平台差异，不能任意统一成纯 Rust。 |
| 6–9、11–14：CLI、安全与状态 | core CLI、stdin秘密、文件→环境→显式输入、初始化和运行分离；旧账户明确拒绝且 status不写文件。pair recover先只读核验 Host UUID/spool再归档账户，不把旧报告换属新实例。报告队列和SMART/SDK读取有预算。 | 旧格式配置不自动迁移；需保全旧文档、按当前配置流程重建和校验。 |
| 15：日志 | neutral typed事件、实例ID、脱敏、logs过滤及有界轮转；Windows protected 私有状态锚与精确继承的日志目录/叶分工明确。 | Windows SCM、DACL 和真实显卡行为须最终Source原生CI/设备证明。 |
| 20–23：发行和验收 | workflow固定官方SHA、runner和timeout；只有依赖验证job的tag-push publication使用contents:write；打包验证版本、Source、hash和原生安装方式。 | Mac Rust/Clippy及安装器故障模拟不代替 Linux包、MSI/SCM、Android/iOS嵌入或OTLP真实Collector路径。 |

正常运行的 0.9.3 历史接收分支已删除。当前结构测试继续拒绝缺字段、未知字段和其他格式；旧账户 status 用例核对原始字节不变并给出显式恢复命令。

Windows 服务日志新增消费共享 `WindowsLogAccess::for_service`：LocalService owner、精确 service SID 的0x1301bf、SYSTEM/Administrators FA与OWNER RIGHTS RC，拒绝宽泛服务账户数据权限。Host 仅声明自身 SCM 身份，不复制原生 ACL/日志实现。维护器和公共 CLI 查询同一 `.jsonl.1` 物理名；真实 Windows SCM/MSI 执行以本轮修复后的最终 Source CI 验收。

Windows 0.10.4/0.11.4 消费修复删除全部临时 NT/SCM token/EventLog 原生探针和额外 SDK features，恢复普通 CI 先检查/测试后包装的顺序。轮转逻辑、authenticated protected anchor 与 service inheritance 均消费共享层；产品只维护 MSI 的 reserved log owner 及事务状态合同，已有 administrative descriptor reader 用于更窄的 LocalService owner 核对，精确 ACL 仍由公共 FS 层判断。实际 SCM 小 retention 证明仅调用安全 public API；Source 77214 写入两次成功证明后完整 MSI 仍失败。该临时 helper/flag/PS/error category 已全部删除，正式无 probe Source 必须重验。

有现存安全 logs 时，维护器持有整个私有链及 logs pin，保留 stateRoot 的 protected/SYSTEM/exact4 service anchor，其他 business descendants 卸载转 protected admin-only；已有 snapshot 完整路径集校验和 parent-first restore 支持原始继承 DACL，不能因日志不 protected 省略 rollback。实际 SDK 继承传播、完整保留/重装/修复及 LocalService 操作必须由本版真实 MSI 证明，源码和 GNU cross-Clippy 不能替代。

行政 ACL setter 使用持有且无 DELETE sharing 的精确 handle 调用 SetSecurityInfo；GetSecurityDescriptorOwner/GetSecurityDescriptorDacl 仅借用唯一 SDK allocation 的指针直到同步 setter 返回。owner 与 present/non-NULL DACL 都是硬前提，Win32 错误码直接核对；随后同一持有目标回读 P/U、owner 和全部精确 ACE，API 成功不能代替物理权限生效。行政 ACL 是安装事务职责，公共 FS 层继续负责运行期权限校验；Root 持有合法 logs 时不重复写 descriptor，以免传播。
快照预检与 setter 共用带 descriptor 借用生命周期的 owner/DACL access helper；解析失败或 getter 失败都由唯一 RAII allocation 释放，完整 restore plan 在任何对象修改前拒绝 owner 缺失或 NULL DACL。有效空 ACL 不拒绝；其原生 SDK 边界测试只能由 Windows CI 实跑，GNU 编译不替代执行。
