use super::{
    run::{PreviewInputs, RunPlan},
    App, JobKind, JobTicket,
};
use route_planner::RoadGraph;
use std::sync::{Arc, Mutex, Weak};
pub struct PreviewResult {
    pub ticket: JobTicket,
    pub key: String,
    pub check: route_planner::check::Check,
    pub plan: Result<crate::track::generate_road::RoadPlan, String>,
}
impl App {
    pub(crate) fn poll_preview_results(&mut self) {
        while let Ok(result) = self.preview_rx.try_recv() {
            // 结束本次规划；参数已变（key 不同）或已取消的结果丢弃
            if !self.jobs.accept(&result.ticket) {
                continue;
            }
            if result.key == self.run_page.preview_key && result.ticket.check().is_ok() {
                self.run_page.check = Some(result.check);
                self.run_page.map_fitted = false;
                match result.plan {
                    Ok(p) => {
                        self.run_page.preview = Some(p);
                        self.run_page.preview_status =
                            if self.run_page.preview.as_ref().unwrap().fences.is_empty() {
                                "道路预览已完成（未提供学校边界）".into()
                            } else {
                                "路线检查和预览已完成".into()
                            };
                    }
                    Err(e) => self.run_page.preview_status = crate::road_status::explain(&e),
                }
            }
        }
    }

    pub(crate) fn prepare_preview(
        &mut self,
        ui: &mut egui::Ui,
        net: Arc<route_planner::RoadGraph>,
        inputs: &PreviewInputs,
        plan: &RunPlan,
    ) {
        let key = format!(
            "{}:{}:{}:{}:{}",
            self.jobs.current_id(JobKind::RoadNetwork),
            inputs.loaded_ms,
            plan.dist,
            plan.dur,
            plan.seed
        );
        if key != self.run_page.preview_key {
            self.jobs.cancel(JobKind::Preview);
            self.run_page.preview_key = key.clone();
            self.run_page.preview_changed_at = Some(std::time::Instant::now());
            self.run_page.preview_stale = true;
            self.run_page.preview = None;
            self.run_page.check = None;
        }
        self.poll_preview_results();
        if self.jobs.busy(JobKind::Preview) {
            if ui
                .add_enabled(
                    self.jobs.cancellable(JobKind::Preview),
                    egui::Button::new("取消路线规划"),
                )
                .clicked()
            {
                self.jobs.cancel(JobKind::Preview);
                self.run_page.preview_stale = false;
                self.run_page.preview_status =
                    "已请求取消；后台规划结束前仍可能占用计算资源，其结果将丢弃".into();
            }
        } else if ui.button("重新检查并规划路线").clicked() {
            self.run_page.preview_stale = true;
            self.run_page.preview_changed_at = None;
        }
        if self.run_page.preview_stale
            && !self.jobs.busy(JobKind::Preview)
            && self
                .run_page
                .preview_changed_at
                .map(|t| t.elapsed().as_millis() >= 300)
                .unwrap_or(true)
        {
            let ticket = self.jobs.start(JobKind::Preview);
            let inputs = inputs.clone();
            let plan = plan.clone();
            let tx = self.preview_tx.clone();
            self.run_page.preview_stale = false;
            self.run_page.preview_status = if inputs.fences.is_empty() {
                "规划本地预览：检查点位与道路连通性（学校边界未校验）…".into()
            } else {
                "规划路线：裁剪围栏、检查点位和校内道路…".into()
            };
            std::thread::spawn(move || {
                // Align + clip once per network/fence set, inspect once, and plan
                // from the inspection's searches.
                let (check, result) = match ticket.check() {
                    Err(e) => (Default::default(), Err(e)),
                    Ok(()) => crate::track::generate_road::preview_prepared(
                        &prepared_network(&net, &inputs.fences),
                        &inputs.points,
                        plan.dist * 1000.,
                        plan.dur,
                        &inputs.fences,
                        || ticket.check(),
                    ),
                };
                let _ = tx.send(PreviewResult {
                    ticket,
                    key,
                    check,
                    plan: result,
                });
            });
        }
        if !self.run_page.preview_status.is_empty() {
            ui.label(&self.run_page.preview_status);
        }
        if self.jobs.busy(JobKind::Preview) || self.run_page.preview_stale {
            ui.ctx()
                .request_repaint_after(std::time::Duration::from_millis(100));
        }
    }
}

/// The last aligned + fence-clipped network; distance/duration edits reuse it.
struct Prepared {
    source: Weak<RoadGraph>,
    fences: Vec<Vec<(f64, f64)>>,
    graph: Arc<RoadGraph>,
}
static PREPARED: Mutex<Option<Prepared>> = Mutex::new(None);

fn prepared_network(net: &Arc<RoadGraph>, fences: &[Vec<(f64, f64)>]) -> Arc<RoadGraph> {
    if let Ok(cache) = PREPARED.lock() {
        if let Some(p) = cache.as_ref().filter(|p| {
            p.fences == fences && p.source.upgrade().is_some_and(|s| Arc::ptr_eq(&s, net))
        }) {
            return p.graph.clone();
        }
    }
    let graph = Arc::new(crate::track::generate_road::prepare_network(net, fences));
    if let Ok(mut cache) = PREPARED.lock() {
        *cache = Some(Prepared {
            source: Arc::downgrade(net),
            fences: fences.to_vec(),
            graph: graph.clone(),
        });
    }
    graph
}
