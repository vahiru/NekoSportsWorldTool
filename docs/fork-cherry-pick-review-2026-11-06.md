# Fork 改动移植评估（2026-11-06）

本地基线：`fix/audit-priority`，`d88bd44`。LightWhite520 复查基线：`347313b`。

后续执行：已在 2026-11-07 核对候选仓库更新并适配六组修复，见[实现与验证记录](fork-integration-2026-11-07.md)。下文保留 11 月 6 日评估时的状态。

结论：优先移植记录状态、点位解析、策略跑区/请求距离和跑道标签支持；地图手势与 macOS 更新签名排在其后。采用按功能拆分的源码适配，保留来源和原许可证。下面的“采用”是评估建议，本次没有合并业务代码。

## 优先采用

| 顺序 | 内容 | 来源 | 本地缺口及处理方式 | 成本 |
|---|---|---|---|---|
| 1 | 记录判定保留未知；违规零计数；学期字段不混用 | LightWhite520 `e9a4ad9` 的 records 部分；GAN `555c059` 的 records/cheat/semester 部分 | `RecordRow`、`RunDetail`、`ReasonItem` 仍把缺失判定变成 false；`self={count:0}` 仍判违规；学期代码把 `semesterValid` 作为有效次数，把 `semesterGoalCount` 写入历史总次数。拆成状态与学期字段两个补丁。 | 低至中 |
| 2 | 有界点位解析、响应上下文、主动刷新 | LightWhite520 `e9a4ad9`，整合自 GAN/Demacia/Esvion | 本地只识别顶层或 data 下的 `pointsResModels`；不支持明确的 result 包装/字段别名。增加来源路径、显式区域上下文及空列表/未配置/错误区分。正式新任务和强制刷新重新请求，普通预览保留有效缓存。 | 中 |
| 3 | 策略跑区、学校规则距离、整数区域 ID | LightWhite520 `347313b` | 当前没有 `runAreaModels`，请求路径仍固定 `/distance/1`，区域 ID 写为字符串。移植策略区域到围栏的显式关联、区域有效性、请求路径与签名、缓存距离作用域；预览、正式准备及 CLI 使用同一学校规则距离。 | 中至高 |
| 4 | OSM 跑道标签进入查询与解析 | LightWhite520 `ed9b025`、`2433a9e` 的相关部分；jie6886 `d385816` 的部分思路 | 本地下载查询和 XML 解析都只收 highway，单独标记 `leisure=track` 的跑步路径会遗漏。补齐查询与解析，保留真实共享节点拓扑和本地解析性能优化。 | 中 |
| 5 | 地图与父页面的手势归属 | LightWhite520 `ca196b1` | 本地仍读取全局鼠标状态判断拖动，滚轮也从原始事件读取；缺少画布归属及父滚动隔离。按 egui 0.37 适配；不要直接复制旧版 InputState 字段。 | 中 |
| 6 | macOS .app 更新后的签名处理 | LightWhite520 `d174361` | 本地已有 .app 发布，但自更新只原子替换内部可执行文件，没有恢复整个 bundle 的签名。保留现有原子更新、校验与失败处理，另外实现 bundle 签名及可回滚路径。 | 中 |

来源链接：

