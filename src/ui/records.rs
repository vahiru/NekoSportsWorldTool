//! 运动记录页：跑步 / AI 两个分栏，egui_extras 表格。
//!
//! 列表的显示文本在数据到达时一次算好（卡片与表格共用），表格只布局可见行。

use super::{mobile, theme, App};
use chrono::TimeZone;
use eframe::egui;
use egui_extras::{Column, TableBuilder};

const TABLE_ROW_HEIGHT: f32 = 20.0;

#[derive(Default)]
pub struct RecordsPage {
    pub rows: Vec<crate::api::records::RecordRow>,
    /// rows 的显示文本（与 rows 一一对应）
    pub run_view: Vec<RunRowView>,
    pub ai_groups: Vec<crate::api::ai::AiRecordGroup>,
    /// ai_groups 展平后的显示文本
    pub ai_view: Vec<AiRowView>,
    pub ai_total: i64,
    pub sub: usize,
    /// 当前展开详情的 rrid
    pub detail_rrid: Option<i64>,
    pub detail_raw: Option<serde_json::Value>,
    pub detail_loading: bool,
    /// AI 详情页当前记录
    pub ai_detail_id: Option<i64>,
    pub ai_detail_raw: Option<serde_json::Value>,
    pub ai_detail_loading: bool,
}

/// 跑步记录一行的显示文本。
pub struct RunRowView {
    pub rrid: i64,
    pub time: String,
    pub dist: String,
    pub dur: String,
    pub pace: String,
    pub freq: String,
    pub complete: bool,
}

/// AI 记录一行的显示文本。
pub struct AiRowView {
    pub id: i64,
    pub date: String,
    pub name: String,
    pub score: String,
    pub done_at: String,
    pub uploaded: String,
    pub has_video: bool,
}

fn fmt_local(ms: i64, pattern: &str) -> String {
    chrono::Local
        .timestamp_millis_opt(ms)
        .single()
        .map(|t| t.format(pattern).to_string())
        .unwrap_or_default()
}

fn fmt_dur(s: i64) -> String {
    if s >= 3600 {
        format!("{}:{:02}:{:02}", s / 3600, s % 3600 / 60, s % 60)
    } else {
        format!("{}:{:02}", s / 60, s % 60)
    }
}

impl RunRowView {
    fn new(r: &crate::api::records::RecordRow) -> Self {
        let pace = if r.total_dis > 0.0 {
            r.total_time as f64 / (r.total_dis / 1000.0)
        } else {
            0.0
        };
        RunRowView {
            rrid: r.rrid,
            time: fmt_local(r.start_time, "%Y-%m-%d %H:%M"),
            dist: format!("{:.2} km", r.total_dis / 1000.0),
            dur: fmt_dur(r.total_time),
            pace: format!("{}:{:02}", (pace / 60.0) as i64, (pace as i64) % 60),
            freq: format!("{} spm", r.avg_step_freq),
            complete: r.complete,
        }
    }
}

impl RecordsPage {
    pub fn set_rows(&mut self, rows: Vec<crate::api::records::RecordRow>) {
        self.run_view = rows.iter().map(RunRowView::new).collect();
        self.rows = rows;
    }

    pub fn set_ai_groups(&mut self, groups: Vec<crate::api::ai::AiRecordGroup>, total: i64) {
        self.ai_view = groups
            .iter()
            .flat_map(|g| {
                let date = fmt_local(g.score_date, "%Y-%m-%d");
                g.records.iter().map(move |r| AiRowView {
                    id: r.id,
                    date: date.clone(),
                    name: r.name.clone(),
                    // 计时类：成绩即用时（毫秒）
                    score: if r.rtype == 2 {
                        format!("{:.1} 秒", r.score.parse::<f64>().unwrap_or(0.0) / 1000.0)
                    } else {
                        format!("{} 个", r.score)
                    },
                    done_at: fmt_local(r.score_date, "%H:%M:%S"),
                    uploaded: fmt_local(r.upload_time, "%m-%d %H:%M"),
                    has_video: r.has_video,
                })
            })
            .collect();
        self.ai_groups = groups;
        self.ai_total = total;
    }
}

fn complete_label(ui: &mut egui::Ui, complete: bool) {
    if complete {
        ui.colored_label(theme::ok(), "达标");
    } else {
        ui.colored_label(theme::err(), "未达标");
    }
}

