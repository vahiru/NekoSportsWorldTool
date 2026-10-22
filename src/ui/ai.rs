//! AI 运动页：多项目选择 / 按分钟与按次 / 单次提交 / 批量补签。

use super::{mobile, theme, App, JobKind, UiEvent};
use crate::api::ai::AiMode;
use eframe::egui;
use std::time::Duration;

#[derive(Default)]
pub struct AiPage {
    pub list: Vec<crate::api::ai::AiSport>,
    pub selected: usize,
    /// 0=按分钟 1=按次
    pub mode: usize,
    /// 批量补签：天数（1-60）与每天次数
    pub days: i64,
    pub per_day: i64,
    /// 勾选的项目 id
    pub picked: Vec<i64>,
    /// 单发项目下拉的显示文本（随列表更新，不每帧重建）
    pub names: Vec<String>,
}

impl AiPage {
    pub fn set_list(&mut self, list: Vec<crate::api::ai::AiSport>) {
        self.names = list
            .iter()
            .map(|s| format!("id={}  {}", s.id, s.name))
            .collect();
        self.list = list;
    }
}

/// 批量计划（确认后执行）。
#[derive(Clone)]
pub struct AiBatchPlan {
    pub sport_ids: Vec<i64>,
    pub days: i64,
    pub per_day: i64,
    pub mode: AiMode,
}

impl App {
    pub fn draw_ai(&mut self, ui: &mut egui::Ui) {
        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(ui, |ui| self.draw_ai_content(ui));
    }