- [LightWhite520 点位、状态、地图下载集成 e9a4ad9](https://github.com/LightWhite520/NekoSportsWorldTool/commit/e9a4ad935a34993ef0ad2958be0a5bfebe3b6d54)
- [LightWhite520 跑区与距离 347313b](https://github.com/LightWhite520/NekoSportsWorldTool/commit/347313b4fdaac7272b9c26c3ed8a54785a4ade1c)
- [GAN 后续修复 555c059](https://github.com/GAN1112233131/NekoSportsWorldTool/commit/555c059efd49389df35d96fca6e8efb9db3f1b77)
- [LightWhite520 跑道标签收紧 ed9b025](https://github.com/LightWhite520/NekoSportsWorldTool/commit/ed9b025)
- [LightWhite520 跑道与运动生成 2433a9e](https://github.com/LightWhite520/NekoSportsWorldTool/commit/2433a9ecca87be31cd6ee16af52bb705c9173f7c)
- [LightWhite520 地图手势 ca196b1](https://github.com/LightWhite520/NekoSportsWorldTool/commit/ca196b111656df90f3ce6d5bd1b7115a62023a0b)
- [LightWhite520 macOS 签名 d174361](https://github.com/LightWhite520/NekoSportsWorldTool/commit/d1743612f0f4632744ae110897c420ea6a7277a1)

### 1. 状态和学期字段

本地 `Verification.complete` 已是 `Option<bool>`，但记录列表、详情模型和原因展示仍使用 bool，两套页面会对缺失字段作不同解释。只补齐 records/UI/CLI 的三态语义，保留已有“明确失败优先”的判定。

GAN 的违规修改有实质增量：本地目前只要 self 是非空对象就判违规，合成响应 `{"self":{"count":0,"appealStatus":2}}` 会触发误报。采用零计数/缺字段的处理时，不从 appealStatus 数字猜测申诉结论，仍保留服务端原始原因与时间。

学期字段应取消语义不明确的跨字段赋值。只合并明确命名的距离、次数和完成比例；汇总中的零值是否表示缺失还需要按响应来源区分，不能把所有 0 都当作待填补。

GAN 还将记录对象和 rrid 校验下沉到 `fetch_one_record`，这能覆盖 CLI 和提交流程；本地目前主要在 GUI 的 `fetch_detail` 中核对 rrid，底层仍接受非 null 的非对象值。与状态修复一同适配。

### 2. 点位解析和新鲜度

移植 LightWhite520 的 `src/api/points/response.rs` 思路，限定命名包装 `data/result`、明确列表字段、最大层数及文本体积，避免把围栏顶点识别成打卡点。保留原始 isFixed 和点位坐标，多个同层候选冲突时停止。

响应上下文只先用于校验与诊断，不从通用 id/areaId/runId 推导请求区域；也不把成功空列表直接解释为完成学期、未到开放时间或允许跳过点位。

LightWhite520 正式流程里的缓存分支还依赖其 `track_selection` 跑道确认功能，我们没有这套字段。只移植“预览可缓存，新一次正式准备或主动刷新需要新响应”的规则，继续让本地输入失效、取消和任务代次机制处理旧结果。

### 3. 跑区和点位距离

LightWhite520 附有 [Android 7.4.0 静态证据报告](https://github.com/LightWhite520/NekoSportsWorldTool/blob/347313b4fdaac7272b9c26c3ed8a54785a4ade1c/RUN_AREA_EVIDENCE_740_2026-11-05.md)：规则 minDistance 的米数进入 URL、区域 ID 为整数、区域通过 fenceIds 对应围栏。该报告比 ConCosew 的“使用用户目标距离”和未附抓包的注释更具体。本轮阅读了报告与补丁，没有重新反编译 DEX，也没有验证实际学校响应。

采用时必须一起处理：

1. `runAreaModels[].id/valid/fenceIds` 和 policy 自带 geoFence；不将围栏 id 当作区域 id。
2. 点位 URL 的距离来自学校策略，签名使用同一完整 URL；不能拿用户生成的 2400m 代替学校要求的 2000m。
3. 缓存纳入学校规则距离，区域/距离变化后旧缓存失效。
4. 我们已有版本化 `StateFile`，缓存升级应作为迁移和正常失效处理，避免把合法旧缓存报成损坏文件。
5. 有多个区域时明确选择；过期或无效区域不能自动换成另一块区域。
6. 保留本地“手动地图先加载、策略或点位失败单独展示”的行为。该提交把策略读取提前到下载之前，整块照搬会使自动地图获取依赖策略请求成功，例如非跑步时段的策略错误可能挡住地图加载。
7. 对未下发区域的旧学校保持现有无区域流程。坐标传输仍需独立核对：报告的 Android 证据不能直接证明本地 iOS 请求链的坐标契约。

### 4. OSM 跑道支持

建议采用的范围是“可识别的跑步线要素”：`leisure=track`，并明确标注 sport 为 running/athletics；支持 sport 分号列表并去空格；拒绝 area=yes 的面轮廓，显式机动车道路标签仍应优先排除。

必须同步下载查询、XML 标签保留、解析及缓存版本。只改 XML 解析，对旧自动下载缓存不会产生新跑道。我们还缓存解析后的图，因此需要同步图缓存的版本/来源校验，不能仅刷新 raw XML 的有效期。

不要整取 jie6886 `d385816` 的自动连通块缝合：距离接近不等于可以穿过围墙或建筑。也不要认为 LightWhite520 较早的 `ed9b025` 检查一定保留到其最新 main：其 `347313b` 下的 load.rs 已采用另一版更宽泛的 leisure/sport OR 判定。应从具体版本提取所需规则并加正反例。

### 5. 地图手势

本地 `MapState::interact` 仍根据指针处于地图范围且全局主键拖动来平移；缺少 `Response` 的拖动归属。LightWhite520 改为画布响应拥有拖动，并消费地图范围内的滚动，方向值得采用。

但我们已升级 egui 0.37，raw_scroll_delta 等字段与其旧版实现不同，应使用当前版本 API。LightWhite520 的 `7f958d8` 和 `481be16` 主要跟随其预览/编辑页面，不作为本地独立修复直接移植。

### 6. macOS 更新

签名问题已在本机用合成 .app 验证：签名并验证成功后，模拟我们的原子替换方式换入另一个可执行文件，`codesign --verify --deep --strict` 返回失败。

LightWhite520 的补丁使用旧版两步替换和外置旧文件路径，我们现在在 Unix 上使用原子 rename。需要适配为保留原子写入优势的签名及回滚方案，验证失败必须能恢复旧包。若后续发布改为 Developer ID 签名，应重新设计更新包策略，不能默认用 ad-hoc 签名覆盖。

## 可选功能与后续增量

| 内容 | 决定 | 原因和前置条件 |
|---|---|---|
| 系统定位 + 地图选位置 | 可作为修复后的首个新功能 | 用户不用手填经纬度，直接改善当前位置配置体验。参考 `bfd8bb9`、`fdc7709`、`e2ab81e` 及后续修复；按本地任务管理和身份确认流程重新接入。跨平台定位、坐标系、macOS 权限及签名使成本较高。 |
| 加密账号归档 | 第二批可选 | `8d24ae5` 加 `e64a64a` 有独立实现，支持限体积、固定文件名、AES-256 ZIP。导入仍直接写固定状态文件，需要接入我们的 StateFile、账号切换失效和提交回执作用域，不能绕过版本迁移。 |
| OSM 官方 map API 备用下载 | 作为现有下载器增量 | `e9a4ad9` 的备用源与 GAN 实现有价值，但本地 `b36b2a3` 已有 Overpass 双站重试和取消。保留现有能力，再增加有界备用源；不整块换成单一 Overpass 主站，也不把大范围或限流请求无限切换。 |
| 手绘路线/离线工作台 | 独立功能再评估 | Autumn984、Ghostdehole 都有实现；地图编辑、保存、坐标与轨迹约束是新子系统，不属于修复点位的必要依赖。 |
| 操场选择和逆时针 | 拆成后续几何功能 | 可以研究 LightWhite520 的轮廓确认及路线方向；其 `2433a9e` 同时替换大部分运动生成器，与本地地形/步态/转弯仿真重叠，不能整取。 |

## 暂不采用

| 来源/内容 | 取舍 |
|---|---|
| LightWhite520 `97f3a35` 的 OBS 速度/配速单位 | 暂缓。它把 lap 配速从秒改回分钟、逐点 speed 从 km/h 改成分钟/公里；我们现有代码和回归相反。双方报告都包含依据截图解释单位的限制，须用同一客户端版本的字段证据逐项核对。 |
| Miempire `c13e627` 的 iOS speed 修复 | 暂缓。该仓库自身短期内也修改过缩放解释；不能把“最新”直接等同于与本地协议链兼容。 |
| abcd-2017 `dc7a654` 整包 7.4.0 对齐 | 不直接移植。一个提交同时改版本头、提交字段及窗口 state；其中 `pace_promille = distance * 6000` 不使用窗口时长，`steps_per_min = steps * 600 / seconds` 比标准每分钟换算多 10 倍。源码注释不能替代量纲检查。可单独参考规则字段模型。 |
| ConCosew `1d8d392` 整包 | 虽可无冲突套用，仍不采用。窗口编号用时间取模不保证跨跑步单调；步数上限裁剪可能破坏总量；position 强制重写会覆盖服务端值；距离取用户目标而非学校规则。只参考距离参数线索。 |
| GAN 零步窗口重新分配 | 暂不采用。它改变派生窗口步数，而本地以最终累计样本/仿真积分为统计源；只保总数不保证逐窗口对应相同物理过程。应修生成或输入并检查一致性。 |
| Miempire 跑道环搜索性能修复 | 本地没有相同的 `track.rs` 候选搜索模块，不存在可直接套用的对应补丁。 |
| LightWhite520 大套 motion/drift/lane/altitude | 与本地 `d88bd44` 及地形仿真重叠；另起模型对照任务才有意义，不通过合并替换现有运动约束。 |
| 自由跑、365 天回溯、自动定时和天气跳过 | 都是产品行为变化，当前无需作为修复依赖引入；尤其不能借解析空点位自动切换运行模式。 |
| 全量界面改版、多账号系统 | 不是上述修复的依赖。新增状态管理和布局迁移会放大本次移植范围。 |

## 已覆盖或不值得重复搬运

- 提交拒绝/未知结果区分、重复提交保护、认证失败失效和旧任务结果隔离，本地已做了后续加固。
- 北京时间序列化已经在本地，不整体采用 cokeenet `ac98285`。
- 十秒尾窗和累计统计已有统一实现，不再套 ConCosew 的末窗缩放和步数裁剪。
- macOS 中文字体候选及 universal .app 构建已支持。Two-Brush 的整包提交不需要重复移植；桌面捆绑兜底字体可作为独立小改动评估。
- Linux GUI 自更新成 CLI 的风险已有 `check_build_compat` 主动拒绝。只有计划发布 Linux GUI 包时，才需要同步引入 GUI 资产选择和 CI；`c2507b2` 不应单独照搬。
- 本地 `b36b2a3` 已改进 Overpass 重试，不以旧 fork 的下载器覆盖它。

## 本轮验证

本轮在临时克隆内操作，基线为 `d88bd44`；当前工作分支没有 cherry-pick、reset 或业务代码修改。未读取运行时账号文件，未登录或请求校园业务接口。

### 试套补丁

对每个提交独立执行 `git cherry-pick --no-commit`，记录冲突后仅在临时克隆 reset。干净应用不表示能编译或语义正确。

| 提交 | 结果 |
|---|---|
| `347313b` | 5 个冲突文件：client、flow、model、points、路线刷新测试 |
| `e9a4ad9` | 5 个冲突文件：points、records、school_roads、online_osm、记录 UI |
| `ed9b025` | load.rs 冲突 |
| `ca196b1` | map.rs、已拆分的 run.rs 冲突 |
| `d174361` / `c2507b2` | update.rs 冲突 |
| `8d24ae5` | Cargo.lock、UI mod 冲突 |
| `1d8d392` | 干净应用；未作为推荐整包 |
| `d385816` | load.rs 冲突 |
| `e5b7b19` | fonts.rs 冲突 |
| `ac98285` | flow.rs、rank.rs、geom.rs 冲突 |
| `97f3a35` | MOTION_MODEL.md 冲突；代码语义仍需核验 |

合计 12 个候选，11 个有冲突。未列为“冲突”的位置仍可能自动合并后产生编译或语义问题。

### 离线行为复现

在临时克隆加入合成样例探针，用 `cargo test --locked --offline --no-default-features --lib` 按过滤器运行；没有运行整个 fork 测试套件：

1. 缺失达标值应保留未知：本地 `RunDetail::default()` 序列化为 false，探针失败，确认缺口。
2. `data.result.pointResModels` 应被识别：本地拒绝，探针失败，确认解析差异。
3. `self.count=0` 不应标为违规：本地得到 Flagged，探针失败，确认缺口。
4. 明确的 leisure=track + sport=running 路径应被导入：本地解析得到 0 条路，探针失败，确认缺口。
5. 将 LightWhite520 的点位解析模块单独放入临时克隆，只适配本地 `check_business` 的 owned Value 参数后运行其 5 个测试：5 passed。覆盖包装/别名、围栏排除、错误与空结果、歧义和深度/体积限制。
6. 合成 .app 在签名后验证通过；原子换入另一个可执行文件后严格签名验证失败。没有修改真实应用包或执行更新。

探针失败表示上述建议行为在当前基线上尚未实现，不是本次给工作分支引入的回归。没有对来源作者给出的完整测试数作独立背书。

## 建议落地顺序与验收

1. **状态正确性**：记录三态、记录 ID/对象校验、违规 count、学期字段；覆盖缺字段、明确 true/false、零值与字段单位，桌面/小屏/CLI 展示一致。
2. **点位获取**：先解析与新鲜度，再策略跑区/距离。回环 HTTP 检查 URL、整数 ID、签名、缓存迁移及不同距离隔离；区域配置变化后需要重选；普通地图加载保持独立。
3. **地图与平台**：跑道查询/解析/缓存，再手势和 macOS 签名。验证不自动连通近邻道路、地图拖动不带动页面、页面拖动不带动地图、更新失败可恢复旧包。
4. **新功能**：系统定位/地图选点优先，加密归档其次；分别评审，避免与上述正确性修复捆绑。

每批适配后运行对应回归和项目要求的检查；协议静态分析与离线测试通过仍不等于某所学校或客户端版本已验证。
