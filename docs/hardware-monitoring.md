# 硬件监控扩展

xsoc 专注硬件与系统运行状态，不枚举进程，不采集命令行、用户进程或远程任务管理数据。所有采集操作只读，不调节风扇、频率、电压或磁盘设置。

## 已实现的采集

| 数据 | Linux | Windows | macOS |
| --- | --- | --- | --- |
| CPU 型号、厂商、逐核频率、平均频率 | sysinfo | sysinfo | sysinfo，频率是否可用取决于系统 |
| CPU 硬件最高频率 | cpufreq 的 cpuinfo_max_freq | 暂不可用 | 暂不可用 |
| 1/5/15 分钟系统负载 | 有 | 不适用，报告 null | 有 |
| 网卡 MAC、IPv4/IPv6 与前缀、MTU | 有 | 有 | 有 |
| 独立识别的物理网卡 | sysfs 设备关联；排除虚拟接口 | Windows 接口表的硬件接口标志 | 公共 SystemConfiguration 的 Ethernet / IEEE80211 接口；排除桥接及隧道 |
| 网卡状态、协商链路速率 | sysfs | 暂不可用 | BSD 接口运行标志；协商速率暂不可用 |
| 风扇 RPM、电压 V、电流 A、功率 W、累计能量 J | 标准 hwmon | 暂无主板传感器提供器 | 暂无主板传感器提供器 |
| 内存料号、厂商、DDR/LPDDR 代际、封装、容量、标称/配置速率、模块/固件版本 | SMBIOS Type 17，需允许读取 DMI | Win32_PhysicalMemory / SMBIOSMemoryType | SPMemoryDataType，保留系统提供的字段 |
| 雷电 / USB4 | Thunderbolt sysfs 路由器及可识别 PCI 控制器 | 当前 PnP 控制器名称与硬件 ID | SPThunderboltDataType |
| 显示器型号、厂商/产品 ID | 已连接 DRM 接口的 EDID | 当前 PnP 显示器 + 活动 WmiMonitorID | SPDisplaysDataType 的显示器清单 |
| 蓝牙芯片 | hci 物理设备关联与 USB/PCI ID | 当前物理 PnP 适配器，排除配对设备及枚举器 | SPBluetoothDataType 的 controller_properties |
| USB 主机控制芯片与 USB 设备 | PCI USB class 与 USB sysfs | 当前 USB PnP 清单 | macOS 26 的 SPUSBHostDataType Driver 及设备树；其他提供方的 SPUSBDataType |
| 音频控制器、声卡与设备型号 | PCI 音频 class、ALSA 声卡、HDA codec | Win32_SoundDevice、当前 MEDIA / AudioEndpoint PnP | SPAudioDataType 的 CoreAudio 设备 |
| 系统热区温度 | hwmon | Windows PDH `Thermal Zone Information`（取决于固件是否暴露） | sysinfo，取决于系统 |
| SMART / NVMe 健康 | smartctl JSON | smartctl JSON | smartctl JSON，取决于设备与驱动 |
| GPU 型号、厂商 | NVML / sysfs | NVML / DXGI | 系统报告 `system_profiler -json SPDisplaysDataType` |
| NVIDIA 利用率、显存、温度、功耗、时钟、PCIe 流量 | NVML | NVML | 无默认支持 |
| AMD GPU 时钟 | DPM 当前级别、hwmon Hz 转 MHz | DXGI/PDH + ADLX：型号、显存、利用率、温度、功耗、时钟、风扇与电压 | 无默认支持 |
| Intel GPU 时钟 | i915、Xe tile0/gt0 的 sysfs | DXGI/PDH + IGCL：型号、显存、利用率、温度、功耗、时钟、风扇与电压 | 无默认支持 |

NVML 采集仅在 Linux 和 Windows 启用。macOS 即使编译时开启 `nvidia` 特性，也会报告 `gpu.nvidia` 为 `unsupported`，不会尝试加载 Windows DLL 或将平台不支持误报为缺少驱动。

