//! UI 层：App 状态机 + 顶栏 + 标签页 + 底部状态栏。
//!
//! 后台任务与消息处理在独立 impl App 块中。
//!
//! 线程模型：所有网络/长任务 spawn thread + mpsc channel<UiMsg> 回 UI（类型化事件见
//! `events.rs`），发送即唤醒重绘（WakeSender）。空闲时仅在时间相关文案即将变化的
//! 时刻（违规结果过期、指定时刻到达、点位过期、跨日）安排一次重绘；有任务进行时 1s 兜底。

pub mod about;
pub mod ai;
pub mod data;
pub mod device;
pub mod events;
pub mod fonts;
pub mod jobs;
pub mod log;
pub mod map;
pub mod mobile;
pub mod msgs;
pub mod osm;
mod preview_jobs;
pub mod records;
mod road_tools;
pub mod run;
pub mod theme;
pub mod user;

use crate::api::model::{self, Config, HeaderIdentity, Session};
use eframe::egui;
use std::sync::mpsc::{Receiver, Sender};
use std::sync::Arc;

pub use events::{GpxOutcome, UiEvent, UiMsg};

/// 有任务进行时的兜底重绘间隔（进度/计时文案）。
const BUSY_REPAINT: std::time::Duration = std::time::Duration::from_secs(1);
/// 空闲兜底重绘间隔；时间相关文案另按其变化时刻精确安排。
const IDLE_REPAINT: std::time::Duration = std::time::Duration::from_secs(10);

/// 提交结果弹窗。
pub struct PopupInfo {
    pub title: String,
    pub lines: Vec<String>,
}

/// mpsc 发送端 + 发送后唤醒 UI，后台结果无需等待轮询即可显示。
pub struct WakeSender<T> {
    tx: Sender<T>,
    ctx: egui::Context,
}
impl<T> Clone for WakeSender<T> {
    fn clone(&self) -> Self {
        Self {
            tx: self.tx.clone(),
            ctx: self.ctx.clone(),
        }
    }
}
impl<T> WakeSender<T> {
    pub fn new(tx: Sender<T>, ctx: &egui::Context) -> Self {
        Self {
            tx,
            ctx: ctx.clone(),
        }
    }
    pub fn send(&self, value: T) -> Result<(), std::sync::mpsc::SendError<T>> {
        let result = self.tx.send(value);
        self.ctx.request_repaint();
        result
    }
}

/// 后台任务的发送端：会话任务自动附带会话代号。
#[derive(Clone)]
pub struct JobSender {
    tx: WakeSender<UiMsg>,
    auth: Option<crate::api::client::AuthContext>,
}
impl JobSender {
    /// 发送失败（界面已关闭）时丢弃事件本身。
    pub fn send(&self, event: UiEvent) -> Result<(), std::sync::mpsc::SendError<()>> {
        self.tx
            .send(UiMsg {
                epoch: self.auth.as_ref().map(|context| context.epoch),
                event,
            })
            .map_err(|_| std::sync::mpsc::SendError(()))
    }
    /// 日志行（会话任务的日志在登出后同样丢弃）。
    pub fn log(&self, line: &str) {
        let _ = self.send(UiEvent::Log(line.to_string()));
    }
    /// 不带会话代号的发送端：用于必须送达的收尾消息（如批量提交结束）。
    pub fn untagged(&self) -> JobSender {
        JobSender {
            tx: self.tx.clone(),
            auth: None,
        }
    }
    /// 本任务所属会话是否已被服务端判定失效。
    pub fn session_rejected(&self) -> bool {
        self.auth
            .as_ref()
            .is_some_and(|c| c.rejected.load(std::sync::atomic::Ordering::SeqCst))
    }
    pub fn client(
        &self,
        identity: HeaderIdentity,
        session: Option<Session>,
    ) -> crate::api::client::ApiClient {
        crate::api::client::ApiClient::new(identity, session).with_auth_context(self.auth.clone())
    }
}

/// 各刷新任务当前的请求号；回包请求号不一致即为过期结果。
#[derive(Default)]
pub struct RequestIds {
    next: u64,
    pub records: u64,
    pub ai_records: u64,
    pub ai_list: u64,
    pub semester: u64,
    pub cheat: u64,
    pub rank: u64,
    pub user: u64,
    pub cache_entries: u64,
    pub gpx: u64,
    pub ai_batch: u64,
}
impl RequestIds {
    pub fn issue(&mut self) -> u64 {
        self.next = self.next.wrapping_add(1);
        self.next
    }
}