    fn draw_ai_content(&mut self, ui: &mut egui::Ui) {
        ui.add_space(6.0);

        // 项目多选
        ui.label("项目（可多选）：");
        egui::ScrollArea::horizontal().show(ui, |ui| {
            ui.horizontal(|ui| {
                for s in &self.ai_page.list {
                    let picked = self.ai_page.picked.contains(&s.id);
                    let response = if mobile::compact_ui(ui) {
                        ui.add_sized(
                            [96.0, mobile::TOUCH_HEIGHT],
                            egui::SelectableLabel::new(picked, &s.name),
                        )
                    } else {
                        ui.selectable_label(picked, &s.name)
                    };
                    if response.clicked() {
                        if picked {
                            self.ai_page.picked.retain(|&x| x != s.id);
                        } else {
                            self.ai_page.picked.push(s.id);
                        }
                    }
                }
            });
        });

        ui.add_space(6.0);
        mobile::row(ui, |ui| {
            ui.radio_value(&mut self.ai_page.mode, 0, "按分钟");
            ui.radio_value(&mut self.ai_page.mode, 1, "按次");
        });

        egui::Grid::new("ai_grid")
            .num_columns(2)
            .spacing([10.0, 6.0])
            .show(ui, |ui| {
                if self.ai_page.mode == 0 {
                    ui.label("时长（分钟）：");
                    mobile::drag_i64(
                        ui,
                        "ai_minutes",
                        &mut self.config.ai_minutes,
                        1..=30,
                        1.0,
                        "",
                        " 分钟",
                    );
                    ui.end_row();
                } else {
                    ui.label("个数：");
                    mobile::drag_i64(
                        ui,
                        "ai_reps",
                        &mut self.config.ai_reps,
                        5..=1000,
                        5.0,
                        "",
                        " 个",
                    );
                    ui.end_row();
                }
                ui.label("补签天数（含今天）：");
                mobile::drag_i64(
                    ui,
                    "ai_days",
                    &mut self.ai_page.days,
                    1..=60,
                    1.0,
                    "",
                    " 天",
                );
                ui.end_row();
                ui.label("每天次数：");
                mobile::drag_i64(
                    ui,
                    "ai_per_day",
                    &mut self.ai_page.per_day,
                    1..=10,
                    1.0,
                    "",
                    "",
                );
                ui.end_row();
            });

        ui.add_space(8.0);
        let logged = self.session.is_some();
        let ai_busy = self.jobs.busy(JobKind::AiSubmit);
        let list_busy = self.jobs.busy(JobKind::AiList);
        let one_enabled = !ai_busy && !list_busy && logged && !self.ai_page.list.is_empty();
        mobile::row(ui, |ui| {
            if ui
                .add_enabled(one_enabled, theme::primary_btn("提交成绩"))
                .clicked()
            {
                let sport_id = self
                    .ai_page
                    .list
                    .get(self.ai_page.selected)
                    .map(|s| s.id)
                    .unwrap_or(0);
                self.submit_ai_once(sport_id);
            }
            if ai_busy {
                ui.label("提交中…");
            } else if list_busy {
                ui.label("项目列表拉取中…");
            }
        });

        self.draw_single_selector(ui);

        // ── 批量补签 ────────────────────────────────────────────
        ui.add_space(10.0);
        ui.separator();
        ui.label("批量补签（单线程顺序提交，每天随机时刻）");
        if let Some(batch) = &self.ai_batch {
            mobile::row(ui, |ui| {
                ui.label(format!("批量补签进行中：{}/{} 条", batch.done, batch.total));
                if ui
                    .add_enabled(
                        self.jobs.cancellable(JobKind::AiSubmit),
                        egui::Button::new("停止批量补签"),
                    )
                    .clicked()
                    && self.jobs.cancel(JobKind::AiSubmit)
                {
                    self.status = "已请求停止批量补签；当前条目结束后停止，其余不再提交".into();
                }
            });
        }
        let picked = self.ai_page.picked.clone();
        let days = self.ai_page.days.max(1);
        let per = self.ai_page.per_day.max(1);
        let total = picked.len() as i64 * days * per;
        mobile::row(ui, |ui| {
            let batch_label = format!(
                "批量提交（{}天 × {}次 × {}项 = {} 条）",
                days,
                per,
                picked.len(),
                total
            );
            let btn = theme::primary_btn(&batch_label);
            let response = if mobile::compact_ui(ui) {
                ui.add_enabled_ui(!ai_busy && logged && !picked.is_empty(), |ui| {
                    ui.add_sized([ui.available_width(), mobile::TOUCH_HEIGHT], btn)
                })
                .inner
            } else {
                ui.add_enabled(!ai_busy && logged && !picked.is_empty(), btn)
            };
            if response.clicked() {
                self.ai_confirm = Some(AiBatchPlan {
                    sport_ids: picked,
                    days,
                    per_day: per,
                    mode: self.ai_mode(),
                });
            }
        });

        if let Some(plan) = self.ai_confirm.take() {
            let total = plan.sport_ids.len() as i64 * plan.days * plan.per_day;
            let mut confirmed = false;
            let mut cancelled = false;
            egui::Window::new(
                egui::RichText::new("确认批量提交")
                    .strong()
                    .color(theme::text()),
            )
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .order(egui::Order::Foreground)
            .show(ui.ctx(), |ui| {
                ui.label(
                    egui::RichText::new(format!(
                        "将顺序提交 {} 条：{} 天 × 每天 {} 次 × {} 个项目",
                        total,
                        plan.days,
                        plan.per_day,
                        plan.sport_ids.len()
                    ))
                    .color(theme::text()),
                );
                ui.add_space(6.0);
                mobile::row(ui, |ui| {
                    if ui.button("取消").clicked() {
                        cancelled = true;
                    }
                    if ui.add(theme::primary_btn("确认提交")).clicked() {
                        confirmed = true;
                    }
                });
            });
            if confirmed {
                self.run_ai_batch(plan);
            } else if cancelled {
                self.status = "已取消批量提交".into();
            } else {
                self.ai_confirm = Some(plan);
            }
        }
    }

    /// 当前提交意图。
    fn ai_mode(&mut self) -> AiMode {
        if self.ai_page.mode == 0 {
            let minutes = self.config.ai_minutes.clamp(1, 30);
            AiMode::Minutes { minutes }
        } else {
            let reps = (self.config.ai_reps.clamp(5, 1000) / 5) * 5;
            AiMode::Count { reps }
        }
    }