macOS 显卡清单通过系统报告的 JSON 获取，独立后台线程每五分钟刷新，单次限时十秒、标准输出上限 1 MiB，不阻塞常规采样。只读取显示硬件类别，不上传完整系统报告。优先使用 `sppci_model`，避免将 Apple 的内部 `_name` 枚举键当作型号。现有协议接收型号与厂商；GPU 核心数、Metal 版本未在协议中定义。系统报告不提供整机 GPU 实时利用率、功耗等读数，共享内存也不作为专用显存填报，这些字段保持 null。

硬件信息按 `slow_interval_seconds` 刷新。当前频率是底层驱动暴露的读数，不保证等于每个核心瞬时有效频率；平均频率只使用有读数的核心。Xe 当前选取主 tile/GT，未聚合多个 tile。DXGI 显存采用专用显存口径，不将系统共享内存伪装成独立显存。Windows x64 已接入只读 ADLX / IGCL，GPU 遥测独立后台刷新；驱动要求、数据口径及错误处理见 [Windows 厂商遥测](windows-gpu-vendors.md)。

hwmon 的风扇原值是 RPM，电压/电流除以 1000，功率/能量除以 1000000；负电压与负电流可以有效。禁用或故障传感器不参与数值展示；非数值读数记录诊断。功率优先读取 input，仅在 input 文件不存在时使用 average。设备、通道与物理路径共同生成稳定标识，避免仅依赖容易变化的 hwmon 编号。

## 内存与外围硬件清单

每条内存模块在 `system.hardware.memory_modules` 上报，其他设备在 `devices` 中按 `kind` 区分。清单采集独立后台刷新，周期使用 `slow_interval_seconds`，完成时间在 `inventory_collected_at` 中提供；首次扫描未完成时清单为空、时间为 null。`probe` / `once` 最多等待首次硬件扫描十一秒。Windows 使用系统目录下 Windows PowerShell 的本地只读 CIM 查询，macOS 使用指定类别的 `system_profiler -json`；每次查询限时十秒、输出上限 2 MiB，不执行远程查询，也不采集完整系统报告、蓝牙配对列表或音频内容。Apple Silicon 后台账户的系统报告不暴露桌面会话显示器时，使用只读 IOKit `DisplayAttributes` 补充型号与厂商/产品 ID，不采集显示器序列号。

内存型号取固件料号，代际取 DDR/LPDDR 类型，模块/固件版本仅在系统提供时填写。标称与配置速率分别使用 MT/s；不将配置速率标成实时测量值。macOS 若只提供带 MHz 等单位的标签，保存到 `reported_speed` 并原样展示，不猜测其与 MT/s 的换算。Apple Silicon 的统一内存按系统提供的容量/代际报告，不虚构独立 DIMM、料号或厂商。

Linux 读取 `/sys/firmware/dmi/tables/DMI`，权限不足时 `hardware.memory` 明确报告 `permission_denied`，不自动提权。低权限服务能识别的内存详情取决于主机对 DMI 的访问授权。USB 产品/厂商名称优先使用 sysfs 描述符；PCI/USB 芯片型号也可从本机 `/usr/share/hwdata/{pci,usb}.ids`、`/usr/share/misc/{pci,usb}.ids` 或 `/usr/share/{pci,usb}.ids` 解析。数据库没有相应名称时保留厂商/产品 ID，不把 ID 猜成商品型号。显示器 EDID 校验头部及基本块校验和，拔出的显示器不会继续留在新清单里。

各类设备分别报告能力诊断；读不到的项目不影响可读设备。内存模块最多 64 条，外围设备最多 256 条，所有文本最多 255 UTF-8 字节。每次刷新替换上一份清单。音频表示系统暴露的控制器、codec 或设备名称，不保证等于扬声器、麦克风的零售型号；雷电表示驱动识别的控制器/路由器，不推断机壳上未暴露的软件不可见接口。