/// 进行中的 AI 批量补签。
pub struct AiBatchState {
    pub id: u64,
    pub done: i64,
    pub total: i64,
}

/// 顶层 App。
pub struct App {
    #[cfg(test)]
    route_fixture: Option<(String, std::path::PathBuf)>,
    pub session_epoch: u64,
    pub auth_guard: Arc<std::sync::atomic::AtomicBool>,
    auth_tx: Sender<crate::api::client::AuthFailure>,
    auth_rx: Receiver<crate::api::client::AuthFailure>,
    pub tx: WakeSender<UiMsg>,
    pub rx: Receiver<UiMsg>,
    pub log: log::LogStore,
    pub font_loaded: Option<String>,

    pub ip: String,
    pub identity: HeaderIdentity,
    pub device_buf: HeaderIdentity,
    pub session: Option<Session>,
    pub config: Config,

    pub username: String,
    pub password: String,
    pub remember: bool,
    pub login_busy: bool,
    pub auth_expired: bool,
    pub run_busy: bool,
    /// AI 成绩提交（单次或批量）进行中。
    pub ai_busy: bool,
    pub ai_list_busy: bool,
    pub records_busy: bool,
    pub ai_records_busy: bool,
    pub semester_busy: bool,
    pub rank_busy: bool,
    pub user_busy: bool,
    pub reqs: RequestIds,
    pub ai_batch: Option<AiBatchState>,
    pub status: String,
    pub popup: Option<PopupInfo>,
    pub ai_confirm: Option<ai::AiBatchPlan>,

    pub tab: usize,
    pub run_page: run::RunPage,
    pub ai_page: ai::AiPage,
    pub records_page: records::RecordsPage,
    pub data_page: data::DataPage,
    pub user_page: user::UserPage,
    pub device_page: device::DevicePage,
    pub update: about::UpdateUi,

    /// 已加载的 OSM 路网（真实道路路由 + 地图显示共用）。
    pub network: Option<Arc<route_planner::RoadGraph>>,
    pub osm_page: osm::OsmPage,
    net_tx: WakeSender<osm::NetworkLoad>,
    net_rx: Receiver<osm::NetworkLoad>,
    preview_tx: WakeSender<preview_jobs::PreviewResult>,
    preview_rx: Receiver<preview_jobs::PreviewResult>,
    run_control: Arc<crate::task_control::Control>,
    ai_control: Arc<crate::task_control::Control>,
}

impl eframe::App for App {
    #[cfg(target_os = "android")]
    fn clear_color(&self, visuals: &egui::Visuals) -> [f32; 4] {
        visuals.panel_fill.to_normalized_gamma_f32()
    }

    fn raw_input_hook(&mut self, ctx: &egui::Context, input: &mut egui::RawInput) {
        crate::platform::apply_safe_area(ctx, input);
    }

    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.poll_messages();
        #[cfg(target_os = "android")]
        self.poll_device_info();
        crate::platform::set_keep_screen_on(self.run_busy || self.ai_busy || self.login_busy);
        self.draw_shell(ctx);
        // 后台消息由 WakeSender 即时唤醒；此处只为时间相关文案安排下一次重绘
        ctx.request_repaint_after(self.next_repaint_delay(crate::crypto::envelope::now_ms()));

        if let Some(p) = &self.popup {
            let mut open = true;
            let mut close = false;
            egui::Window::new(egui::RichText::new(&p.title).strong())
                .open(&mut open)
                .collapsible(false)
                .resizable(false)
                .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
                .order(egui::Order::Foreground)
                .show(ctx, |ui| {
                    ui.set_max_width((ctx.screen_rect().width() - 32.0).min(520.0));
                    for line in &p.lines {
                        ui.label(line);
                    }
                    ui.add_space(6.0);
                    if ui.button("关闭").clicked() {
                        close = true;
                    }
                });
            if !open || close {
                self.popup = None;
            }
        }
        self.draw_update_windows(ctx);
        crate::platform::sync_clipboard(ctx);
    }
}

