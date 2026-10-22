//! 跑步页：恒 sportType=1；距离/配速范围 + 开始时间（随机或指定，最多前 3 天）。
//!
//! 方案与开始时间规则在 [`crate::plan`] / [`crate::start_time`]（不依赖 egui，测试用固定时刻）；
//! 本模块只保存页面状态并在调用处读一次真实时钟。绘制见 `draw`（表单与提示）和
//! `preview`（真实道路预览地图）。

mod draw;
mod preview;

use super::map::{MapLayer, MapState};
use super::{App, JobKind, UiEvent};
use crate::plan::PlanInputs;
use crate::track::generate_road::RouteMode;
use chrono::{DateTime, Local, TimeZone};

pub use crate::plan::RunPlan;

/// 今天的指定时刻尚未到达时的提示（只显示一处）。
pub(crate) const SPECIFIED_PENDING_WARNING: &str =
    "指定时刻在今天且整段跑步尚未结束，将提前到最晚可行时刻（当前时间减去用时）提交";

#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub struct PreviewInputs {
    pub uid: i64,
    pub unid: String,
    pub anchor: crate::location::Coordinate,
    pub points: Vec<(f64, f64)>,
    pub fences: Vec<Vec<(f64, f64)>>,
    pub loaded_ms: i64,
}

#[derive(Default)]
pub struct RunPage {
    pub dist_min: f32,
    pub dist_max: f32,
    pub pace_min: f32,
    pub pace_max: f32,
    /// 手动绝对海拔（米）；空白表示使用生成器默认海拔。
    pub manual_altitude: String,
    /// 0=随机时刻 1=指定时刻
    pub start_mode: usize,
    pub days_ago: i64,
    pub hour: i64,
    pub minute: i64,
    pub face_check: bool,
    /// 预计算方案：参数变更时重抽样，提交直接使用
    pub plan: Option<RunPlan>,
    /// 路线算法模式
    pub route_mode: RouteMode,
    /// 地图视图状态
    pub map: MapState,
    /// 路网预览（真实道路路由）
    pub preview: Option<crate::track::generate_road::RoadPlan>,
    /// 预览是否过期（参数变更后置真）
    pub preview_stale: bool,
    /// 路线点位与围栏（Arc：每帧按引用使用，不复制）
    pub preview_inputs: Option<std::sync::Arc<PreviewInputs>>,
    pub check: Option<route_planner::check::Check>,
    pub progress: f32,
    pub preview_key: String,
    pub preview_changed_at: Option<std::time::Instant>,
    pub preview_status: String,
    /// 地图视野是否已适配
    pub map_fitted: bool,
    /// Request whose WGS84 base map currently owns the viewport; None means BD09 preview.
    pub base_map_request: Option<u64>,
    /// 预览道路/建筑的绘制缓存（双向道路去重 + 建筑包围盒），预览更换时重建
    pub preview_layer: Option<PreviewLayer>,
}

/// 由 RoadPlan 派生的绘制数据；source 标识对应的预览，预览更换即失效。
pub struct PreviewLayer {
    source: (String, usize, usize),
    pub layer: MapLayer,
}

impl PreviewLayer {
    fn source_of(
        key: &str,
        plan: &crate::track::generate_road::RoadPlan,
    ) -> (String, usize, usize) {
        (
            key.to_string(),
            plan.edges.as_ptr() as usize,
            plan.edges.len(),
        )
    }
}

impl RunPage {
    /// 方案相关的输入（距离/配速范围 + 开始时间控件）。
    pub(crate) fn inputs(&self) -> PlanInputs {
        PlanInputs {
            dist_min: self.dist_min,
            dist_max: self.dist_max,
            pace_min: self.pace_min,
            pace_max: self.pace_max,
            start_mode: self.start_mode,
            days_ago: self.days_ago,
            hour: self.hour,
            minute: self.minute,
        }
    }

    /// 用于开始时刻上限的时长：已抽样的方案优先，否则按当前配速区间上界估一个。
    #[cfg(test)]
    pub(crate) fn dur_hint(&self) -> i64 {
        self.inputs().dur_hint(self.plan.as_ref())
    }

    /// 「换一版」：重掷运动量；随机模式下同时换一个开始时刻（指定模式的时刻不动）。
    pub fn regen_plan<Tz: TimeZone>(&mut self, now: &DateTime<Tz>) {
        self.plan = Some(self.inputs().regen(now, &mut rand::thread_rng()));
        self.preview_stale = true;
    }

    /// 确保方案与当前参数一致（参数变更补齐 / 提交前兜底）；运动量重抽时预览过期。
    pub fn ensure_plan<Tz: TimeZone>(&mut self, now: &DateTime<Tz>) {
        if self
            .inputs()
            .ensure(&mut self.plan, now, &mut rand::thread_rng())
        {
            self.preview_stale = true;
        }
    }

    /// 今天 + 指定时刻且整段跑步（含余量）尚未结束时，返回结束时刻（毫秒）。
    fn pending_specified_today<Tz: TimeZone>(&self, now: &DateTime<Tz>) -> Option<i64> {
        self.inputs()
            .pending_specified_today(self.plan.as_ref(), now)
    }

    /// 随机模式的抽样结果是否落在所选日期的 7:00-20:00 窗口内。
    fn random_window_ok<Tz: TimeZone>(&self, now: &DateTime<Tz>) -> bool {
        self.inputs().random_window_ok(self.plan.as_ref(), now)
    }

    /// 跑步页时间相关文案的下一次变化时刻：指定时刻的跑步结束、跨日（日期标签/随机窗口提示）。
    pub(crate) fn next_time_transition(&self, now_ms: i64) -> Option<i64> {
        let now = Local.timestamp_millis_opt(now_ms).single()?;
        self.inputs().next_time_transition(self.plan.as_ref(), &now)
    }
}