接口依据：[DMTF SMBIOS 3.8 Type 17](https://www.dmtf.org/sites/default/files/standards/documents/DSP0134_3.8.0.pdf)、[Linux Thunderbolt sysfs](https://github.com/torvalds/linux/blob/master/Documentation/ABI/testing/sysfs-bus-thunderbolt)、[Win32_PhysicalMemory](https://learn.microsoft.com/en-us/windows/win32/cimwin32prov/win32-physicalmemory)、[WmiMonitorID](https://learn.microsoft.com/en-us/windows/win32/wmicoreprov/wmimonitorid)。

## SMART 配置

在客户端 JSON 配置中添加：

```json
"smart": {
  "enabled": true,
  "interval_seconds": 300,
  "executable": null
}
```

默认每五分钟采集一次，允许 60–86400 秒。Windows MSI 随包安装经过固定 SHA-256 校验的 smartmontools 7.5 独立 `smartctl.exe`、驱动数据库、GPL 许可证与完整对应源码，并优先使用该副本；它不与 Rust 客户端链接。macOS PKG 同样包含由固定 SHA-256 的 7.5 源码原生编译的独立 arm64 `smartctl`、驱动数据库、许可证、源码和复现脚本，优先使用 `/usr/local/libexec/xsoc-smartmontools/bin/smartctl`。Linux deb/rpm 声明系统 `smartmontools` 包依赖。其他自动发现位置包括 Linux `/usr/sbin/smartctl`、`/usr/bin/smartctl`，macOS `/opt/homebrew/sbin/smartctl`、`/opt/homebrew/bin/smartctl`、`/usr/local/bin/smartctl`，Windows `C:\Program Files\smartmontools\bin\smartctl.exe`。自定义 `executable` 必须是绝对路径。程序不自动提升服务权限。设备访问权限不足会报告 `permission_denied`；工具缺失报告 `driver_missing`。

独立后台线程先执行只读设备扫描，再获取信息、健康与属性。每个子进程限时五秒、输出上限 1 MiB，一轮限时三十秒，最多 64 个设备，不阻塞常规 CPU/内存采样。使用 standby 检查，尽量避免唤醒休眠机械盘。单次 `probe`/`once` 会等待首次扫描完成或到达限时；守护进程持续采样，首次扫描完成前显示等待状态。

磁盘报告包含设备路径、型号、序列号、协议、健康检查、温度，以及 NVMe 的寿命消耗、备用空间、严重警告、通电小时、通电次数、非安全关机、介质错误和累计读写字节。ATA/SATA 使用标准 JSON 的健康、温度、通电字段，不猜测厂商 SMART 属性的寿命含义。NVMe `percentage_used` 可超过 100，不能当成“剩余健康度”。Data Units 按 512000 字节换算，溢出返回不可用，不环绕或截断。SMART 非零退出码的健康告警位仍保留有效健康数据，不把故障磁盘隐藏掉。

SMART 按设备路径和 smartctl 类型选择器共同区分物理盘。扫描提供类型时，报告中的设备标识形如 `/dev/bus/0 [type=megaraid,1]`，同一 RAID 控制器的其他成员不会因路径相同而被丢弃；超出字段长度限制的组合使用稳定摘要标识。

每块磁盘携带实际采集时间。缺失数据保持 null，不用零替代；已有低权限服务账户保持不变。现场仍需在目标硬件上验证驱动、设备权限和遥测可用性。

## 协议与构建

报告结构仅支持 **1**，继续使用严格字段解析。硬件位于 `system.hardware`，没有进程扩展。服务端、客户端和 Web 必须同步更新；新增硬件清单字段为必填，不接受其他结构或缺少清单字段的报告，不转换旧数据，也不会回退发送旧协议。明确的协议拒绝或当前错误格式的永久 400 响应会提示检查版本并停止重试该报告，不清除配对凭据。无法识别的代理错误仍按网络故障退避，避免误删凭据。

客户端通过完整 Git 提交修订 `3b5c437a80424c717a20ffbad07c3e8d01cd83ae` 固定依赖服务端的 `xsos-protocol` 1.0.0，统一使用结构 1。协议源码仅由服务端的 `crates/protocol/src` 维护；独立克隆客户端即可构建，不需要相邻服务端目录。

接口依据：[Linux hwmon](https://docs.kernel.org/hwmon/sysfs-interface.html)、[Intel Xe 频率接口](https://www.kernel.org/doc/html/latest/gpu/xe/xe_gt_freq.html)、[smartctl 手册源码](https://github.com/smartmontools/smartmontools/blob/master/smartmontools/smartctl.8.in)。
