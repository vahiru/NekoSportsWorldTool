# 山地轨迹仿真与协议适配

当前版本提供独立的 `simulate` CLI：本地高程网格 → 路线空间采样 → 高程去尖峰/平滑 → 坡度配速 → 时间与步态积分 → 可选多源传感器 → 最终一致性报告。

`simulate` 不创建账号 API 客户端，不登录、不上传运动记录；指定在线高程源时会向所选服务查询路线范围的 WGS84 坐标。`run` 和 GUI 跑步页也可显式启用山地模式，经过 Adapter 接入已有提交及 OBS 链路；默认关闭，旧配置和原有生成行为保持兼容。

## 内置在线高程源

| `--elevation-source` / 配置值 | 数据与高程基准 | 凭据 |
|---|---|---|
| `open_meteo` | Open-Meteo / Copernicus DEM 2021 GLO-90，约 90m，EGM2008 | 无需密钥 |
| `opentopodata` | OpenTopoData / SRTM90m，约 90m，EGM96；SRTM 覆盖范围有限 | 无需密钥 |
| `mapbox` | Mapbox Terrain-RGB；分辨率与高程基准因地区而异，未统一基准 | 环境变量 `MAPBOX_ACCESS_TOKEN` |
| `auto` | 优先有效缓存；无缓存时按 Open-Meteo → OpenTopoData → Mapbox 顺序尝试 | 仅 Mapbox 需要 token |
| `local` | 既有本地高程 JSON | 无 |

下载覆盖路线包围框并带一个网格单元缓冲的 WGS84 网格。默认间距 90m，可通过 `--elevation-spacing 30` 修改；采样加密不提高源 DEM 的精度。Mapbox 还会考虑所选 zoom 的地面像素间距，避免低 zoom 被标为高分辨率数据。

点查询每批最多 100 个坐标；OpenTopoData 在本进程内按至少 1.1 秒间隔限速。Mapbox 同次下载复用瓦片，按官方 `-10000 + RGB整数 × 0.1` 解码。HTTP 请求有超时和 2MiB 响应限制，不自动重试突发错误，不跟随重定向。缺测、响应顺序不符或异常海拔会使整个数据源失败；`auto` 从下一个源重新下载整块，不混合 EGM96、EGM2008 等不同基准的数据。

网格缓存在应用数据目录的 `terrain_cache/`，默认有效期 30 天，缓存键包含来源、覆盖范围、间距和 Mapbox zoom。有效缓存可离线复用；损坏、过期、元数据不符时重新获取。`cache_days: 0` 强制联网刷新。缓存使用临时文件与替换，写入失败不丢弃本次下载数据。网络请求之间可取消，正在进行的请求最多等待配置的超时。

在线网格上限 1024 点，Mapbox 上限 64 个瓦片；超限时增大间距、降低 zoom 或缩小范围。公共接口有服务配额和可用性限制，请遵守提供方条款。数据署名和来源信息保留在导出网格及仿真结果中。

仅下载为原有离线格式，无需登录：

```sh
NekoSportsWorldTool terrain-fetch --route route.json \
  --elevation-source auto --output campus-grid.json
```

无需先准备网格文件的仿真：

```sh
NekoSportsWorldTool simulate --route route.json --elevation-source auto --sensors
```

使用在线高程的原有运动链路：

```sh
NekoSportsWorldTool run --route road --elevation-source auto
```

独立 `simulate` 仍要求显式指定 `--terrain` 或 `--elevation-source`，避免遗漏本地文件时意外联网。`--terrain` / `--terrain-file` 与在线来源互斥。`terrain-fetch` 不覆盖已有输出。Mapbox token 不写入配置、缓存或错误日志。

在 `terrain_simulation` 中保存在线来源：

```json
{
  "enabled": true,
  "elevation": {
    "provider": "auto",
    "grid_spacing_m": 90,
    "cache_days": 30,
    "timeout_s": 15,
    "mapbox_zoom": 14
  }
}
```

GUI 跑步页可选择这些来源；在线源无需高程路径。旧配置未声明 `elevation` 时仍使用本地文件，不会自动联网。`run` 在实际道路/闭环路线确定后获取覆盖范围，包含经典拟合曲线可能产生的外扩。

