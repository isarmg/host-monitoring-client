# 硬件监控扩展

host-monitor 专注硬件与系统运行状态，不枚举进程，不采集命令行、用户进程或远程任务管理数据。所有采集操作只读，不调节风扇、频率、电压或磁盘设置。

## 已实现的采集

| 数据 | Linux | Windows | macOS |
| --- | --- | --- | --- |
| CPU 型号、厂商、逐核频率、平均频率 | sysinfo | sysinfo | sysinfo，频率是否可用取决于系统 |
| CPU 硬件最高频率 | cpufreq 的 cpuinfo_max_freq | 暂不可用 | 暂不可用 |
| 1/5/15 分钟系统负载 | 有 | 不适用，报告 null | 有 |
| 网卡 MAC、IPv4/IPv6 与前缀、MTU | 有 | 有 | 有 |
| 网卡状态、协商链路速率 | sysfs | 暂不可用 | 暂不可用 |
| 风扇 RPM、电压 V、电流 A、功率 W、累计能量 J | 标准 hwmon | 暂无主板传感器提供器 | 暂无主板传感器提供器 |
| 系统热区温度 | hwmon | Windows PDH `Thermal Zone Information`（取决于固件是否暴露） | sysinfo，取决于系统 |
| SMART / NVMe 健康 | smartctl JSON | smartctl JSON | smartctl JSON，取决于设备与驱动 |
| NVIDIA 利用率、显存、温度、功耗、时钟、PCIe 流量 | NVML | NVML | 无默认支持 |
| AMD GPU 时钟 | DPM 当前级别、hwmon Hz 转 MHz | DXGI/PDH + ADLX：型号、显存、利用率、温度、功耗、时钟、风扇与电压 | 无默认支持 |
| Intel GPU 时钟 | i915、Xe tile0/gt0 的 sysfs | DXGI/PDH + IGCL：型号、显存、利用率、温度、功耗、时钟、风扇与电压 | 无默认支持 |

硬件信息按 `slow_interval_seconds` 刷新。当前频率是底层驱动暴露的读数，不保证等于每个核心瞬时有效频率；平均频率只使用有读数的核心。Xe 当前选取主 tile/GT，未聚合多个 tile。DXGI 显存采用专用显存口径，不将系统共享内存伪装成独立显存。Windows x64 已接入只读 ADLX / IGCL，GPU 遥测独立后台刷新；驱动要求、数据口径及错误处理见 [Windows 厂商遥测](windows-gpu-vendors.md)。

hwmon 的风扇原值是 RPM，电压/电流除以 1000，功率/能量除以 1000000；负电压与负电流可以有效。禁用或故障传感器不参与数值展示；非数值读数记录诊断。功率优先读取 input，仅在 input 文件不存在时使用 average。设备、通道与物理路径共同生成稳定标识，避免仅依赖容易变化的 hwmon 编号。

## SMART 配置

在客户端 JSON 配置中添加：

```json
"smart": {
  "enabled": true,
  "interval_seconds": 300,
  "executable": null
}
```

默认每五分钟采集一次，允许 60–86400 秒。Windows MSI 随包安装经过固定 SHA-256 校验的 smartmontools 7.5 独立 `smartctl.exe`、驱动数据库、GPL 许可证与完整对应源码，并优先使用该副本；它不与 Rust 客户端链接。Linux deb/rpm 声明系统 `smartmontools` 包依赖。其他自动发现位置包括 Linux `/usr/sbin/smartctl`、`/usr/bin/smartctl`，macOS `/opt/homebrew/sbin/smartctl`，Windows `C:\Program Files\smartmontools\bin\smartctl.exe`。自定义 `executable` 必须是绝对路径。程序不自动提升服务权限。设备访问权限不足会报告 `permission_denied`；工具缺失报告 `driver_missing`。

独立后台线程先执行只读设备扫描，再获取信息、健康与属性。每个子进程限时五秒、输出上限 1 MiB，一轮限时三十秒，最多 64 个设备，不阻塞常规 CPU/内存采样。使用 standby 检查，尽量避免唤醒休眠机械盘。单次 `probe`/`once` 会等待首次扫描完成或到达限时；守护进程持续采样，首次扫描完成前显示等待状态。

磁盘报告包含设备路径、型号、序列号、协议、健康检查、温度，以及 NVMe 的寿命消耗、备用空间、严重警告、通电小时、通电次数、非安全关机、介质错误和累计读写字节。ATA/SATA 使用标准 JSON 的健康、温度、通电字段，不猜测厂商 SMART 属性的寿命含义。NVMe `percentage_used` 可超过 100，不能当成“剩余健康度”。Data Units 按 512000 字节换算，溢出返回不可用，不环绕或截断。SMART 非零退出码的健康告警位仍保留有效健康数据，不把故障磁盘隐藏掉。

每块磁盘携带实际采集时间。缺失数据保持 null，不用零替代；已有低权限服务账户保持不变。现场仍需在目标硬件上验证驱动、设备权限和遥测可用性。

## 协议与构建

报告 schema 仅支持 **2**，继续使用严格字段解析。硬件位于 `system.hardware`，没有进程扩展。服务端必须同步更新；不会回退发送旧协议。明确的协议拒绝或当前错误格式的永久 400 响应会提示检查版本并停止重试该报告，不清除配对凭据。无法识别的代理错误仍按网络故障退避，避免误删凭据。

升级前积压的非当前 schema 报告保留原始字节隔离，不尝试转换，也不把版本问题累积为磁盘故障。CLI JSON、配对协议的版本号独立于报告 schema，不随此变更提升。

客户端通过固定 Git 提交依赖 Server 0.9.26 的 `host-protocol` 共享协议源码，具体提交见 `Cargo.toml` 与 `Cargo.lock`。独立克隆客户端即可构建，不需要相邻服务端目录。

接口依据：[Linux hwmon](https://docs.kernel.org/hwmon/sysfs-interface.html)、[Intel Xe 频率接口](https://www.kernel.org/doc/html/latest/gpu/xe/xe_gt_freq.html)、[smartctl 手册源码](https://github.com/smartmontools/smartmontools/blob/master/smartmontools/smartctl.8.in)。