    fn submit_ai_once(&mut self, sport_id: i64) {
        let mode = self.ai_mode();
        let (mode_label, score_label, secs, per_min, consume) = ai_display(mode);
        let identity = self.identity.clone();
        let session = self.session.clone().unwrap_or_default();
        let ticket = self.jobs.start(JobKind::AiSubmit);
        self.status = "AI 成绩提交中…".into();
        self.spawn_job(move |tx| {
            let mut log = App::logger(tx.clone());
            let mut client = tx.client(identity, Some(session));
            let payload = match crate::api::flow::run_ai_submit(
                &mut client,
                sport_id,
                mode,
                &mut log,
            ) {
                Ok(biz) => {
                    log(&format!("[ai] 响应：{}", truncate_json(&biz)));
                    serde_json::json!({
                        "ok": true,
                        "sport_id": sport_id,
                        "mode": mode_label,
                        "score": score_label,
                        "secs": secs, "per_min": per_min, "consume": consume,
                        "resp_error": biz.get("error").cloned().unwrap_or(serde_json::Value::Null),
                        "resp_msg": biz.get("message").and_then(|m| m.as_str()).unwrap_or(""),
                    })
                }
                Err(e) => {
                    log(&format!("× AI 提交失败: {e}"));
                    serde_json::json!({ "ok": false, "message": e })
                }
            };
            tx.send(UiEvent::AiDone {
                ticket,
                result: payload,
            })
            .ok();
        });
    }

    /// 批量补签：单线程顺序提交，过去天数取当天 7:00-21:00 随机时刻。
    ///
    /// 可随时停止；登出 / 重新登录 / 会话失效时自动停止（每条提交之间检查）。
    /// 结束消息不带会话代号，保证提交忙碌状态一定被复位。
    pub(crate) fn run_ai_batch(&mut self, plan: AiBatchPlan) {
        if self.jobs.busy(JobKind::AiSubmit) {
            return;
        }
        let Some(session) = self.session.clone() else {
            self.status = "请先登录".into();
            return;
        };
        let identity = self.identity.clone();
        let total = plan.sport_ids.len() as i64 * plan.days * plan.per_day;
        let ticket = self.jobs.start(JobKind::AiSubmit);
        self.ai_batch = Some(super::AiBatchState { done: 0, total });
        self.status = "批量补签进行中…".into();
        #[cfg(test)]
        let test_host = self.route_fixture.as_ref().map(|(host, _)| host.clone());
        self.spawn_session_job(move |tx| {
            let mut log = App::logger(tx.clone());
            let mut client = tx.client(identity, Some(session));
            #[cfg(test)]
            if let Some(host) = test_host {
                client.set_test_host(host);
            }
            let mut upload = |sport: i64, at: Option<i64>| {
                crate::api::ai::upload(&mut client, sport, plan.mode, at)
            };
            let mut wait = |duration: Duration| batch_wait(ticket.control(), &tx, duration);
            let mut progress = |done: i64| {
                tx.send(UiEvent::AiBatchProgress {
                    ticket: ticket.clone(),
                    done,
                    total,
                })
                .ok();
            };
            let outcome = run_batch_items(&plan, &mut upload, &mut wait, &mut log, &mut progress);
            let payload = serde_json::json!({
                "ok": true, "batch": true,
                "success": outcome.ok, "done": outcome.done, "total": total,
                "days": plan.days, "per_day": plan.per_day,
                "sports": plan.sport_ids.len(),
                "stopped": outcome.stopped,
            });
            tx.untagged()
                .send(UiEvent::AiDone {
                    ticket: ticket.clone(),
                    result: payload,
                })
                .ok();
        });
    }

    fn draw_single_selector(&mut self, ui: &mut egui::Ui) {
        let page = &mut self.ai_page;
        mobile::row(ui, |ui| {
            ui.label("单发项目：");
            let sel_text = page
                .names
                .get(page.selected)
                .map(String::as_str)
                .unwrap_or("-");
            egui::ComboBox::from_id_salt("ai_combo")
                .selected_text(sel_text)
                .show_ui(ui, |ui| {
                    for (i, name) in page.names.iter().enumerate() {
                        ui.selectable_value(&mut page.selected, i, name.as_str());
                    }
                });
        });
    }
}

fn ai_display(mode: AiMode) -> (&'static str, String, f64, i64, f64) {
    // 展示预估：与提交侧同源参数（计次 90 个/分，计时 240 毫秒/个）
    match mode {
        AiMode::Minutes { minutes } => (
            "按分钟",
            format!("{minutes} 分钟"),
            (minutes * 60) as f64,
            (90.0 * minutes as f64).round() as i64,
            (minutes * 60) as f64 * 0.2,
        ),
        AiMode::Count { reps } => {
            let secs = ((reps as f64 / 90.0) * 60.0).round().max(30.0);
            (
                "按次",
                format!("{reps} 个"),
                secs,
                (reps as f64 / secs * 60.0).round() as i64,
                reps as f64 * 0.07,
            )
        }
    }
}