fn table_header(h: &mut egui_extras::TableRow<'_, '_>, titles: &[&str]) {
    for t in titles {
        h.col(|ui| {
            ui.label(egui::RichText::new(*t).strong().color(theme::text()));
        });
    }
}

impl App {
    pub fn draw_records(&mut self, ui: &mut egui::Ui) {
        ui.add_space(6.0);
        mobile::row(ui, |ui| {
            ui.selectable_value(&mut self.records_page.sub, 0, "跑步");
            ui.selectable_value(&mut self.records_page.sub, 1, "AI 运动");
            ui.add_space(8.0);
            let (busy, label) = match self.records_page.sub {
                0 => (self.records_busy, "刷新"),
                _ => (self.ai_records_busy, "刷新全部记录"),
            };
            if ui.add_enabled(!busy, theme::primary_btn(label)).clicked() {
                match self.records_page.sub {
                    0 => self.refresh_records(),
                    _ => self.refresh_ai_records(),
                }
            }
            if busy {
                ui.label("拉取中…");
            }
        });
        ui.separator();

        match self.records_page.sub {
            0 => self.draw_run_view(ui),
            _ => self.draw_ai_view(ui),
        }
    }

    /// AI 视图：列表与详情两个页面互斥。
    fn draw_ai_view(&mut self, ui: &mut egui::Ui) {
        if self.records_page.ai_detail_id.is_some() {
            self.draw_ai_detail_view(ui);
        } else {
            self.draw_ai_table(ui);
        }
    }

