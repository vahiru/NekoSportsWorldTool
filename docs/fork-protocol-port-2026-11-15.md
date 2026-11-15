# Fork 协议口径移植（2026-11-15）

本地基线：`fix/audit-priority@d88bd44`（工作树含未提交的身份/跑区改动）。
来源仓库在 `docs/fork-cherry-pick-review-2026-11-06.md` 的评估之后又更新，本轮只取协议数据口径，
不合并 fork、不引入平台授权/每日限制/UI 改版。

## 来源

| 来源 | 内容 |
|---|---|
| Miempire `7fc5ebe` | Android 7.4.0 头（版本、`CustomDeviceId=""`）、提交体 `roomId/segmentJson`、`more=true`、`goalId=0`、暂停抖动、截断取整 |
| Miempire `dea2c02` / `acb6708` | OBS 11 键（`extension_json`）、`segment_json` 11 字段分段、单 objectKey、PUT `text/plain` |
| Miempire `acb6708` | 五点 `position` 0 基、`isPass` 40m 吸附（BD 缺失回退 GCJ） |
| Miempire `898a405` | 点集去掉 `steps`（27 键）、`count=步数增量`、`stepDistance=段距离/步数` |
| abcd-2017 `dc7a654` | `appVersion/UA=7.4.0`、Android `CustomDeviceId=""`、`RunRuleModel.minRunTime` → `selRunTime` |
| Miempire `5d1b457` | GT4 PoW 按 `bits` 动态难度；缺口识别固定纵向位置 |

## 改动

- `src/crypto/header.rs`：`ANDROID_APP_VERSION`/`UA_ANDROID` 升到 `7.4.0`；Android `CustomDeviceId` 改空串。
  `studentId` 保留：Miempire 删除、abcd 保留，两源冲突，未按单源改动。
- `src/api/submit.rs`：45 键提交体（补 `roomId=0`、`segmentJson=""`）；`more=true`、`goalId=0`、`themeId=-1`；
  `stopTime = start + totalTime*1000 + uuid 派生 1~3 次 1~5s 暂停`；`totalDis/validDis` 截断；
  `selRunTime = policy.minRunTime`（>0 时，否则回退 `totalTime`）；请求头 `appVersion` 用共享常量。
  `latitude/longitude` 仍发起点坐标：只有自由跑样本支持 0.0，校园跑未证实，暂不改。
- `src/api/policy.rs`：解析 `runRuleModel.minRunTime`（缺失/负值 = 0）。
- `src/track/wire.rs`：OBS 11 键 + 1000m 分段；单 objectKey；点集 27 键；`stepDistance` 取真实值；
  五点 `id=i+1`、`position=i`、`isPass` 按轨迹 40m 判定。
- `src/track/stats.rs`：`rebuild` 统一由累计步数/距离反推每点 `count` 与 `stepDistance`（夹 0.5–1.8m），
  生成器、路网与地形仿真共用。
- `src/api/obs.rs`：Put 签名与 PUT 头改 `text/plain`，Get 保持 `application/json`。
- `src/api/gt4.rs` / `gt4image.rs`：PoW 前导 0 字符数取 `pow_detail.bits`（clamp 2–16）；
  模板匹配固定纵向位置（同高取 0，否则居中），只在横向扫描。

## 未移植（保持现状）

- 圈 `laps_json[].avgPace` 单位：本地按截图证据用秒/公里，Miempire 认为 Android 为分/公里，冲突未解。
- 7.4.0 窗口 `state` 分桶打标（abcd）：其换算 `pace_promille = 10s 距离 * 6000` 与
  `steps_per_min = 步数 * 600 / 秒` 量纲不成立，未采用。
- `latitude/longitude=0.0`、`studentId` 删除、iOS 协议链、`extension_json` 真值、分段字段语义仍属未验证项；
  Miempire `session.txt` §12 同样标注 U4/U5/U8。
- RTK/自由跑/室内跑、geetoken、设备档案、路线工作台、365 天日期等属于独立功能，另立任务。

## 验证

- `cargo test --workspace --locked --offline`：354 单元 + 27 集成全部通过（1+1 ignored）。
- `cargo clippy --workspace --all-targets --locked --offline -- -D warnings`：通过。
- `cargo fmt --all`：已执行。
- 新增/更新测试覆盖：提交体 43 键（+2 签名 = 45）、固定值、截断、`selRunTime`、暂停抖动稳定性、
  OBS 11 键/分段/单 key/`extension_json`、27 键点集、五点 `position`、OBS content type、
  PoW `bits` 与固定纵向匹配、`minRunTime` 解析。

## 真机验证（2026-11-15，minghang 账号）

运行环境：`/tmp/nswt-minhang` 隔离目录（复制该账号的 `identity/session/config/points_cache` 与共享路网缓存），
不写入真实账号目录的 `submission_receipts`。

### 两次提交

| rrid | 运行时间 | 统计 | 二进制 | 结果 |
|---|---|---|---|---|
| 1365411143 | 11-15 10:57:11（3000m/1500s） | 8:20/km，170 步/分 | 旧 `target/debug`（未 rebuild） | 200，达标 |
| 1365411903 | 11-14 09:30:04（3000m/1500s） | 8:20/km，170 步/分 | 本批代码 | 200，达标 |

第一条是复制二进制时未重新构建，日志为 `10 键` + 双 key；第二条日志为
`OBS 上传（11 键）`，只 PUT 一个 `run_data/2026111409/<uuid>.json`。

### 服务端详情（`record-info --rrid 1365411903`）

```
uuid        = 3335D8E7-88F0-4835-A5A8-4DE48CD68B31
totalDis    = 3000.0        totalTime = 1500.0
startTime   = 1794619804000 stopTime  = 1794621308000   # 差 1508s，比 totalTime 多 8s
speed       = 8.333          totalSteps = 4252   avgStepFreq = 170
complete    = true
reasonList  = [{complete:true,"按顺序通过所有点位"}, …]
```

`stopTime − startTime` 多出的 8s 与本批新增的 uuid 派生暂停抖动一致，说明服务端按新提交体落库。

### OBS 回读对比（`obs-get`）

| 检查 | 新代码 rrid 1365411903 | 旧代码 rrid 1365411143 |
|---|---|---|
| 顶层键数 | **11** | 10 |
| 单点键数 | **27**（无 `steps`） | 28（含 `steps`） |
| `segment_json` | **3 段**，每段 11 字段（1000m 切分） | 空串 |
| `fixed_point_json` | 5 个点，`runAreaId=-1`、`freedomShowFence=false` | 5 个点 |
| `extension_json` / `runFaceCheck` | 空串 gzip | 无 / 空串 |

新对象键序：`rrid, uid, uuid, run_data, step_freq_json, speed_json, segment_json,
fixed_point_json, runFaceCheck, extension_json, laps_json`；laps 3 圈，首圈
1000m/502s/avgPace=502。回读字节数 30101。

证据文件：`record-info.json`、`obs-server.json`（新）、`obs-old.json`（旧），均位于运行目录。

### 限制

- 未抓包；只能证明服务端接受、落库、判达标，以及 OBS 对象可按单 key 回读，不能证明各字段的解析语义。
- 两条记录已在服务端，本地真实账号没有对应回执；GUI 的本地防重复不会识别这两个时间窗。
- 运行后修正 CLI/UI 将 OBS 分母写死为 `/2` 的问题，改用 `RunOutcome.obs_total`。

