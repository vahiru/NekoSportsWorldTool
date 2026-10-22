//! 真实道路模式的预览地图：路线检查、每圈长度、道路/建筑/路线/打卡点绘制。

use super::PreviewLayer;
use crate::track::generate_road::RouteMode;
use crate::ui::map::{self, MapLayer};
use crate::ui::{theme, App};
use eframe::egui;

impl App {
    pub(super) fn draw_run_map(&mut self, ui: &mut egui::Ui) {
        if self.run_page.route_mode != RouteMode::Road {
            ui.label("经典模式仅按点位拟合，不包含建筑避让；需要沿道路行进时请选择真实道路模式。");
            return;
        }
        if ui
            .add_enabled(!self.osm_page.busy, egui::Button::new("刷新路线点位与围栏"))
            .clicked()
        {
            self.refresh_points();
        }
        if !self.osm_page.status.is_empty() {
            ui.label(&self.osm_page.status);
        }
        ui.horizontal_wrapped(|ui| {
            if ui.button("查看路网与校区").clicked() {
                self.tab = 7;
            }
            if self.identity.has_unconfigured_default_location()
                && ui.button("设置定位锚点").clicked()
            {
                self.tab = 5;
            }
        });
        let Some(net) = self.network.clone() else {
            ui.colored_label(
                theme::warn(),
                format!(
                    "学校路网尚未就绪：{}。可点击刷新，或到路网页自动获取。",
                    self.osm_page.status
                ),
            );
            return;
        };
        let Some(plan) = self.run_page.plan.clone() else {
            return;
        };

        // Arc 克隆仅增加引用计数，不复制点位与围栏
        let inputs = self
            .run_page
            .preview_inputs
            .as_ref()
            .filter(|input| {
                self.osm_page.offline
                    || (self
                        .session
                        .as_ref()
                        .map(|s| s.uid == input.uid && s.unid == input.unid)
                        .unwrap_or(false)
                        && self
                            .identity
                            .anchor_coordinate()
                            .map(|a| a == input.anchor)
                            .unwrap_or(false)
                        && (0..crate::api::model::POINTS_TTL_MS)
                            .contains(&(crate::crypto::envelope::now_ms() - input.loaded_ms)))
            })
            .cloned();
        let Some(inputs) = inputs else {
            self.run_page.preview = None;
            self.run_page.check = None;
            let message = if self.osm_page.busy {
                "正在刷新学校围栏、策略与点位，请查看上方进度。"
            } else if self.session.is_none() && !self.osm_page.offline {
                "本地地图已加载；请先登录，再刷新路线点位与围栏。"
            } else if self.identity.has_unconfigured_default_location() && !self.osm_page.offline {
                "本地地图已加载；请设置定位锚点，再刷新路线点位与围栏。"
            } else if self.run_page.preview_inputs.is_some() {
                "路线数据已过期或账号/定位已改变，请刷新路线点位与围栏。"
            } else {
                "地图已加载，但路线数据未就绪。请查看上方原因并刷新路线点位与围栏。"
            };
            ui.colored_label(theme::warn(), message);
            self.draw_run_base_map(ui, &net);
            return;
        };
        // A base map uses WGS84, while route inputs and previews use BD09.
        if self.run_page.base_map_request.take().is_some() {
            self.run_page.map_fitted = false;
        }
        self.prepare_preview(ui, net.clone(), &inputs, &plan);

        if inputs.fences.is_empty() {
            ui.colored_label(
                theme::warn(),
                "学校边界未提供：按道路闭环重复里程，无法确认路线始终在校内。",
            );
        }

        if let Some(check) = &self.run_page.check {
            ui.label(format!(
                "路线检查：{} 个道路连通分量；按当前顺序完整闭环最短距离 {}",
                check.components,
                check
                    .minimum_m
                    .map(|m| format!("{m:.0} 米"))
                    .unwrap_or("未知".into())
            ));
            for issue in &check.issues {
                ui.colored_label(theme::warn(), &issue.message);
            }
        }
        if let Some(p) = &self.run_page.preview {
            ui.label(format!(
                "总距离 {:.2} km · 预计 {} 圈（返回起点计一圈，含末圈）",
                plan.dist,
                p.laps.len()
            ));
            ui.label(format!(
                "点位顺序：{}",
                (1..=p.checkpoints.len())
                    .map(|i| i.to_string())
                    .collect::<Vec<_>>()
                    .join(" → ")
            ));
            ui.collapsing("每圈长度", |ui| {
                for (i, length) in p.laps.iter().enumerate() {
                    ui.label(format!(
                        "第 {} 圈：{:.1} 米{}",
                        i + 1,
                        length,
                        if i + 1 == p.laps.len() {
                            "（可能为末圈）"
                        } else {
                            ""
                        }
                    ));
                }
            });
            ui.add(egui::Slider::new(&mut self.run_page.progress, 0.0..=1.0).text("沿路线进度"));
        }
        if self.run_page.preview.is_some() {
            map::legend(
                ui,
                &[
                    ("道路", egui::Color32::from_rgb(200, 208, 204)),
                    ("建筑", egui::Color32::from_rgb(224, 194, 170)),
                    ("路线", egui::Color32::from_rgb(30, 111, 216)),
                    ("打卡点", egui::Color32::from_rgb(240, 180, 0)),
                    ("起点", egui::Color32::from_rgb(22, 160, 90)),
                    ("终点", egui::Color32::from_rgb(220, 38, 38)),
                ],
            );
        }

        ui.add_space(4.0);
        let rect = ui.available_rect_before_wrap();
        let (response, painter) = ui.allocate_painter(
            egui::Vec2::new(rect.width().max(200.0), 240.0),
            egui::Sense::drag(),
        );
        let canvas = response.rect;
        painter.rect_filled(
            canvas,
            egui::Rounding::ZERO,
            egui::Color32::from_rgb(250, 252, 251),
        );

        // 首次预览后适配视野：优先以电子围栏为中点/范围，无围栏时退化为路线+打卡点+道路
        if !self.run_page.map_fitted {
            let mut bounds: Vec<(f64, f64)> = Vec::new();
            if let Some(p) = &self.run_page.preview {
                for f in &p.fences {
                    bounds.extend(f.iter().copied());
                }
                if bounds.is_empty() {
                    bounds = map::collect_bounds(&p.edges);
                    bounds.extend(p.route.iter().copied());
                    bounds.extend(p.checkpoints.iter().copied());
                }
            }
            if bounds.is_empty() {
                bounds.extend(inputs.fences.iter().flatten().copied());
                bounds.extend(inputs.points.iter().copied());
            }
            if !bounds.is_empty() {
                self.run_page.map.fit(canvas, &bounds);
                self.run_page.map_fitted = true;
            }
        }

        self.sync_preview_layer();
        if let Some(p) = &self.run_page.preview {
            if let Some(layer) = &self.run_page.preview_layer {
                self.run_page.map.draw_layer(
                    &painter,
                    canvas,
                    &layer.layer,
                    egui::Color32::from_rgb(200, 208, 204),
                    egui::Color32::from_rgb(224, 194, 170),
                );
            }
            let fence_c = egui::Color32::from_rgb(180, 118, 0);
            for f in &p.fences {
                self.run_page
                    .map
                    .draw_polygon(&painter, canvas, f, fence_c, 2.0);
            }
            let route_c = egui::Color32::from_rgb(30, 111, 216);
            self.run_page
                .map
                .draw_polyline(&painter, canvas, &p.route, route_c, 2.5);
            let cp = egui::Color32::from_rgb(240, 180, 0);
            for &(la, lo) in &p.checkpoints {
                self.run_page
                    .map
                    .draw_point(&painter, canvas, la, lo, cp, 3.5);
            }
            if let Some(first) = p.route.first() {
                self.run_page.map.draw_point(
                    &painter,
                    canvas,
                    first.0,
                    first.1,
                    egui::Color32::from_rgb(22, 160, 90),
                    4.5,
                );
            }
            if let Some(last) = p.route.last() {
                self.run_page.map.draw_point(
                    &painter,
                    canvas,
                    last.0,
                    last.1,
                    egui::Color32::from_rgb(220, 38, 38),
                    4.5,
                );
            }
            if let Some(pos) = crate::track::route_view::position(
                &p.route,
                &p.arcs,
                plan.dist * 1000. * self.run_page.progress as f64,
            ) {
                self.run_page.map.draw_point(
                    &painter,
                    canvas,
                    pos.0,
                    pos.1,
                    egui::Color32::from_rgb(170, 20, 200),
                    7.0,
                );
            }
            let label = if p.length_m > 0.0 {
                let loops = plan.dist * 1000.0 / p.length_m;
                if loops > 1.15 {
                    format!(
                        "本次方案 {:.2} km · 单圈 {:.0} m × {:.1} 圈 · {} 打卡点",
                        plan.dist,
                        p.length_m,
                        loops,
                        p.checkpoints.len()
                    )
                } else {
                    format!(
                        "本次方案 {:.2} km · 路线 {:.0} m · {} 打卡点",
                        plan.dist,
                        p.length_m,
                        p.checkpoints.len()
                    )
                }
            } else {
                format!(
                    "本次方案 {:.2} km · {} 打卡点",
                    plan.dist,
                    p.checkpoints.len()
                )
            };
            painter.text(
                egui::Pos2::new(canvas.left() + 8.0, canvas.top() + 8.0),
                egui::Align2::LEFT_TOP,
                label,
                egui::FontId::proportional(12.0),
                egui::Color32::from_rgb(88, 104, 99),
            );
        } else {
            painter.text(
                canvas.center(),
                egui::Align2::CENTER_CENTER,
                "（无预览）",
                egui::FontId::proportional(14.0),
                egui::Color32::from_rgb(150, 160, 156),
            );
        }

        if self.run_page.preview.is_none() {
            for f in &inputs.fences {
                self.run_page.map.draw_polygon(
                    &painter,
                    canvas,
                    f,
                    egui::Color32::from_rgb(180, 118, 0),
                    2.0,
                );
            }
            for &(lat, lon) in &inputs.points {
                self.run_page
                    .map
                    .draw_point(&painter, canvas, lat, lon, egui::Color32::YELLOW, 4.);
            }
        }
        if let Some(check) = &self.run_page.check {
            for issue in &check.issues {
                if let Some(pos) = issue.position {
                    self.run_page.map.draw_point(
                        &painter,
                        canvas,
                        pos.lat,
                        pos.lon,
                        egui::Color32::RED,
                        7.,
                    );
                    painter.text(
                        self.run_page.map.to_screen(canvas, pos.lat, pos.lon),
                        egui::Align2::LEFT_BOTTOM,
                        issue
                            .point
                            .map(|i| format!("点位 {}", i + 1))
                            .unwrap_or("距离不足".into()),
                        egui::FontId::proportional(12.),
                        egui::Color32::RED,
                    );
                }
            }
        }
        self.run_page.map.interact(ui, canvas);
    }