    /// AI 详情独立页：返回 + 全量字段。
    fn draw_ai_detail_view(&mut self, ui: &mut egui::Ui) {
        let id = self.records_page.ai_detail_id.unwrap_or(0);
        ui.add_space(4.0);
        mobile::row(ui, |ui| {
            if ui.button("← 返回列表").clicked() {
                self.records_page.ai_detail_id = None;
                self.records_page.ai_detail_raw = None;
                self.records_page.ai_detail_loading = false;
            }
            ui.add_space(6.0);
            ui.label(
                egui::RichText::new(format!("AI 运动详情 #{}", id))
                    .strong()
                    .color(theme::accent()),
            );
        });
        ui.separator();
        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                if self.records_page.ai_detail_loading && self.records_page.ai_detail_raw.is_none()
                {
                    ui.label("加载详情中…");
                } else if let Some(raw) = &self.records_page.ai_detail_raw {
                    if let Some(err) = raw.get("fetchError").and_then(|x| x.as_str()) {
                        ui.colored_label(theme::err(), err);
                    } else {
                        draw_ai_detail_panel(ui, raw);
                    }
                } else {
                    ui.label("（无详情数据）");
                }
            });
    }

    /// 跑步视图：列表与详情两个页面互斥。
    fn draw_run_view(&mut self, ui: &mut egui::Ui) {
        if self.records_page.detail_rrid.is_some() {
            self.draw_detail_view(ui);
        } else {
            self.draw_run_table(ui);
        }
    }

    /// 详情独立页：返回 + 全部字段。
    fn draw_detail_view(&mut self, ui: &mut egui::Ui) {
        let rrid = self.records_page.detail_rrid.unwrap_or(0);
        ui.add_space(4.0);
        mobile::row(ui, |ui| {
            if ui.button("← 返回列表").clicked() {
                self.records_page.detail_rrid = None;
                self.records_page.detail_raw = None;
                self.records_page.detail_loading = false;
            }
            ui.add_space(6.0);
            ui.label(
                egui::RichText::new(format!("跑步详情 #{}", rrid))
                    .strong()
                    .color(theme::accent()),
            );
        });
        ui.separator();
        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                if self.records_page.detail_loading && self.records_page.detail_raw.is_none() {
                    ui.label("加载详情中…");
                } else if let Some(raw) = &self.records_page.detail_raw {
                    draw_detail_panel(ui, raw);
                } else {
                    ui.label("（无详情数据）");
                }
            });
    }

    fn open_run_detail(&mut self, rrid: i64) {
        self.records_page.detail_rrid = Some(rrid);
        self.records_page.detail_raw = None;
        self.records_page.detail_loading = true;
        self.fetch_run_detail(rrid);
    }

    fn open_ai_detail(&mut self, id: i64) {
        self.records_page.ai_detail_id = Some(id);
        self.records_page.ai_detail_raw = None;
        self.records_page.ai_detail_loading = true;
        self.fetch_ai_detail(id);
    }

    fn draw_run_table(&mut self, ui: &mut egui::Ui) {
        if self.records_page.run_view.is_empty() {
            ui.add_space(20.0);
            ui.centered_and_justified(|ui| ui.label("暂无跑步记录"));
            return;
        }
        let mut open = None;
        let rows = &self.records_page.run_view;
        if mobile::compact_ui(ui) {
            egui::ScrollArea::vertical()
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    for r in rows {
                        egui::Frame::group(ui.style()).show(ui, |ui| {
                            mobile::row(ui, |ui| {
                                ui.strong(&r.time);
                                complete_label(ui, r.complete);
                            });
                            ui.label(format!("{} · {} · 配速 {}/km", r.dist, r.dur, r.pace));
                            ui.label(format!("步频 {} · rrid {}", r.freq, r.rrid));
                            if ui
                                .add_sized([96.0, mobile::TOUCH_HEIGHT], egui::Button::new("详情"))
                                .clicked()
                            {
                                open = Some(r.rrid);
                            }
                        });
                        ui.add_space(6.0);
                    }
                });
        } else {
            TableBuilder::new(ui)
                .striped(true)
                .vscroll(true)
                .auto_shrink([false, false])
                .column(Column::auto().at_least(130.0)) // 时间
                .column(Column::auto().at_least(70.0)) // 距离
                .column(Column::auto().at_least(70.0)) // 时长
                .column(Column::auto().at_least(60.0)) // 配速
                .column(Column::auto().at_least(70.0)) // 步频
                .column(Column::auto().at_least(60.0)) // 达标
                .column(Column::auto().at_least(90.0)) // rrid
                .column(Column::remainder()) // 操作
                .header(22.0, |mut h| {
                    table_header(
                        &mut h,
                        &[
                            "时间", "距离", "时长", "配速", "步频", "达标", "rrid", "操作",
                        ],
                    );
                })
                .body(|body| {
                    // 只布局可见行
                    body.rows(TABLE_ROW_HEIGHT, rows.len(), |mut row| {
                        let r = &rows[row.index()];
                        row.col(|ui| {
                            ui.label(egui::RichText::new(&r.time).color(theme::text()));
                        });
                        row.col(|ui| {
                            ui.monospace(&r.dist);
                        });
                        row.col(|ui| {
                            ui.monospace(&r.dur);
                        });
                        row.col(|ui| {
                            ui.monospace(&r.pace);
                        });
                        row.col(|ui| {
                            ui.monospace(&r.freq);
                        });
                        row.col(|ui| complete_label(ui, r.complete));
                        row.col(|ui| {
                            ui.monospace(r.rrid.to_string());
                        });
                        row.col(|ui| {
                            let txt = egui::RichText::new("详情").color(theme::accent());
                            if ui.small_button(txt).clicked() {
                                open = Some(r.rrid);
                            }
                        });
                    });
                });
        }
        if let Some(rrid) = open {
            self.open_run_detail(rrid);
        }
    }

    fn draw_ai_table(&mut self, ui: &mut egui::Ui) {
        if self.records_page.ai_groups.is_empty() {
            ui.add_space(20.0);
            ui.centered_and_justified(|ui| ui.label("暂无 AI 记录，选择项目后「刷新记录」"));
            return;
        }
        ui.label(format!(
            "共 {} 条（{} 天）",
            self.records_page.ai_total,
            self.records_page.ai_groups.len()
        ));
        let mut open = None;
        let rows = &self.records_page.ai_view;
        let video = |has_video: bool| if has_video { "有" } else { "-" };
        if mobile::compact_ui(ui) {
            egui::ScrollArea::vertical()
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    for r in rows {
                        egui::Frame::group(ui.style()).show(ui, |ui| {
                            ui.strong(format!("{} · {}", r.date, r.name));
                            ui.label(format!("成绩 {} · 视频 {}", r.score, video(r.has_video)));
                            ui.label(format!("提交 {}", r.uploaded));
                            if ui
                                .add_sized([96.0, mobile::TOUCH_HEIGHT], egui::Button::new("详情"))
                                .clicked()
                            {
                                open = Some(r.id);
                            }
                        });
                        ui.add_space(6.0);
                    }
                });
        } else {
            TableBuilder::new(ui)
                .striped(true)
                .vscroll(true)
                .auto_shrink([false, false])
                .column(Column::auto().at_least(100.0)) // 日期
                .column(Column::auto().at_least(110.0)) // 项目
                .column(Column::auto().at_least(90.0)) // 成绩
                .column(Column::auto().at_least(100.0)) // 完成时间
                .column(Column::auto().at_least(90.0)) // 提交时间
                .column(Column::auto().at_least(60.0)) // 视频
                .column(Column::remainder()) // 操作
                .header(22.0, |mut h| {
                    table_header(
                        &mut h,
                        &[
                            "日期",
                            "项目",
                            "成绩",
                            "完成时间",
                            "提交时间",
                            "视频",
                            "操作",
                        ],
                    );
                })
                .body(|body| {
                    body.rows(TABLE_ROW_HEIGHT, rows.len(), |mut row| {
                        let r = &rows[row.index()];
                        row.col(|ui| {
                            ui.monospace(&r.date);
                        });
                        row.col(|ui| {
                            ui.label(&r.name);
                        });
                        row.col(|ui| {
                            ui.monospace(&r.score);
                        });
                        row.col(|ui| {
                            ui.monospace(&r.done_at);
                        });
                        row.col(|ui| {
                            ui.monospace(&r.uploaded);
                        });
                        row.col(|ui| {
                            if r.has_video {
                                ui.colored_label(theme::accent(), "有");
                            } else {
                                ui.label("-");
                            }
                        });
                        row.col(|ui| {
                            if ui.small_button("详情").clicked() {
                                open = Some(r.id);
                            }
                        });
                    });
                });
        }
        if let Some(id) = open {
            self.open_ai_detail(id);
        }
    }
}