/// days_ago 天前的随机时刻（7:00-21:00）。
fn random_time_days_ago(days_ago: i64) -> i64 {
    use chrono::{Datelike, Duration, Local, TimeZone};
    let base = Local::now() - Duration::days(days_ago);
    let h = 7 + rand::random::<u32>() % 15;
    Local
        .with_ymd_and_hms(
            base.year(),
            base.month(),
            base.day(),
            h,
            rand::random::<u32>() % 60,
            rand::random::<u32>() % 60,
        )
        .single()
        .map(|t| t.timestamp_millis())
        .unwrap_or_else(crate::crypto::envelope::now_ms)
}

/// 批量循环结果。
#[derive(Debug, Default, PartialEq)]
pub(crate) struct BatchOutcome {
    pub ok: i64,
    pub done: i64,
    /// 中途停止的原因；None 表示全部处理完。
    pub stopped: Option<String>,
}

const BATCH_INTERVAL: Duration = Duration::from_millis(1200);

/// 等待 duration（可为 0），期间每 100ms 检查一次取消与会话失效；被打断时返回原因。
fn batch_wait(
    control: &crate::task_control::Control,
    tx: &super::JobSender,
    duration: Duration,
) -> Option<String> {
    let deadline = std::time::Instant::now() + duration;
    loop {
        if tx.session_rejected() {
            return Some(crate::api::client::SESSION_EXPIRED.into());
        }
        if control.check().is_err() {
            return Some("已取消（手动停止、登出或重新登录）".into());
        }
        let now = std::time::Instant::now();
        if now >= deadline {
            return None;
        }
        std::thread::sleep((deadline - now).min(Duration::from_millis(100)));
    }
}

/// 批量提交主循环：每条之前检查是否需要停止，条目之间间隔 1.2s。
pub(crate) fn run_batch_items(
    plan: &AiBatchPlan,
    upload: &mut dyn FnMut(i64, Option<i64>) -> Result<serde_json::Value, String>,
    wait: &mut dyn FnMut(Duration) -> Option<String>,
    log: &mut dyn FnMut(&str),
    progress: &mut dyn FnMut(i64),
) -> BatchOutcome {
    let total = plan.sport_ids.len() as i64 * plan.days * plan.per_day;
    let mut out = BatchOutcome::default();
    for day in 0..plan.days {
        for _ in 0..plan.per_day {
            for &sport in &plan.sport_ids {
                if let Some(reason) = wait(Duration::ZERO) {
                    out.stopped = Some(reason);
                    return out;
                }
                let at = (day != 0).then(|| random_time_days_ago(day));
                out.done += 1;
                let done = out.done;
                let tag = if day == 0 {
                    "今天".to_string()
                } else {
                    format!("{day} 天前")
                };
                match upload(sport, at) {
                    Ok(biz) if biz.get("error").and_then(|e| e.as_i64()) == Some(10000) => {
                        out.ok += 1;
                        log(&format!(
                            "√ [ai] {tag} sport={sport} 成功（{done}/{total}）"
                        ));
                    }
                    Ok(biz) => {
                        let msg = biz.get("message").and_then(|m| m.as_str()).unwrap_or("");
                        log(&format!(
                            "× [ai] {tag} sport={sport} 失败: {msg}（{done}/{total}）"
                        ));
                    }
                    Err(e) => {
                        log(&format!(
                            "× [ai] {tag} sport={sport} 失败: {e}（{done}/{total}）"
                        ));
                        if crate::api::client::is_session_expired(&e) {
                            progress(done);
                            out.stopped = Some(e);
                            return out;
                        }
                    }
                }
                progress(done);
                if done < total {
                    if let Some(reason) = wait(BATCH_INTERVAL) {
                        out.stopped = Some(reason);
                        return out;
                    }
                }
            }
        }
    }
    out
}

fn truncate_json(v: &serde_json::Value) -> String {
    let s = v.to_string();
    s.chars().take(200).collect()
}