impl App {
    fn draw_shell(&mut self, ctx: &egui::Context) {
        // ── 顶栏 ────────────────────────────────────────────────
        egui::TopBottomPanel::top("top_bar").show(ctx, |ui| {
            ui.add_space(6.0);
            let compact = mobile::compact_ui(ui);
            let logged = self.session.is_some();
            let btn_text = if logged {
                "登出"
            } else if self.auth_expired {
                "重新登录"
            } else {
                "登录"
            };
            let mut login_clicked = false;
            let draw_connection = |ui: &mut egui::Ui| {
                ui.label("IP：");
                if self.ip.is_empty() {
                    ui.colored_label(theme::warn(), "获取中…");
                } else if self.ip == "失败" {
                    ui.colored_label(theme::err(), "失败");
                } else {
                    ui.monospace(&self.ip);
                }
                match &self.session {
                    Some(s) => {
                        ui.colored_label(theme::ok(), format!("● {}（uid={}）", s.name, s.uid));
                    }
                    None => {
                        ui.colored_label(theme::err(), "● 未登录");
                    }
                }
            };
            if compact {
                ui.horizontal_wrapped(|ui| {
                    ui.label(if logged { "已登录" } else { "未登录" });
                    if let Some(session) = &self.session {
                        ui.label(&session.name);
                    }
                });
                egui::CollapsingHeader::new("账号与登录")
                    .open(self.auth_expired.then_some(true))
                    .default_open(false)
                    .show(ui, |ui| {
                        egui::ScrollArea::vertical()
                            .id_salt("compact_account")
                            .max_height((ctx.screen_rect().height() * 0.3).min(200.0))
                            .show(ui, |ui| {
                                ui.label(format!("设备：{}", self.identity.device_id));
                                ui.add_space(4.0);
                                login_clicked = self.draw_login_form(ui, true, btn_text);
                            });
                    });
            } else {
                egui::Grid::new("top_row1")
                    .num_columns(6)
                    .spacing([10.0, 3.0])
                    .show(ui, |ui| {
                        draw_connection(ui);
                        ui.label("设备：");
                        ui.monospace(&self.identity.device_id);
                        ui.end_row();
                    });
                ui.add_space(3.0);
                egui::Grid::new("top_row2")
                    .num_columns(5)
                    .spacing([8.0, 3.0])
                    .show(ui, |ui| {
                        login_clicked = self.draw_login_form(ui, false, btn_text);
                        ui.end_row();
                    });
            }
            if login_clicked {
                if logged {
                    self.do_logout();
                } else {
                    self.do_login();
                }
            }
            ui.add_space(4.0);
        });

        // ── 底部状态栏 ──────────────────────────────────────────
        egui::TopBottomPanel::bottom("status_bar").show(ctx, |ui| {
            if mobile::compact_ui(ui) {
                ui.horizontal(|ui| {
                    ui.add(egui::Label::new(&self.status).truncate());
                });
                return;
            }
            ui.horizontal_wrapped(|ui| {
                ui.label(format!("日志 {} 行", self.log.len()));
                ui.separator();
                if let Some(s) = &self.data_page.semester {
                    ui.label(format!(
                        "学期 {}：有效 {}/{} 次",
                        s.sname, s.semester_valid_count, s.semester_count
                    ));
                }
                if let Some(c) = &self.data_page.cheat {
                    ui.label(c.label());
                }
                ui.separator();
                ui.label(&self.status);
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    // 超链接但不用链接样式，纯文本观感
                    ui.hyperlink_to(
                        egui::RichText::new("YanamiNeko").color(theme::text_dim()),
                        "https://github.com/YanamiNeko",
                    );
                    ui.label(egui::RichText::new("Powered by").color(theme::text_dim()));
                    ui.separator();
                    if ui
                        .add_enabled(
                            !(self.update.checking || self.update.downloading),
                            egui::Button::new(
                                egui::RichText::new("检查更新")
                                    .small()
                                    .color(theme::text_dim()),
                            ),
                        )
                        .clicked()
                    {
                        self.check_update(true);
                    }
                    ui.label(
                        egui::RichText::new(format!("v{}", crate::platform::version_name()))
                            .small()
                            .color(theme::text_dim()),
                    );
                });
            });
            ui.add_space(2.0);
        });

        // ── 标签页 ──────────────────────────────────────────────
        egui::CentralPanel::default().show(ctx, |ui| {
            mobile::tab_bar(ui, &mut self.tab);
            ui.separator();
            match self.tab {
                0 => self.draw_run(ui),
                1 => self.draw_ai(ui),
                2 => self.draw_records(ui),
                3 => self.draw_data(ui),
                4 => self.draw_user(ui),
                5 => self.draw_device(ui),
                6 => self.log.render(ui),
                7 => self.draw_osm_page(ui),
                _ => self.draw_about(ui),
            }
        });
    }
}

