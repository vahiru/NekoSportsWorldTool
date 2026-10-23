//! 跑步页表单：距离/配速/海拔/开始时间控件、方案摘要与开始时间提示。

use super::SPECIFIED_PENDING_WARNING;
use crate::start_time::{RAND_HOUR_HI, RAND_HOUR_LO};
use crate::track::generate_road::RouteMode;
use crate::ui::{mobile, theme, App, JobKind};
use chrono::{DateTime, Local, TimeZone};
use eframe::egui;

/// 日期下拉的显示文本。
fn days_ago_label(days_ago: i64) -> String {
    match days_ago {
        0 => "今天".into(),
        1 => "昨天".into(),
        n => format!("{n} 天前"),
    }
}

impl App {
    pub fn draw_run(&mut self, ui: &mut egui::Ui) {
        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                self.draw_run_content(ui);
            });
    }

    fn draw_run_content(&mut self, ui: &mut egui::Ui) {
        let prev_route = self.run_page.route_mode;
        {
            let page = &mut self.run_page;
            let compact = mobile::compact_ui(ui);
            let mut draw_distance_inputs = |ui: &mut egui::Ui| {
                mobile::drag_f32(
                    ui,
                    "run_dist_min",
                    &mut page.dist_min,
                    0.5..=20.0,
                    0.05,
                    2,
                    " km",
                );
                ui.label("至");
                mobile::drag_f32(
                    ui,
                    "run_dist_max",
                    &mut page.dist_max,
                    0.5..=20.0,
                    0.05,
                    2,
                    " km",
                );
            };
            if compact {
                ui.label("距离范围（km）：");
                ui.horizontal(draw_distance_inputs);
            } else {
                ui.horizontal(|ui| {
                    ui.label("距离范围（km）：");
                    draw_distance_inputs(ui);
                });
            }
            let pace_ceiling = if page.terrain_simulation.enabled {
                1800.0
            } else {
                520.0
            };
            // Android inputs do not clamp existing values; drop slow terrain paces
            // once the mode is off so the plan is rebuilt for the flat generator.
            page.pace_min = page.pace_min.min(pace_ceiling);
            page.pace_max = page.pace_max.min(pace_ceiling);
            let mut draw_pace_inputs = |ui: &mut egui::Ui| {
                mobile::drag_f32(
                    ui,
                    "run_pace_min",
                    &mut page.pace_min,
                    180.0..=pace_ceiling,
                    5.0,
                    0,
                    "",
                );
                ui.label("至");
                mobile::drag_f32(
                    ui,
                    "run_pace_max",
                    &mut page.pace_max,
                    180.0..=pace_ceiling,
                    5.0,
                    0,
                    "",
                );
            };
            if compact {
                ui.label("配速范围（秒/km）：");
                ui.horizontal(draw_pace_inputs);
            } else {
                ui.horizontal(|ui| {
                    ui.label("配速范围（秒/km）：");
                    draw_pace_inputs(ui);
                });
            }
            mobile::row(ui, |ui| {
                ui.label("手动海拔（米/区间）：");
                mobile::text_edit(
                    ui,
                    "run_manual_altitude",
                    &mut page.manual_altitude,
                    crate::platform::InputKind::Text,
                    120.0,
                );
                ui.label("留空自动；可填 17.2 或 11.6-22.8");
            });
            ui.checkbox(&mut page.terrain_simulation.enabled, "启用山地地形仿真");
            if page.terrain_simulation.enabled {
                mobile::row(ui, |ui| {
                    ui.label("高程网格 JSON：");
                    mobile::text_edit(
                        ui,
                        "run_terrain_file",
                        &mut page.terrain_simulation.terrain_path,
                        crate::platform::InputKind::Text,
                        240.0,
                    );
                });
                mobile::row(ui, |ui| {
                    ui.label("仿真配置 JSON（可选）：");
                    mobile::text_edit(
                        ui,
                        "run_simulation_config",
                        &mut page.terrain_simulation.config_path,
                        crate::platform::InputKind::Text,
                        240.0,
                    );
                });
                ui.label("按当前距离和用时求解坡度配速；请清空手动海拔。地图显示路线几何，地形可行性在生成时校验。");
            }
            mobile::row(ui, |ui| {
                ui.label("开始时间：");
                ui.radio_value(&mut page.start_mode, 0, "随机时刻");
                ui.radio_value(&mut page.start_mode, 1, "指定时刻");
                ui.label("日期：");
                egui::ComboBox::from_id_salt("run_days_ago")
                    .width(96.0)
                    .selected_text(days_ago_label(page.days_ago))
                    .show_ui(ui, |ui| {
                        for d in 0..=3 {
                            ui.selectable_value(&mut page.days_ago, d, days_ago_label(d));
                        }
                    });
                if page.start_mode == 0 {
                    ui.label(format!("（{RAND_HOUR_LO}:00-{RAND_HOUR_HI}:00 内随机）"));
                } else {
                    ui.label("时刻：");
                    mobile::drag_i64(ui, "run_hour", &mut page.hour, 0..=23, 1.0, "", " 点");
                    ui.label(":");
                    mobile::drag_i64(ui, "run_minute", &mut page.minute, 0..=59, 1.0, "", "");
                }
            });
            mobile::row(ui, |ui| {
                ui.label("人脸校验标记：");
                ui.checkbox(&mut page.face_check, "faceCheck=1");
            });
            ui.horizontal_wrapped(|ui| {
                ui.label("路线算法：");
                ui.selectable_value(&mut page.route_mode, RouteMode::Legacy, "经典打卡点环");
                ui.selectable_value(&mut page.route_mode, RouteMode::Road, "真实道路路由");
                if page.route_mode == RouteMode::Road && self.network.is_none() {
                    ui.colored_label(theme::warn(), "（正在按学校获取，可到路网页查看）");
                }
            });
        }

        // 切换到真实道路路由时按需拉取电子围栏与实时点位（经典模式不触发这些端点）。
        if self.run_page.route_mode == RouteMode::Road && prev_route != RouteMode::Road {
            self.refresh_points();
        }

        ui.add_space(8.0);
        let fmt_pace = |s: f32| format!("{}:{:02}", (s / 60.0) as i64, (s as i64) % 60);
        let fmt_dur = |s: i64| {
            if s >= 3600 {
                format!("{}:{:02}:{:02}", s / 3600, s % 3600 / 60, s % 60)
            } else {
                format!("{}:{:02}", s / 60, s % 60)
            }
        };
        // 本帧读一次真实时钟；参数变更时补方案；显示本次提交的确定方案
        let now = Local::now();
        self.run_page.ensure_plan(&now);
        self.draw_run_warnings(ui, &now);
        let plan_label = match &self.run_page.plan {
            Some(p) => {
                let start = chrono::Local
                    .timestamp_millis_opt(p.start_ms)
                    .single()
                    .map(|t| t.format("%Y-%m-%d %H:%M").to_string())
                    .unwrap_or_default();
                let (dist, pace, dur) = (p.dist, p.pace, p.dur);
                format!(
                    "本次方案：距离 {dist:.2} km · 配速 {}/km · 用时 {} · 开始 {start}",
                    fmt_pace(pace),
                    fmt_dur(dur)
                )
            }
            None => String::new(),
        };
        mobile::row(ui, |ui| {
            ui.colored_label(theme::plain(), plan_label);
            if ui.small_button("换一版").clicked() {
                self.run_page.regen_plan(&now);
            }
        });

        self.draw_run_map(ui);

        ui.add_space(8.0);
        let run_busy = self.jobs.busy(JobKind::Run);
        if run_busy
            && ui
                .add_enabled(
                    self.jobs.cancellable(JobKind::Run),
                    egui::Button::new("取消准备/轨迹生成"),
                )
                .clicked()
            && self.jobs.cancel(JobKind::Run)
        {
            self.status = "已请求取消；等待当前请求/计算结束，取消后不会进入记录提交".into();
        }
        let enabled = !run_busy && self.session.is_some();
        let btn = if run_busy {
            theme::primary_btn("提交中…")
        } else {
            theme::primary_btn("开始跑步")
        };
        mobile::row(ui, |ui| {
            if ui.add_enabled(enabled, btn).clicked() {
                self.start_run();
            }
            if self.session.is_none() {
                ui.label("（请先登录）");
            }
        });
    }

    /// 开始时间相关的提示。须在 `ensure_plan()` 之后调用，才对得上本次提交时刻。
    fn draw_run_warnings<Tz: TimeZone>(&self, ui: &mut egui::Ui, now: &DateTime<Tz>) {
        let page = &self.run_page;
        if page.start_mode == crate::plan::MODE_RANDOM {
            if !page.random_window_ok(now) {
                ui.colored_label(
                    theme::warn(),
                    format!(
                        "所选日期的 {RAND_HOUR_LO}:00-{RAND_HOUR_HI}:00 尚未到达，本次只能贴近当前时刻",
                    ),
                );
            }
            return;
        }
        // 今天 + 指定时刻：整段跑步尚未结束时提示会提前开跑
        if page.pending_specified_today(now).is_some() {
            ui.colored_label(theme::warn(), SPECIFIED_PENDING_WARNING);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 日期下拉文本。
    #[test]
    fn days_ago_labels() {
        assert_eq!(days_ago_label(0), "今天");
        assert_eq!(days_ago_label(1), "昨天");
        assert_eq!(days_ago_label(3), "3 天前");
    }
}
