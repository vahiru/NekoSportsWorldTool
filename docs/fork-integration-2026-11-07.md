# Fork 修复适配与验证（2026-11-07）

本地起点：`fix/audit-priority@d88bd44`。本轮按 [11 月 6 日评估](fork-cherry-pick-review-2026-11-06.md) 适配六组修复，不整体合并 fork；保留现有 CC BY-NC 4.0 许可及 YanamiNeko 署名。借鉴与改编的代码来源列于下文。

## 最新提交核对

通过远程分支引用核对了 8 个重点候选仓库，与上次调查快照比较，范围包含这些仓库公开的非默认分支：

| 仓库 | 变化 |
|---|---|
| LightWhite520/NekoSportsWorldTool | 无变化，main 仍为 `347313b` |
| GAN1112233131/NekoSportsWorldTool | 无变化，main 仍为 `555c059` |
| ConCosew/NekoSportsWorldTool | 无变化，检查点修复分支仍为 `1d8d392` |
| jie6886/NekoSportsWorldTool | 无变化，跑道路网分支仍为 `d385816` |
| abcd-2017/NekoSportsWorldTool | 无变化，main 仍为 `dc7a654` |
| Two-Brush/NekoSportsWorldTool | 无变化，main 仍为 `eaa021b` |
| Miempire/NekoSportsWorldTool | main 从 `c13e627` 到 [`d99447b`](https://github.com/Miempire/NekoSportsWorldTool/commit/d99447b4598519e4226c751d18a907708313a903)，新增 12 个提交，主要为图标、平台授权、每日限制和相关修正 |
| LazyBonesLZY/NekoSportsWorldTool | 新分支 `ios协议`，[`27de277`](https://github.com/LazyBonesLZY/NekoSportsWorldTool/commit/27de277faa0734a0cec0117131bf04e793b491c8)，涉及 GCJ 坐标、户外/自由跑与提交保护的大范围变化 |

新增内容没有改变本轮选择：平台授权和每日限制不属于本项目修复依赖；坐标、运行模式及整套协议变化仍需独立核验，没有混入以下补丁。

## 已适配的六组修复

1. **记录、违规及学期状态**
   - 参考 LightWhite520 [`e9a4ad9`](https://github.com/LightWhite520/NekoSportsWorldTool/commit/e9a4ad935a34993ef0ad2958be0a5bfebe3b6d54) 和 GAN [`555c059`](https://github.com/GAN1112233131/NekoSportsWorldTool/commit/555c059efd49389df35d96fca6e8efb9db3f1b77)。
   - 记录列表、详情及原因项保留 `Option<bool>`，手机、桌面和 CLI 缺判定字段时显示未知。详情对象类型及 rrid 校验下沉至原始详情读取，覆盖 CLI 和提交流程。
   - 违规对象只有明确的正计数才判为已标记，零计数表示此次未发现；缺失或异常计数保持未知，不猜测申诉状态的含义。
   - 学期仅合并同名且有效的距离、次数和完成度字段，取消 `semesterValid` 到有效次数、目标次数到历史总次数的赋值。明确的零值优先保留；UI 和 CLI 分别标注有效次数与历史总次数。
2. **点位解析与主动刷新**
   - 改编自 LightWhite520 `e9a4ad9`，其解析整合参考 GAN、Demacia 和 Esvion。
   - 增加明确的列表别名与 data/result 包装解析，限定层数和编码文本总量，保留原始点位与响应来源；围栏顶点不能作为检查点，多个冲突列表或区域会报错。
   - 区分明确未配置、成功空列表、业务错误及损坏响应；空结果仍停止。新任务或强制刷新重新请求，失败不返回旧点位。响应明确声明的区域若与请求不符，不更新缓存。
3. **策略跑区及点位请求契约**
   - 改编自 LightWhite520 [`347313b`](https://github.com/LightWhite520/NekoSportsWorldTool/commit/347313b4fdaac7272b9c26c3ed8a54785a4ade1c)，依据范围见其 [7.4.0 静态证据报告](https://github.com/LightWhite520/NekoSportsWorldTool/blob/347313b4fdaac7272b9c26c3ed8a54785a4ade1c/RUN_AREA_EVIDENCE_740_2026-11-05.md)。本轮没有重新反编译或验证学校实际响应。
   - 支持 runAreaModels 与策略内 geoFence，通过 fenceIds 关联围栏、以区域 id 请求点位，并拒绝无效区域。重叠区域要求选择，配置变化要求重选，正式准备仍需确认。
   - 距离使用学校策略明确的正整数 minDistance（米），同步 URL、签名及缓存作用域；runAreaId 作为 JSON 整数发送。CLI 诊断也读取学校策略。
   - 点位缓存升级为 v2；旧缓存迁移为正常过期状态，不误报损坏。原有 scope/位置/有效期隔离保留。
   - 地图读取保持独立于策略成功；中间地图结果不发布未经策略核对的校区 key，避免重复刷新清除有效确认。
4. **OSM 跑道进入查询及解析**
   - 参考 LightWhite520 [`ed9b025`](https://github.com/LightWhite520/NekoSportsWorldTool/commit/ed9b025) 的标签收紧与 [`2433a9e`](https://github.com/LightWhite520/NekoSportsWorldTool/commit/2433a9ecca87be31cd6ee16af52bb705c9173f7c) 的查询补充；没有移植其整套运动生成器。
   - 补充 leisure=track 且 sport 明确为 running/athletics 的线要素，支持 sport 分号列表。拒绝 area=yes 的跑道面，保留机动车道路排除；不同节点的近邻道路不自动连接。
   - 同时更新下载作用域版本和解析图版本。旧下载可供离线查看，但不会被当作已经包含新查询结果的在线缓存。
5. **地图手势与页面滚动**
   - 参考 LightWhite520 [`ca196b1`](https://github.com/LightWhite520/NekoSportsWorldTool/commit/ca196b111656df90f3ce6d5bd1b7115a62023a0b)，适配本地 egui 0.37。
   - 拖动以画布响应的归属判断；进入地图的外部拖动不会带动地图。地图缩放消费对应滚轮和平滑尾量，地图外页面继续滚动。
   - 对小屏额外处理：地图中心尚未进入可见范围时，允许页面继续滚动，避免地图刚露出一角就锁住页面滚动。
6. **macOS .app 更新签名**
   - 参考 LightWhite520 [`d174361`](https://github.com/LightWhite520/NekoSportsWorldTool/commit/d1743612f0f4632744ae110897c420ea6a7277a1) 指出的 bundle seal 问题，结合本地原子更新重新实现。
   - 用同盘临时副本准备整个应用包，替换内部程序并完成签名和严格验证后，使用 macOS RENAME_SWAP 原子交换目录。准备或签名失败时原包不变，旧副本位于新签名包之外并清理。
   - 适用于当前 ad-hoc 签名发布；已有开发者签名要求通过完整应用包更新，避免改写签名身份。裸二进制沿用原有更新路径。

## 验证

以下验证使用本地合成数据、临时目录、回环 HTTP 和临时 .app，没有登录业务平台、提交成绩、上传 OBS 或更新真实安装的应用包。

| 检查 | 结果 |
|---|---|
| `cargo fmt --all -- --check`、`git diff --check` | 通过 |
| `cargo test --workspace --locked --offline` | 359 passed，2 ignored，0 failed |
| `cargo test --workspace --locked --offline --no-default-features` | 283 passed，2 ignored，0 failed |
| `cargo clippy --workspace --all-targets --locked --offline -- -D warnings` | 通过 |
| `cargo clippy --workspace --all-targets --locked --offline --no-default-features -- -D warnings` | 通过 |
| Android ARM64 `cargo clippy --locked --offline --target aarch64-linux-android --features android --lib -- -D warnings` | 通过，本机 NDK 30.0.16138531，API 26 编译器 |

重点回归覆盖真实点位请求路径、签名固定向量、整数 ID、按距离隔离缓存、强制刷新及失败、响应区域不一致、旧缓存迁移、无效/重叠跑区、预览与准备区域一致、重复刷新保留确认、手机/桌面未知状态、地图拖动与滚动归属，以及 .app 成功更新和签名失败保留旧包。

测试计数按各构建的工作区单元与集成测试分别累计，不把两种构建相同的测试视作独立协议证据。Android 仅编译和静态检查，未做真机验收；离线测试不证明具体学校或客户端版本的服务端接受结果。

配速单位、坐标全局切换、外部平台授权、每日限制、自由跑和整套运动模型替换仍按前次评估暂缓。系统定位/地图选点、加密账号归档属于后续可选功能。