接口说明：[Open-Meteo Elevation API](https://open-meteo.com/en/docs/elevation-api)、[OpenTopoData API](https://www.opentopodata.org/api/)、[Mapbox Terrain-RGB](https://docs.mapbox.com/data/tilesets/reference/mapbox-terrain-rgb-v1/)。Copernicus/Open-Meteo 和 Mapbox 数据须按提供方要求署名；Mapbox 高程可能混合不同原始基准，不能视为测绘级统一正高。

## 在原有跑步链路中启用

GUI 跑步页勾选“启用山地地形仿真”，填写覆盖当前校园路线的高程网格 JSON 和可选仿真配置。需要清空手动海拔。启用时配速输入上限扩展至 1800 秒/公里，便于慢速爬坡；地图仍显示路线几何，地形/配速可行性在生成阶段验证。

CLI：

```sh
NekoSportsWorldTool run --route road --dist 1.4 --pace 10:00 \
  --terrain-file /path/to/campus-grid.json \
  --simulation-config /path/to/simulation-config.json
```

`run` 执行原有真实提交流程。`--terrain-file` 或 `--simulation-config` 会启用山地模式；`--terrain` 可使用已保存的路径启用，`--no-terrain` 可单次禁用已保存的模式，不能与启用参数同时使用。旧配置没有以下字段时默认关闭：

```json
{
  "terrain_simulation": {
    "enabled": false,
    "terrain_path": "/path/to/campus-grid.json",
    "config_path": "/path/to/simulation-config.json"
  }
}
```

经典模式复用拟合闭环几何，道路模式复用原来的路网、围栏和必经点规划。随后按 0.3m 容差（Douglas–Peucker）简化路线：道路规划输出约 1m 一个的平滑加密点，只保留真实拐点和折返顶点，其余交给 `spacing_m` 重新采样。再按目标距离展开路线、保留拐点、截取末段（末段不足 5cm 时补足到 5cm，总距离最多超出 5cm，避免极短末段触发 1mm 线段或 1ms 时间戳限制），整个运动学、采样和观测生成使用新的仿真管线。不会调用旧的距离重分配、位置漂移或哨兵后处理。全部点位、轨迹一致性、重复提交保护及签名验证仍沿用原链路。

每次提交的距离、整数秒时长和 seed 优先于仿真配置中的 `target_duration_s`、`seed`；山地模式强制生成传感器，以保证观测海拔可用。时长不经过旧平地最低速度钳制。不可行时长、缺测、高程覆盖不足、手动海拔冲突都会报错，不会退回原模式。

### Adapter 字段映射

| 仿真数据 | Track / 提交字段 | 单位或规则 |
|---|---|---|
| 路线坐标 | `gLat/gLng` | 内部 BD09；OBS 延用既有 BD09→GCJ02 转换 |
| `observed_height_m` | `bdA` | GNSS 观测残差沿时间插值，再叠加当前地形；米，保留 2 位小数 |
| `time_s` | `gainTimeMs`、`gainTime`、`totalTime` | 毫秒绝对时间在内部保留；日期字符串及累计时间按已有秒级协议输出 |
| `speed_mps` | `speed` / `avgSpeed` | 分别为 km/h / m/s，保留 4 位小数 |
| `cumulative_steps` | `steps`、`totalSteps` | 累计值统一四舍五入，避免逐段取整 |
| 运动积分 | 10 秒速度/步数窗口、圈统计 | 使用最终物理状态插值，末端与提交总量一致 |
| `ground_height_m` | `totalAscent`、圈 `elevationGain` | 使用无噪声地形，GNSS 抖动不增加爬升 |
| GNSS 高度序列 | `Track.simulation.sensors`、`RunOutcome.simulation` | 跑步只生成 1Hz GNSS 高度；`absolute_sensors` 提供绝对毫秒时间 |

**传感器上传边界：** 当前服务端提交体及 OBS 只有已知的位置、窗口、圈等字段，没有已确认的气压/三轴加速度序列格式。因此跑步模式不生成气压和加速度序列，只生成写入 `bdA` 的 GNSS 高度，也不会伪造 `sensor_json` 等未知字段发送，也不会自动写盘。需要确认目标协议字段/请求样例后才能增加这些序列的网络序列化。独立 `simulate --output` 仍可将完整传感器导出到本地 JSON。

Adapter 对总时长要求为整数秒，节点时间保留到毫秒。两个空间节点不足 1ms 时明确失败，避免改写时间或丢掉道路拐点。为了遵守现有协议格式，`totalTime` 取毫秒偏移的整数秒部分，`gainTime` 延用原有日期字符串格式；不能把秒级 wire 数据当作毫秒无损编码。

## 运行示例

```sh
cargo run --no-default-features -- simulate \
  --route examples/terrain/route.json \
  --terrain examples/terrain/grid.json \
  --config examples/terrain/config.json \
  --output /tmp/neko-terrain-simulation.json
```

示例高程是人工构造的山坡，不是校园实测数据。输出路径必须不存在，以免覆盖输入或已有结果。不传 `--output` 时只打印摘要；输出完整 JSON 时包含运动状态、传感器和校验结果。写盘错误可能留下不完整文件，CLI 会明确提示。

可用参数：

| 参数 | 含义 |
|---|---|
| `--route` | 必需，路线 JSON |
| `--terrain` | 必需，高程网格 JSON |
| `--config` | 可选，仿真参数 JSON，未填写字段使用默认值 |
| `--speed` | 覆盖基准水平速度，m/s |
| `--duration` | 指定总时长，秒；不可行时返回最短可行时长并退出 1 |
| `--seed` | 传感器随机种子，默认 1，可使用 0 |
| `--sensors` | 开启 GNSS 高度、气压、加速度 |
| `--output` | 完整结果的新文件路径 |

JSON 配置拒绝未知字段，CLI 参数优先于配置文件。

## 路线与地形格式

参见 [路线示例](../examples/terrain/route.json) 和 [网格示例](../examples/terrain/grid.json)。

路线必须声明 `coordinate_system`，支持 `wgs84`、`gcj02`、`bd09`。节点使用 `latitude`、`longitude`。路线按输入顺序行进，保留原始道路拐点，不自动重排或补圈。`closed: true` 要求末尾显式重复首点。拒绝连续重复点、跨日期变更线及纬度超出 ±85° 的路线。

网格原点为西南角，`heights_m` 每行从西向东、行序从南向北；`longitude_step`、`latitude_step` 单位为度。至少 2×2 个节点，`null` 表示缺测。采样使用双线性插值，超出覆盖范围或插值依赖缺测节点时明确失败；精确命中有效节点时不依赖权重为零的缺测邻居。

`source`、`vertical_datum`、`resolution_m` 必填。高程单位为米；`vertical_datum` 必须说明正高基准、椭球基准或本地基准，当前不执行大地水准面转换。数据转换时应先统一基准，不能直接混合 EGM96、EGM2008 与 GNSS 椭球高。

路线坐标在查询网格时转换到网格坐标系；GCJ-02 逆转换采用迭代近似，不用于测绘精度保证。插值后的空间采样间隔不会改变原始 DEM 分辨率。桥梁、台阶、树冠和建筑物影响需要人工/实测校准，首版不自动识别道路结构。

## 模型与默认参数

| 配置字段 | 默认值 | 说明 |
|---|---:|---|
| `spacing_m` | 5 | 最大空间采样间隔，保留所有原始拐点 |
| `smoothing_radius_m` | 15 | 高斯空间滤波半径；0 关闭；σ 为半径的一半 |
| `spike_threshold_m` | 8 | 相邻两侧反向跳变都超过该值时修正孤立尖峰 |
| `base_speed_mps` | 2.5 | 基准水平速度 |
| `max_speed_mps` | 5 | 绝对水平速度上限 |
| `max_climb_mps` | 0.3 | 上升速度约束，非普适生理极限 |
| `max_descent_mps` | 0.6 | 下降速度约束 |
| `max_acceleration_mps2` | 0.8 | 加速限制 |
| `max_braking_mps2` | 1.2 | 制动限制 |
| `max_lateral_acceleration_mps2` | 2.5 | 离散弯道速度约束参数；转弯半径下限 0.5m |
| `pace_variation` | 0.03 | 沿路线的平滑配速波动幅度（0～0.2），由 seed 决定；0 关闭 |
| `target_duration_s` | null | 可选总时长 |
| `sensors` | false | 开启传感器序列 |
| `acceleration_hz` | 50 | 加速度采样率，支持 1～200 Hz |
| `gnss_height_sigma_m` | 0.5 | GNSS 白噪声标准差，无固定 ± 边界 |
| `reference_pressure_hpa` | 1013.25 | 气压参考值 |
| `reference_height_m` | 0 | 参考气压对应的高度，须与地形使用同一基准 |
| `reference_temperature_k` | 288.15 | 参考高度处温度 |

Open-Meteo（Copernicus GLO-90）和 OpenTopoData（SRTM）均为地表模型，包含建筑与树冠高度，城区校园的坡度与爬升可能偏大；使用时会给出提示，建议用实测 GPX（`template`）核对。

地形去尖峰使用距离加权插值；平滑按弧长单元宽度加权，避免密集拐点过度影响结果。闭环采用周期扩展，首尾地形高度一致；开放路线使用截断窗口。它们都是重建估计，不保证道路真实高度。

坡度定义为 `dh / 水平弧长`。上坡期望速度指数衰减，缓下坡恢复、陡下坡减速；再叠加爬升、下降和离散弯道上限。弯道上限 `v = √(a_lat · r)`，`r = 段长 / 转角`，并设 0.5m 下限：离散曲率依赖节点间距，否则折返处的厘米级碎段会算出近乎停止的速度。基准速度另乘一组由 seed 决定的平滑波动（三条 150～600m 波长的正弦，幅度 `pace_variation`），同一位置每圈速度不同，目标时长仍精确拟合。前后向扫描满足 `v_next² ≤ v_prev² + 2*a*ds`。区间按匀加速度模型用 `dt = 2*ds/(v_prev+v_next)` 积分时间。

给定目标时长时，在上述全部约束内搜索速度缩放系数。低于最快可行时长会失败，不强行归一化距离。起点速度不超过 1 m/s，按加速度上限起跑；终点不强制减速。不包含暂停、冲刺、个体体能校准或训练负荷模型。

步态保持 `速度 = 步频 / 60 × 单步长度`。目标步频 `(140 + 9v) × (1 − 0.05·疲劳) × (1 + 0.2·上坡坡度)`，约 2.1 m/s 时 159、5 m/s 时 185 步/分；单步长度限制在 0.2～1.7m，最终校验要求步频 40～240 步/分。步数使用连续步频积分保存为浮点累计值，避免逐段取整丢失步数。慢爬坡可进入步行范围，不强制所有速度都使用跑步步频。

## 传感器与报告

传感器统一从最终运动状态采样，分别使用独立随机流；种子相同可复现。运动位置在区间内按匀加速度插值，步态相位为步频积分。

- GNSS 高度：1 Hz，真实地形高度 + 时间相关漂移 + 高斯白噪声。
- 气压：5 Hz，参考高度锚定的对流层 ISA 方程 + 0.5s 响应时间 + 漂移和噪声。
- 加速度：默认 50 Hz，理想前/左/上坐标系，包含向上的约 9.80665m/s² 重力项及步态谐波；不是手机姿态模型，也不是经人体实验标定的力学波形。

`ground_height_m` 用于运动学和爬升统计；`observed_height_m` 用于 GNSS 观测。观测噪声不会增加累计爬升，也不会反馈影响速度。

最终报告检查时间/弧长严格递增、速度积分、步频积分、步态耦合、坡度、爬升/下降与加减速约束、终点汇总、传感器时序与覆盖率、GNSS/气压残差及加速度相位。气压残差阈值当前为 0.5 hPa，GNSS 容限为 6 倍白噪声标准差加 2 米漂移余量。报告提供错误代码、索引和相对时间，最多保留 100 条错误。低采样率无法解析步态二次谐波时会提示混叠风险。

该报告验证模型内部一致性，不能证明与真实运动或真实设备完全一致。传感器通过校验后才返回成功；异常参数不会静默退回固定海拔。

## 资源边界与后续工作

空间采样上限 200000 点、平滑邻居访问上限 20000000 次、传感器上限 2000000 样本；超限明确报错，避免高频长轨迹耗尽内存。

后续可扩展更高分辨率的数据源、持久化原始瓦片缓存、实测控制点校准、GUI 剖面，以及获得协议定义后的原始传感器上传。Adapter 保留仿真来源，提交前再次校验映射；生成后修改海拔、时间、坐标或速度会被拒绝。
