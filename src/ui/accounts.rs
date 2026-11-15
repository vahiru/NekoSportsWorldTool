//! Account picker and profile management. Adapted from ZhYue314's account slots;
//! submission remains an explicit action in the existing run page.
use super::{mobile, App, JobKind};
use crate::api::model;
use eframe::egui;

#[derive(Default)]
pub struct AccountsUi {
    pub open: bool,
    pub name: String,
    pub pending: Option<String>,
    remove: Option<String>,
}

impl App {
    fn profile_busy(&self) -> bool {
        [
            JobKind::Login,
            JobKind::Run,
            JobKind::AiSubmit,
            JobKind::UpdateDownload,
            JobKind::UpdateCheck,
        ]
        .into_iter()
        .any(|kind| self.jobs.busy(kind))
    }

    pub(super) fn draw_profile_picker(&mut self, ui: &mut egui::Ui) {
        let enabled = self.profiles_available && !self.profile_busy();
        let compact = mobile::compact_ui(ui);
        ui.add_enabled_ui(enabled, |ui| {
            ui.horizontal_wrapped(|ui| {
                if !compact {
                    ui.label("当前账号");
                }
                let mut selected = self.profiles.active.clone();
                egui::ComboBox::from_id_salt("account_profile")
                    .selected_text(if compact {
                        format!(
                            "{} · {}",
                            self.profiles.name(),
                            if self.session.is_some() {
                                "已登录"
                            } else {
                                "未登录"
                            }
                        )
                    } else {
                        self.profiles.name().to_owned()
                    })
                    .width(
                        (ui.available_width() - if compact { 65.0 } else { 110.0 })
                            .clamp(100.0, 280.0),
                    )
                    .show_ui(ui, |ui| {
                        for p in &self.profiles.entries {
                            ui.selectable_value(&mut selected, p.id.clone(), &p.name);
                        }
                    });
                if selected != self.profiles.active {
                    self.accounts_ui.pending = Some(selected);
                }
                if ui
                    .button(if compact { "账号" } else { "管理账号" })
                    .clicked()
                {
                    self.accounts_ui.open = true;
                    self.accounts_ui.name = self.profiles.name().into();
                }
            });
        })
        .response
        .on_disabled_hover_text("请等待登录、提交或更新任务结束后切换账号");
    }

    pub(super) fn draw_accounts_window(&mut self, ctx: &egui::Context) {
        if !self.accounts_ui.open {
            return;
        }
        let mut open = true;
        egui::Window::new("管理账号")
            .open(&mut open)
            .default_width(460.0)
            .show(ctx, |ui| {
                ui.set_max_width((ctx.content_rect().width() - 48.0).max(160.0));
                egui::ScrollArea::vertical()
                    .id_salt("account_manager_scroll")
                    .max_height((ctx.content_rect().height() - 100.0).max(120.0))
                    .show(ui, |ui| {
                        ui.label("每个账号独立保存登录状态、密码、设备信息和设置。");
                        ui.label(
                            "新增账号会生成独立设备环境并固定保存；位置可在设备信息页单独修改。",
                        );
                        ui.add_space(8.0);
                        ui.label("账号名称");
                        mobile::text_edit(
                            ui,
                            "profile_name",
                            &mut self.accounts_ui.name,
                            crate::platform::InputKind::Text,
                            (ui.available_width() - 12.0).max(40.0),
                        );
                        let enabled = self.profiles_available && !self.profile_busy();
                        ui.add_enabled_ui(enabled, |ui| {
                            ui.horizontal_wrapped(|ui| {
                                if ui.button("重命名当前账号").clicked() {
                                    if let Err(error) = self.profiles.rename(&self.accounts_ui.name)
                                    {
                                        self.status = error;
                                    }
                                }
                                if ui.button("新增并切换").clicked() {
                                    match self.profiles.create(
                                        &self.accounts_ui.name,
                                        &self.identity,
                                        &self.config,
                                    ) {
                                        Ok(id) => self.accounts_ui.pending = Some(id),
                                        Err(error) => self.status = error,
                                    }
                                }
                            });
                            ui.separator();
                            egui::ScrollArea::vertical()
                                .max_height(220.0)
                                .show(ui, |ui| {
                                    for profile in self.profiles.entries.clone() {
                                        ui.horizontal_wrapped(|ui| {
                                            ui.label(&profile.name);
                                            if profile.id == self.profiles.active {
                                                ui.label("当前");
                                            } else if ui.button("切换").clicked() {
                                                self.accounts_ui.pending = Some(profile.id.clone());
                                            }
                                            if profile.id != "default"
                                                && profile.id != self.profiles.active
                                                && ui.small_button("移除").clicked()
                                            {
                                                self.accounts_ui.remove = Some(profile.id);
                                            }
                                        });
                                    }
                                });
                            if let Some(id) = self.accounts_ui.remove.clone() {
                                ui.separator();
                                ui.label("从账号列表移除？本地账号文件会保留，便于恢复。");
                                ui.horizontal_wrapped(|ui| {
                                    if ui.button("确认移除").clicked() {
                                        if let Err(error) = self.profiles.remove(&id) {
                                            self.status = error;
                                        }
                                        self.accounts_ui.remove = None;
                                    }
                                    if ui.button("取消").clicked() {
                                        self.accounts_ui.remove = None;
                                    }
                                });
                            }
                        });
                        ui.label(&self.status);
                    });
            });
        self.accounts_ui.open = open;
    }