impl App {
    fn start_run(&mut self) {
        if self.jobs.busy(JobKind::Run) {
            return;
        }
        if self.run_page.route_mode == RouteMode::Road && self.network.is_none() {
            self.status = "请先加载路网并刷新路线点位".into();
            return;
        }
        // 参数变更时补齐方案；提交直接使用预计算值
        self.run_page.ensure_plan(&Local::now());
        let page = &mut self.run_page;
        let plan = match page.plan.clone() {
            Some(p) => p,
            None => return,
        };
        let altitude_spec = match crate::track::altitude::parse_spec(&page.manual_altitude) {
            Ok(spec) => spec,
            Err(e) => {
                self.status = e;
                return;
            }
        };
        let (manual_altitude, manual_altitude_range) = match altitude_spec {
            None => (None, None),
            Some(crate::track::altitude::AltitudeSpec::Single(value)) => (Some(value), None),
            Some(crate::track::altitude::AltitudeSpec::Range(range)) => (None, Some(range)),
        };
        let (dist, dur) = (plan.dist * 1000.0, plan.dur); // 米
        let start_ms = plan.start_ms;
        let face_check = if page.face_check { 1 } else { 0 };
        let route_mode = page.route_mode;
        self.config.dist_min = page.dist_min;
        self.config.dist_max = page.dist_max;
        self.config.pace_min = page.pace_min;
        self.config.pace_max = page.pace_max;
        self.config.face_check = page.face_check;
        self.config.manual_altitude = manual_altitude;
        self.config.manual_altitude_range = manual_altitude_range;
        self.config.route_mode = if route_mode == RouteMode::Road {
            "road".into()
        } else {
            "legacy".into()
        };
        let _ = crate::api::model::save_config(&self.config);

        let identity = self.identity.clone();
        let session = match self.session.clone() {
            Some(s) => s,
            None => {
                self.status = "请先登录".into();
                return;
            }
        };
        let ticket = self.jobs.start(JobKind::Run);
        self.status = "跑步提交中…".into();
        let seed = plan.seed;
        self.spawn_job(move |tx| {
            let mut log = App::logger(tx.clone());
            let mut client = tx.client(identity, Some(session));
            let control = ticket.control();
            let params = crate::api::flow::RunParams {
                dist,
                dur,
                start_ms,
                face_check,
                manual_altitude,
                manual_altitude_range,
                seed,
                route_mode,
            };
            let payload = match crate::api::flow::run_full_flow_cancellable(
                &mut client,
                &params,
                &mut |s| {
                    if s.starts_with('[') && control.check().is_ok() {
                        let _ = tx.send(UiEvent::RunStage(s.to_string()));
                    }
                    log(s);
                },
                control,
            ) {
                Ok(out) => {
                    log(&format!(
                        "全链完成 rrid={} obs={}/2 verify={} uuid={}",
                        out.result.rrid,
                        out.obs_ok,
                        out.verification.label(),
                        out.result.uuid
                    ));
                    serde_json::json!({
                        "ok": true, "rrid": out.result.rrid,
                        "obs_ok": out.obs_ok, "verification": out.verification,
                        "uuid": out.result.uuid,
                        "dist": out.result.total_dis, "dur": out.result.total_time,
                        "steps": out.result.total_steps, "avg_step_freq": out.result.avg_step_freq,
                        "calorie": out.result.calorie, "avg_power": out.result.avg_power,
                        "sel_distance": out.result.sel_distance, "start": out.result.start_ms,
                    })
                }
                Err(e) => {
                    log(&format!("跑步提交失败: {e}"));
                    serde_json::json!({ "ok": false, "message": crate::road_status::explain(&e) })
                }
            };
            tx.send(UiEvent::RunDone {
                ticket,
                result: payload,
            })
            .ok();
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::FixedOffset;

    fn now() -> DateTime<FixedOffset> {
        FixedOffset::east_opt(8 * 3600)
            .unwrap()
            .with_ymd_and_hms(2026, 10, 22, 12, 0, 0)
            .unwrap()
    }

    fn page(start_mode: usize, days_ago: i64) -> RunPage {
        RunPage {
            dist_min: 2.0,
            dist_max: 2.2,
            pace_min: 350.0,
            pace_max: 370.0,
            start_mode,
            days_ago,
            hour: 12,
            minute: 0,
            ..Default::default()
        }
    }

    /// 页面字段原样映射到方案输入。
    #[test]
    fn inputs_mirror_page_fields() {
        let mut p = page(1, 2);
        p.hour = 9;
        p.minute = 15;
        let i = p.inputs();
        assert_eq!(
            (i.dist_min, i.dist_max, i.pace_min, i.pace_max),
            (2.0, 2.2, 350.0, 370.0)
        );
        assert_eq!((i.start_mode, i.days_ago, i.hour, i.minute), (1, 2, 9, 15));
    }

    /// 只有运动量重抽（新方案）才让预览过期；只改时刻不重算预览。
    #[test]
    fn preview_goes_stale_only_on_new_shape() {
        let now = now();
        let mut p = page(1, 1);
        p.ensure_plan(&now);
        assert!(p.preview_stale);
        p.preview_stale = false;
        p.hour = 8;
        p.ensure_plan(&now);
        assert!(!p.preview_stale);
        assert_eq!(p.plan.as_ref().unwrap().hour, 8);
        p.regen_plan(&now);
        assert!(p.preview_stale);
        assert_eq!(p.dur_hint(), p.plan.as_ref().unwrap().dur);
    }
}