/// AI 记录详情：record/info 全量字段。
fn draw_ai_detail_panel(ui: &mut egui::Ui, raw: &serde_json::Value) {
    let i = |k: &str| raw.get(k).and_then(|v| v.as_i64()).unwrap_or(0);
    let f = |k: &str| raw.get(k).and_then(|v| v.as_f64()).unwrap_or(0.0);
    let s = |k: &str| -> String {
        raw.get(k)
            .and_then(|v| v.as_str())
            .filter(|x| !x.is_empty())
            .map(|x| x.to_string())
            .unwrap_or_else(|| "-".into())
    };
    let dt = |k: &str| -> String {
        let ms = i(k);
        if ms <= 0 {
            return "-".into();
        }
        chrono::Local
            .timestamp_millis_opt(ms)
            .single()
            .map(|t| t.format("%Y-%m-%d %H:%M:%S").to_string())
            .unwrap_or_else(|| "-".into())
    };
    let fmt_ms = |ms: i64| -> String {
        if ms <= 0 {
            return "-".into();
        }
        if ms >= 60_000 {
            format!(
                "{}:{:02}:{:02}",
                ms / 60_000,
                ms % 60_000 / 1000,
                ms % 1000 / 10
            )
        } else {
            format!("{}.{:02} 秒", ms / 1000, ms % 1000 / 10)
        }
    };

    let rtype = i("type");
    // 详情接口的 score 单位与列表不同（计时类为计分制），展示原始值，用时以 timeConsume 为准
    let score_text = if rtype == 2 {
        format!("{}（原始计分）", i("score"))
    } else {
        format!("{} 个", i("score"))
    };

    let mut rows: Vec<(&str, String)> = vec![
        ("项目名称", s("name")),
        (
            "类型",
            if rtype == 2 {
                "计时".into()
            } else {
                "计次".into()
            },
        ),
        ("成绩", score_text),
        ("用时", fmt_ms(i("timeConsume"))),
        ("速度", {
            let sp = f("speed");
            if sp > 0.0 {
                format!("{sp:.0} /分")
            } else {
                "-".into()
            }
        }),
        ("消耗", {
            let c = f("consume");
            if c > 0.0 {
                format!("{c:.1} kcal")
            } else {
                "0 kcal".into()
            }
        }),
        ("完成时间", dt("scoreDate")),
        ("提交时间", dt("uploadTime")),
        ("状态", i("status").to_string()),
        ("记录 ID", i("id").to_string()),
        ("项目 ID", i("sportId").to_string()),
        ("任务 ID", i("taskId").to_string()),
        ("用户 ID", i("uid").to_string()),
        ("记录 UUID", s("uuid")),
        ("视频", s("exerciseMediaUrl")),
    ];
    if let Some(reason) = raw.get("reason").and_then(|v| v.as_str()) {
        if !reason.is_empty() {
            rows.push(("原因", reason.to_string()));
        }
    }
    mobile::kv_list(
        ui,
        "ai_detail_grid",
        4.0,
        rows.iter().map(|(k, v)| (*k, v.as_str())),
        |ui, k, v| {
            mobile::row(ui, |ui| {
                ui.label(egui::RichText::new(v).color(theme::text()));
                let copyable = (k == "视频" && v != "-") || k == "记录 UUID";
                if copyable && ui.button("复制").clicked() {
                    ui.ctx().copy_text(v.to_string());
                }
            });
        },
    );
}