impl App {
    /// 账号/密码/记住密码/登录按钮（手机纵向排列，桌面放在一行网格里）。返回按钮是否被点击。
    fn draw_login_form(&mut self, ui: &mut egui::Ui, compact: bool, btn_text: &str) -> bool {
        let (user_width, pass_width) = if compact {
            (ui.available_width(), ui.available_width())
        } else {
            (220.0, 200.0)
        };
        ui.label("账号：");
        mobile::text_edit(
            ui,
            "login_username",
            &mut self.username,
            crate::platform::InputKind::Text,
            user_width,
        );
        ui.label("密码：");
        mobile::text_edit(
            ui,
            "login_password",
            &mut self.password,
            crate::platform::InputKind::Password,
            pass_width,
        );
        let login_busy = self.login_busy;
        let remember = &mut self.remember;
        let mut controls = |ui: &mut egui::Ui| {
            ui.checkbox(remember, "记住密码");
            ui.add_enabled(!login_busy, theme::primary_btn(btn_text))
                .clicked()
        };
        if compact {
            ui.horizontal_wrapped(controls).inner
        } else {
            controls(ui)
        }
    }

    fn from_state(
        ctx: &egui::Context,
        identity: HeaderIdentity,
        session: Session,
        config: Config,
    ) -> Self {
        let (tx, rx) = std::sync::mpsc::channel();
        let tx = WakeSender::new(tx, ctx);
        let (net_tx, net_rx) = std::sync::mpsc::channel();
        let net_tx = WakeSender::new(net_tx, ctx);
        let (preview_tx, preview_rx) = std::sync::mpsc::channel();
        let preview_tx = WakeSender::new(preview_tx, ctx);
        let font_loaded = fonts::install(ctx);
        theme::apply(ctx);
        let device_buf = identity.clone();
        let username = config.username.clone();
        let password = config.password.clone();
        let remember = config.remember;

        let (auth_tx, auth_rx) = std::sync::mpsc::channel();
        Self {
            #[cfg(test)]
            route_fixture: None,
            session_epoch: 0,
            auth_guard: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            auth_tx,
            auth_rx,
            tx: tx.clone(),
            rx,
            log: log::LogStore::default(),
            font_loaded,
            ip: String::new(),
            identity,
            device_buf,
            session: session.is_logged_in().then_some(session),
            config: config.clone(),
            username,
            password,
            remember,
            login_busy: false,
            auth_expired: false,
            run_busy: false,
            ai_busy: false,
            ai_list_busy: false,
            records_busy: false,
            ai_records_busy: false,
            semester_busy: false,
            rank_busy: false,
            user_busy: false,
            reqs: RequestIds::default(),
            ai_batch: None,
            status: "就绪".into(),
            popup: None,
            ai_confirm: None,
            tab: 0,
            run_page: run::RunPage {
                dist_min: config.dist_min,
                dist_max: config.dist_max,
                pace_min: config.pace_min,
                pace_max: config.pace_max,
                manual_altitude: config
                    .manual_altitude_range
                    .map(|r| format!("{}-{}", r.min_m, r.max_m))
                    .or_else(|| config.manual_altitude.map(|v| v.to_string()))
                    .unwrap_or_default(),
                start_mode: 0,
                days_ago: 0,
                hour: 12,
                minute: 0,
                face_check: config.face_check,
                plan: None,
                route_mode: crate::track::generate_road::RouteMode::from_str(&config.route_mode),
                map: map::MapState::default(),
                preview: None,
                preview_stale: true,
                preview_inputs: None,
                map_fitted: false,
                ..Default::default()
            },
            ai_page: ai::AiPage {
                days: 1,
                per_day: 1,
                ..Default::default()
            },
            records_page: records::RecordsPage::default(),
            data_page: data::DataPage::default(),
            user_page: user::UserPage::default(),
            device_page: device::DevicePage::default(),
            update: about::UpdateUi::default(),
            network: None,
            osm_page: osm::OsmPage {
                path: config.osm_path.clone(),
                ..Default::default()
            },
            net_tx,
            net_rx,
            preview_tx,
            preview_rx,
            run_control: Arc::new(crate::task_control::Control::default()),
            ai_control: Arc::new(crate::task_control::Control::default()),
        }
    }

    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        let mut app = Self::from_state(
            &cc.egui_ctx,
            model::load_identity(),
            model::load_session(),
            model::load_config(),
        );
        if app.font_loaded.is_none() {
            app.log.push("未找到系统中文字体，界面中文可能显示为方块");
        }
        // 上次更新残留的 .old/.new 顺手清掉
        crate::update::cleanup_residue();
        // 启动检查更新（silent 静默 / ask 询问 / off 关闭）
        match app.config.update_check.as_str() {
            "off" => {}
            "ask" => app.update.ask_startup = true,
            _ => app.check_update(false),
        }
        app.fetch_ip();
        #[cfg(target_os = "android")]
        crate::android::request_device_info(true);
        // AI 项目列表先上缓存，网络刷新后覆盖
        app.ai_page
            .set_list(crate::api::model::load_ai_sports().unwrap_or_default());
        if app.session.is_some() {
            app.refresh_data_page();
            app.refresh_records();
            app.refresh_user_page();
            app.refresh_ai_list();
            if app.config.osm_path.is_empty() {
                app.refresh_points();
            }
        }
        // 已配置 OSM 路网则启动时后台加载
        if !app.config.osm_path.is_empty() {
            let path = app.config.osm_path.clone();
            app.load_osm_async(path);
        }
        app
    }

    /// 后台任务（线程 + 日志 channel）。
    pub fn spawn_job<F>(&self, f: F)
    where
        F: FnOnce(JobSender) + Send + 'static,
    {
        let tx = JobSender {
            tx: self.tx.clone(),
            auth: None,
        };
        std::thread::spawn(move || f(tx));
    }

    pub fn spawn_session_job<F>(&self, f: F)
    where
        F: FnOnce(JobSender) + Send + 'static,
    {
        let tx = JobSender {
            tx: self.tx.clone(),
            auth: Some(crate::api::client::AuthContext {
                epoch: self.session_epoch,
                rejected: self.auth_guard.clone(),
                notify: self.auth_tx.clone(),
            }),
        };
        std::thread::spawn(move || f(tx));
    }

    /// 会话切换（登录/登出）：旧会话的结果将被丢弃，对应忙碌标记一并复位。
    fn renew_session_jobs(&mut self) {
        self.data_page.cheat = None;
        self.data_page.cheat_error = None;
        self.retire_session_jobs();
        self.auth_guard = Arc::new(std::sync::atomic::AtomicBool::new(false));
    }

    /// 递增会话代号并复位只属于旧会话的忙碌状态；批量补签随会话结束而停止。
    fn retire_session_jobs(&mut self) {
        self.session_epoch = self.session_epoch.wrapping_add(1);
        self.data_page.cheat_checking = false;
        self.records_busy = false;
        self.ai_records_busy = false;
        self.ai_list_busy = false;
        self.semester_busy = false;
        self.rank_busy = false;
        self.user_busy = false;
        self.records_page.detail_loading = false;
        self.records_page.ai_detail_loading = false;
        self.ai_control.cancel();
    }

    pub fn logger(tx: JobSender) -> impl FnMut(&str) {
        move |s: &str| tx.log(s)
    }

    /// 本帧之后的重绘间隔：有任务时 1s 兜底；否则只在时间相关文案变化的时刻重绘。
    pub(crate) fn next_repaint_delay(&self, now_ms: i64) -> std::time::Duration {
        let busy = self.any_busy()
            || self.update.checking
            || self.update.downloading
            || self.data_page.cheat_checking;
        let mut delay = if busy { BUSY_REPAINT } else { IDLE_REPAINT };
        if let Some(at) = self.next_time_transition(now_ms) {
            // 略晚于变化时刻，保证重绘时条件已成立
            let until = std::time::Duration::from_millis((at - now_ms).max(0) as u64 + 50);
            delay = delay.min(until);
        }
        delay
    }

    /// 最近一个会改变界面文案的时刻（毫秒时间戳）。
    fn next_time_transition(&self, now_ms: i64) -> Option<i64> {
        let mut next: Option<i64> = None;
        let mut consider = |at: i64| {
            if at > now_ms {
                next = Some(next.map_or(at, |n: i64| n.min(at)));
            }
        };
        // 违规检查结果在状态栏与数据页显示，到期后变为「已过期」
        if let Some(c) = &self.data_page.cheat {
            if c.checked_at_ms > 0 {
                consider(c.checked_at_ms + crate::api::cheat::FRESH_MS);
            }
        }
        if self.tab == 0 {
            if let Some(at) = self.run_page.next_time_transition(now_ms) {
                consider(at);
            }
            if let Some(inputs) = &self.run_page.preview_inputs {
                consider(inputs.loaded_ms + crate::api::model::POINTS_TTL_MS);
            }
        }
        next
    }
}

#[cfg(test)]
mod layout_tests;
