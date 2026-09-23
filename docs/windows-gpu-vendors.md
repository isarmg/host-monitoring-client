# Windows AMD / Intel 只读遥测

Windows x64 客户端自动启用 ADLX / IGCL，通过本机驱动读取数据。没有远程命令、进程枚举、调频、电压设置、风扇控制、超频授权或固件操作，也没有相应配置、协议字段或服务端接口。

| 提供器 | GPU 数据 | 其他传感器 |
| --- | --- | --- |
| AMD ADLX | 利用率、显存总量/使用量、温度、整卡功耗（不可用时使用 GPU 芯片功耗）、核心/显存频率 | 热点温度、风扇 RPM、核心电压 |
| Intel IGCL | 利用率、温度、整卡平均功耗（不可用时使用 GPU 芯片平均功耗）、核心/显存频率；显存容量/使用量继续使用 DXGI/PDH | 显存温度、风扇 RPM、核心/显存电压 |

字段是否可用取决于具体显卡与驱动。传感器支持标志、返回状态、类型、单位和有限数值全部通过检查后才写入报告。零转速是有效读数；不支持的字段保持 null。Intel 功耗由相邻两次能量增量（J）除以 SDK 时间增量（s）计算；利用率同样由忙碌时间计数计算。首次采样、计数回退、重复时间或超过 120 秒的时间间隔不计算差值。频率单位为 MHz，电压为 V。

GPU 优先按 Windows LUID 与 DXGI 结果关联，不按型号或枚举序号匹配。旧 AMD 驱动若提供性能遥测但没有 `IADLXGPU2.LUID`，客户端保留一个 `amd-adlx-no-luid` 传感器条目；该条目只携带温度、功耗、时钟、风扇和电压，不重复携带利用率或显存，避免服务端汇总重复计数。成功的厂商字段补充通用采集，不可用字段保留通用来源；GPU 的 `source` 标明合并来源。风扇和电压写入现有 `system.hardware.sensors`，热点/显存温度写入 `system.temperatures`，功耗与频率写入现有 GPU 字段。服务端已有展示、存储和聚合链路直接使用这些数据，无需增加协议版本或兼容分支。

## 驱动与故障处理

- 只从 Windows System32 搜索 `amdadlx64.dll` 和 `ControlLib.dll`。不搜索工作目录，不自动下载或分发驱动/SDK，不提升权限。
- ADLX 使用 **2.0.0.125** 初始化 ABI；优先使用 `IADLXGPU2.LUID`，缺少该扩展时使用基础 `IADLXGPU.UniqueId` 建立不参与 DXGI 合并的本地传感器身份。IGCL 使用 **1.1** API、版本 **1** 的遥测结构。旧版或不匹配驱动返回包含失败阶段、数值状态码和符号名的能力状态，不调用允许不兼容驱动的初始化接口，也不尝试未知虚表布局。
- 缺少 DLL 报告 `driver_missing`；缺少接口、不支持的 API 版本报告 `unsupported`；权限错误报告 `permission_denied`；失效设备或暂时性故障报告 `transient`。诊断保留厂商错误码。
- 单个后台线程拥有 SDK 会话和原生对象，约每秒读取一次。失败初始化或会话错误后等待 60 秒再初始化；没有无界队列或反复创建线程。上报仅复制缓存，不等待驱动调用。
- 超过 120 秒的读数不再用于报告。原生驱动调用无法强制取消；若阻塞，该线程上的厂商采集会暂停，常规系统采样继续运行。
- `probe` / `once` 为首次厂商结果最多额外等待 3 秒（可与 SMART 等待重叠）。等待超时会输出能力状态；Intel 首个样本可能没有功耗，常驻运行取得第二个样本后才能计算。
- Windows ARM64 不加载这些 x64 DLL，会报告不支持。Linux 继续使用现有 sysfs/hwmon 采集。

## 原生接口范围

ADLX 只解析 `ADLXInitialize`、`ADLXTerminate` 两个导出；虚表调用仅包含接口查询/释放、GPU 列表读取、名称/显存/LUID、性能服务、能力查询和当前指标读取。未使用的虚表位置仅保留不具备调用签名的占位空间，尤其不绑定 GPU 调节或电源操作。不启动 SDK 历史跟踪，也不设置 SDK 采样参数。

IGCL 只解析 `ctlInit`、`ctlClose`、`ctlEnumerateDevices`、`ctlGetDeviceProperties`、`ctlPowerTelemetryGet`。初始化只启用 `USE_LEVEL_ZERO`，不启用固件功能。初始化和清理用于本机 SDK 会话管理。

## ABI 来源与验证

精简 Rust 绑定依据以下固定官方头文件版本：

- [AMD ADLX 32b5a740d42295c5dfe9026b9f52683da0f3af91](https://github.com/GPUOpen-LibrariesAndSDKs/ADLX/tree/32b5a740d42295c5dfe9026b9f52683da0f3af91/SDK/Include)：`ADLXVersion.h`、`ISystem.h`、`ISystem2.h`、`IPerformanceMonitoring.h`。
- [Intel IGCL b6c462933502e13d1537dd5024949a51be30e63d](https://github.com/intel/drivers.gpu.control-library/blob/b6c462933502e13d1537dd5024949a51be30e63d/include/igcl_api.h)。

将上述头文件放在同一目录，在具有 `c++` 和 `rustc` 的 64 位开发环境运行：

```sh
python3 scripts/verify-windows-gpu-abi.py /path/to/headers
cargo test --offline --all-features
cargo clippy --offline --lib --target x86_64-pc-windows-gnu -- -D warnings
```

ABI 脚本编译官方 Intel 头文件和实际 Rust 绑定，比较结构体大小与关键字段偏移，并解析官方 AMD 虚表位置与 Rust 偏移进行比较。脚本不下载文件，不参与产品运行。单元测试另行覆盖原生接口模拟、资源释放、错误恢复、计数器计算、失效缓存与多 GPU 匹配。跨平台编译及模拟测试不能代替 Windows AMD/Intel 实机验证；仍需在目标驱动上核对 `probe` 与持续采样输出。