    /// 预览更换时重建绘制缓存（道路去重、建筑包围盒）；无预览时释放。
    fn sync_preview_layer(&mut self) {
        let page = &mut self.run_page;
        let Some(plan) = &page.preview else {
            page.preview_layer = None;
            return;
        };
        let source = PreviewLayer::source_of(&page.preview_key, plan);
        if page.preview_layer.as_ref().map(|l| &l.source) != Some(&source) {
            page.preview_layer = Some(PreviewLayer {
                source,
                layer: MapLayer::new(&plan.edges, &plan.buildings),
            });
        }
    }

    fn draw_run_base_map(
        &mut self,
        ui: &mut egui::Ui,
        net: &std::sync::Arc<route_planner::RoadGraph>,
    ) {
        ui.label("基础地图：道路与建筑已加载，路线尚未生成。可拖拽和滚轮缩放。");
        map::legend(
            ui,
            &[
                ("道路", egui::Color32::from_rgb(190, 200, 196)),
                ("建筑", egui::Color32::from_rgb(214, 182, 158)),
            ],
        );
        let (response, painter) = ui.allocate_painter(
            egui::vec2(ui.available_width().max(200.), 240.),
            egui::Sense::drag(),
        );
        let canvas = response.rect;
        if self.run_page.base_map_request != Some(self.osm_page.request_id) {
            let mut bounds: Vec<_> = net
                .graph
                .node_weights()
                .map(|n| (n.coord.lat, n.coord.lon))
                .collect();
            bounds.extend(net.buildings.iter().flatten().map(|p| (p.lat, p.lon)));
            self.run_page.map.fit(canvas, &bounds);
            self.run_page.base_map_request = Some(self.osm_page.request_id);
            self.run_page.map_fitted = false;
        }
        painter.rect_filled(
            canvas,
            egui::Rounding::ZERO,
            egui::Color32::from_rgb(250, 252, 251),
        );
        self.run_page.map.draw_network(&painter, canvas, net);
        self.run_page.map.interact(ui, canvas);
    }
}