    pub(super) fn apply_pending_profile(&mut self, ctx: &egui::Context) {
        if let Some(id) = self.accounts_ui.pending.take() {
            match self.switch_profile(ctx, &id) {
                Ok(()) => {
                    // Reuse saved sessions. Never log out the old account on switch.
                    let _scope = crate::state::enter(self.profiles.root());
                    self.ai_page
                        .set_list(model::load_ai_sports().unwrap_or_default());
                    if self.session.is_some() {
                        self.refresh_data_page();
                        self.refresh_records();
                        self.refresh_user_page();
                        self.refresh_ai_list();
                        self.refresh_points();
                    } else if !self.config.osm_path.is_empty() {
                        self.load_osm_async(self.config.osm_path.clone());
                    }
                    ctx.request_repaint();
                }
                Err(error) => self.status = error,
            }
        }
    }

    /// Reset all account-specific view state and channels. Late workers own their
    /// old directory and disconnected senders; none can restore the old session.
    pub(super) fn switch_profile(&mut self, ctx: &egui::Context, id: &str) -> Result<(), String> {
        if !self.profiles_available {
            return Err("账号索引读取失败，请先恢复索引".into());
        }
        if self.profile_busy() {
            return Err("请等待登录、提交或更新任务结束后切换账号".into());
        }
        if self.profiles.active == id {
            return Ok(());
        }
        let mut next = self.profiles.clone();
        if !next.entries.iter().any(|p| p.id == id) {
            return Err("找不到该账号".into());
        }
        {
            let _scope = crate::state::enter(self.profiles.root());
            let mut config = self.config.clone();
            config.dist_min = self.run_page.dist_min;
            config.dist_max = self.run_page.dist_max;
            config.pace_min = self.run_page.pace_min;
            config.pace_max = self.run_page.pace_max;
            config.face_check = self.run_page.face_check;
            config.terrain_simulation = self.run_page.terrain_simulation.clone();
            config.route_mode =
                if self.run_page.route_mode == crate::track::generate_road::RouteMode::Road {
                    "road"
                } else {
                    "legacy"
                }
                .into();
            if let Ok(altitude) = crate::track::altitude::parse_spec(&self.run_page.manual_altitude)
            {
                use crate::track::altitude::AltitudeSpec;
                (config.manual_altitude, config.manual_altitude_range) = match altitude {
                    None => (None, None),
                    Some(AltitudeSpec::Single(v)) => (Some(v), None),
                    Some(AltitudeSpec::Range(r)) => (None, Some(r)),
                };
            }
            config.username = self.username.clone();
            config.remember = self.remember;
            config.password = if self.remember {
                self.password.clone()
            } else {
                String::new()
            };
            model::save_config(&config)?;
        }
        next.active = id.into();
        let _scope = crate::state::enter(next.root());
        // Strict reads: a corrupt target must not silently become a fresh login.
        let identity = model::try_load_identity()?;
        let session = model::try_load_session()?;
        let config = model::try_load_config()?;
        next.select(id)?;
        self.jobs.reset_session();
        self.jobs.cancel(JobKind::RoadNetwork);
        self.jobs.cancel(JobKind::Preview);
        let ip = self.ip.clone();
        let tab = self.tab;
        let mut replacement = Self::from_state(ctx, identity, session, config);
        replacement.profiles = next;
        replacement.ip = ip;
        replacement.tab = tab;
        replacement.status = format!("已切换至 {}", replacement.profiles.name());
        *self = replacement;
        Ok(())
    }
}