fn draw_detail_panel(ui: &mut egui::Ui, raw: &serde_json::Value) {
    let g = |k: &str| -> String {
        match raw.get(k) {
            Some(serde_json::Value::String(s)) if !s.is_empty() => s.clone(),
            Some(serde_json::Value::Number(n)) => n.to_string(),
            _ => String::new(),
        }
    };
    let f = |k: &str| -> f64 { raw.get(k).and_then(|v| v.as_f64()).unwrap_or(0.0) };
    let i = |k: &str| -> i64 { raw.get(k).and_then(|v| v.as_i64()).unwrap_or(0) };

    let mut items: Vec<(&str, String)> = vec![
        ("距离", format!("{:.2} km", f("totalDis") / 1000.0)),
        ("有效里程", format!("{:.2} km", f("validDis") / 1000.0)),
        ("时长", {
            let t = i("totalTime");
            if t >= 3600 {
                format!("{}:{:02}:{:02}", t / 3600, t % 3600 / 60, t % 60)
            } else {
                format!("{}:{:02}", t / 60, t % 60)
            }
        }),
        ("卡路里", format!("{} kcal", i("calorie"))),
        ("功率", format!("{} W", i("avgPower"))),
        ("步数", i("totalSteps").to_string()),
        ("步频", format!("{} spm", i("avgStepFreq"))),
        ("爬升", format!("{} m", i("totalAscent"))),
        ("配速区间", {
            let lo = f("speedBottom");
            let hi = f("speedTop");
            if hi > 0.0 {
                format!("{:.2} ~ {:.2} m/s", lo, hi)
            } else {
                "-".into()
            }
        }),
        ("地址", g("address")),
        ("状态", g("statusInfo")),
    ];
    let st_ms = i("startTime");
    if st_ms > 0 {
        let st = chrono::Local
            .timestamp_millis_opt(st_ms)
            .single()
            .map(|t| t.format("%Y-%m-%d %H:%M:%S").to_string())
            .unwrap_or_default();
        if !st.is_empty() {
            items.push(("开始时间", st));
        }
    }
    mobile::kv_list(
        ui,
        "detail_grid",
        4.0,
        items
            .iter()
            .filter(|(_, v)| !(v.is_empty() || v == "0" || v == "0.0"))
            .map(|(k, v)| (*k, v.as_str())),
        |ui, _, v| {
            ui.label(egui::RichText::new(v).color(theme::text()));
        },
    );

    // 达标判定
    if let Some(list) = raw.get("reasonList").and_then(|x| x.as_array()) {
        ui.add_space(6.0);
        ui.label(
            egui::RichText::new("达标判定")
                .strong()
                .color(theme::text()),
        );
        for r in list {
            let ok = r.get("complete").and_then(|x| x.as_bool()).unwrap_or(false);
            let reason = r.get("reason").and_then(|x| x.as_str()).unwrap_or("");
            mobile::row(ui, |ui| {
                if ok {
                    ui.colored_label(theme::ok(), "√");
                } else {
                    ui.colored_label(theme::err(), "×");
                }
                ui.label(reason);
            });
        }
    }
}
