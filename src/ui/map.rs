//! 地图画布绘制原语：等距投影 + 拖拽平移 + 滚轮缩放。
//!
//! 供「路网」页与「跑步」页复用，纯离线绘制折线/多边形/点。

use eframe::egui;
use egui::{Color32, Pos2, Rect, Stroke, Vec2};
use std::sync::{Arc, Weak};

use crate::track::geom::{MET_PER_DEG_LAT, MET_PER_DEG_LNG};

/// 经纬度包围盒（度）。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BBox {
    min_lat: f64,
    min_lon: f64,
    max_lat: f64,
    max_lon: f64,
}

impl BBox {
    pub fn of(points: impl IntoIterator<Item = (f64, f64)>) -> Option<Self> {
        let mut out: Option<BBox> = None;
        for (lat, lon) in points {
            let b = out.get_or_insert(BBox {
                min_lat: lat,
                min_lon: lon,
                max_lat: lat,
                max_lon: lon,
            });
            b.min_lat = b.min_lat.min(lat);
            b.min_lon = b.min_lon.min(lon);
            b.max_lat = b.max_lat.max(lat);
            b.max_lon = b.max_lon.max(lon);
        }
        out
    }

    pub fn intersects(&self, other: &BBox) -> bool {
        self.max_lon >= other.min_lon
            && self.min_lon <= other.max_lon
            && self.max_lat >= other.min_lat
            && self.min_lat <= other.max_lat
    }
}

/// 预处理过的绘制图层：无向道路段（双向道路只保留一条）+ 带包围盒的建筑。
///
/// 数据到达时构建一次，之后每帧只做视野裁剪与投影。
#[derive(Default)]
pub struct MapLayer {
    segments: Vec<[(f64, f64); 2]>,
    polylines: Vec<(BBox, Vec<(f64, f64)>)>,
    buildings: Vec<(BBox, Vec<(f64, f64)>)>,
}

impl MapLayer {
    pub fn new(edges: &[Vec<(f64, f64)>], buildings: &[Vec<(f64, f64)>]) -> Self {
        let key = |p: (f64, f64)| (p.0.to_bits(), p.1.to_bits());
        let mut seen = std::collections::HashSet::new();
        let mut layer = MapLayer::default();
        for edge in edges {
            match edge.len() {
                0 | 1 => {}
                2 => {
                    let (a, b) = (key(edge[0]), key(edge[1]));
                    if seen.insert(if a <= b { (a, b) } else { (b, a) }) {
                        layer.segments.push([edge[0], edge[1]]);
                    }
                }
                _ => {
                    if let Some(bbox) = BBox::of(edge.iter().copied()) {
                        layer.polylines.push((bbox, edge.clone()));
                    }
                }
            }
        }
        for building in buildings.iter().filter(|b| b.len() >= 3) {
            if let Some(bbox) = BBox::of(building.iter().copied()) {
                layer.buildings.push((bbox, building.clone()));
            }
        }
        layer
    }

    /// 去重后的道路段数。
    #[cfg(test)]
    pub fn segment_count(&self) -> usize {
        self.segments.len()
    }
}

/// 地图视图状态（世界坐标为度）。
pub struct MapState {
    /// 参考中心（lat, lon）。
    pub center: (f64, f64),
    /// 屏幕每米像素（缩放）。
    pub scale: f32,
    /// 平移偏移（像素）。
    pub offset: Vec2,
    /// 最近是否被拖拽过（供外部判断）。
    pub dragged: bool,
    /// 原始路网建筑的包围盒缓存（Weak 保证同一路网才复用）。
    network_boxes: Option<(Weak<route_planner::RoadGraph>, Vec<BBox>)>,
}

impl Default for MapState {
    fn default() -> Self {
        MapState {
            center: (0.0, 0.0),
            scale: 0.3,
            offset: Vec2::ZERO,
            dragged: false,
            network_boxes: None,
        }
    }
}

impl MapState {
    pub fn world_bounds(&self, rect: Rect) -> (route_planner::Coord, route_planner::Coord) {
        let x = |screen: f32| {
            self.center.1
                + (screen - rect.center().x - self.offset.x) as f64
                    / (self.scale as f64 * MET_PER_DEG_LNG)
        };
        let y = |screen: f32| {
            self.center.0
                - (screen - rect.center().y - self.offset.y) as f64
                    / (self.scale as f64 * MET_PER_DEG_LAT)
        };
        (
            route_planner::Coord::new(x(rect.left()), y(rect.bottom())),
            route_planner::Coord::new(x(rect.right()), y(rect.top())),
        )
    }
    /// 当前画布可见范围（度）。
    pub fn view_box(&self, rect: Rect) -> BBox {
        let (a, b) = self.world_bounds(rect);
        BBox {
            min_lat: a.lat.min(b.lat),
            min_lon: a.lon.min(b.lon),
            max_lat: a.lat.max(b.lat),
            max_lon: a.lon.max(b.lon),
        }
    }

    /// 依据一组点（lat, lon）自动适配视野。
    pub fn fit(&mut self, rect: Rect, pts: &[(f64, f64)]) {
        if pts.len() < 2 {
            return;
        }
        let (mut slat, mut slon) = (0.0f64, 0.0f64);
        for (la, lo) in pts {
            slat += *la;
            slon += *lo;
        }
        let n = pts.len() as f64;
        self.center = (slat / n, slon / n);
        let (mut minx, mut maxx, mut miny, mut maxy) = (
            f64::INFINITY,
            f64::NEG_INFINITY,
            f64::INFINITY,
            f64::NEG_INFINITY,
        );
        for (la, lo) in pts {
            let x = (lo - self.center.1) * MET_PER_DEG_LNG;
            let y = (la - self.center.0) * MET_PER_DEG_LAT;
            minx = minx.min(x);
            maxx = maxx.max(x);
            miny = miny.min(y);
            maxy = maxy.max(y);
        }
        let w = (maxx - minx).max(40.0);
        let h = (maxy - miny).max(40.0);
        let sx = rect.width() as f64 / w;
        let sy = rect.height() as f64 / h;
        self.scale = ((sx.min(sy)) * 0.92) as f32;
        self.offset = Vec2::ZERO;
    }

    /// 度坐标 → 屏幕坐标。
    pub fn to_screen(&self, rect: Rect, lat: f64, lon: f64) -> Pos2 {
        let x = (lon - self.center.1) * MET_PER_DEG_LNG * self.scale as f64;
        let y = -(lat - self.center.0) * MET_PER_DEG_LAT * self.scale as f64;
        let c = rect.center();
        Pos2::new(
            (c.x as f64 + x + self.offset.x as f64) as f32,
            (c.y as f64 + y + self.offset.y as f64) as f32,
        )
    }

    /// 处理拖拽/缩放交互（需在分配好 rect 后调用）。
    pub fn interact(&mut self, ui: &egui::Ui, rect: Rect) {
        self.dragged = false;
        let (drag, scroll) = ui.input(|i| {
            let hover = i
                .pointer
                .interact_pos()
                .map(|p| rect.contains(p))
                .unwrap_or(false);
            let dragging = hover && i.pointer.primary_down() && i.pointer.is_decidedly_dragging();
            (dragging, if hover { i.raw_scroll_delta.y } else { 0.0 })
        });
        if drag {
            let d = ui.input(|i| i.pointer.delta());
            self.offset += d;
            self.dragged = true;
        }
        if scroll != 0.0 {
            let factor = (scroll * 0.0015).exp() as f32;
            self.scale = (self.scale * factor).clamp(0.002, 200.0);
        }
    }

    pub fn draw_polyline(
        &self,
        painter: &egui::Painter,
        rect: Rect,
        pts: &[(f64, f64)],
        color: Color32,
        width: f32,
    ) {
        if pts.len() < 2 {
            return;
        }
        let Some(bbox) = BBox::of(pts.iter().copied()) else {
            return;
        };
        if !bbox.intersects(&self.view_box(rect)) {
            return;
        }
        let stroke = Stroke::new(width, color);
        if let [a, b] = pts {
            painter.line_segment(
                [self.to_screen(rect, a.0, a.1), self.to_screen(rect, b.0, b.1)],
                stroke,
            );
            return;
        }
        let ps: Vec<Pos2> = pts
            .iter()
            .map(|(la, lo)| self.to_screen(rect, *la, *lo))
            .collect();
        painter.add(egui::Shape::line(ps, stroke));
    }

    pub fn draw_polygon(
        &self,
        painter: &egui::Painter,
        rect: Rect,
        pts: &[(f64, f64)],
        color: Color32,
        width: f32,
    ) {
        if pts.len() < 3 {
            return;
        }
        // closed_line 自动闭合，无需重复首点
        let ps: Vec<Pos2> = pts
            .iter()
            .map(|(la, lo)| self.to_screen(rect, *la, *lo))
            .collect();
        painter.add(egui::Shape::closed_line(ps, Stroke::new(width, color)));
    }

    /// 绘制预处理图层：道路段与建筑均先按视野裁剪；缩小视野时道路抽稀。
    pub fn draw_layer(
        &self,
        painter: &egui::Painter,
        rect: Rect,
        layer: &MapLayer,
        road: Color32,
        building: Color32,
    ) {
        let view = self.view_box(rect);
        let stroke = Stroke::new(1.0_f32, road);
        let stride = if self.scale < 0.1 {
            (layer.segments.len() / 4000).max(1)
        } else {
            1
        };
        for [a, b] in layer.segments.iter().step_by(stride) {
            let seg = BBox::of([*a, *b]).expect("two points");
            if seg.intersects(&view) {
                painter.line_segment(
                    [self.to_screen(rect, a.0, a.1), self.to_screen(rect, b.0, b.1)],
                    stroke,
                );
            }
        }
        for (bbox, line) in &layer.polylines {
            if bbox.intersects(&view) {
                self.draw_polyline(painter, rect, line, road, 1.0);
            }
        }
        for (bbox, ring) in &layer.buildings {
            if bbox.intersects(&view) {
                self.draw_polygon(painter, rect, ring, building, 1.0);
            }
        }
    }

    pub fn draw_point(
        &self,
        painter: &egui::Painter,
        rect: Rect,
        lat: f64,
        lon: f64,
        color: Color32,
        radius: f32,
    ) {
        let c = self.to_screen(rect, lat, lon);
        painter.circle_filled(c, radius, color);
    }

    /// Draw the unmodified WGS84 OSM graph, without school API data or route planning.
    ///
    /// 道路由空间索引按视野取出（已去重）；建筑按缓存的包围盒裁剪，直接投影不做中间拷贝。
    pub fn draw_network(
        &mut self,
        painter: &egui::Painter,
        rect: Rect,
        net: &Arc<route_planner::RoadGraph>,
    ) {
        let road_color = Color32::from_rgb(190, 200, 196);
        let road = Stroke::new(1.0_f32, road_color);
        let (min, max) = self.world_bounds(rect);
        let visible = net.visible_edges(min, max);
        let stride = if self.scale < 0.1 {
            (visible.len() / 4000).max(1)
        } else {
            1
        };
        for edge in visible.iter().step_by(stride) {
            painter.line_segment(
                [
                    self.to_screen(rect, edge[0].lat, edge[0].lon),
                    self.to_screen(rect, edge[1].lat, edge[1].lon),
                ],
                road,
            );
        }
        if stride > 1 {
            painter.text(
                rect.left_top() + egui::vec2(6., 6.),
                egui::Align2::LEFT_TOP,
                "缩小视野：道路显示已简化",
                egui::FontId::proportional(12.),
                road_color,
            );
        }
        let cached = self
            .network_boxes
            .as_ref()
            .is_some_and(|(source, _)| std::ptr::eq(source.as_ptr(), Arc::as_ptr(net)));
        if !cached {
            let boxes = net
                .buildings
                .iter()
                .map(|ring| {
                    BBox::of(ring.iter().map(|c| (c.lat, c.lon))).unwrap_or(BBox {
                        min_lat: f64::NAN,
                        min_lon: f64::NAN,
                        max_lat: f64::NAN,
                        max_lon: f64::NAN,
                    })
                })
                .collect();
            self.network_boxes = Some((Arc::downgrade(net), boxes));
        }
        let view = BBox {
            min_lat: min.lat.min(max.lat),
            min_lon: min.lon.min(max.lon),
            max_lat: min.lat.max(max.lat),
            max_lon: min.lon.max(max.lon),
        };
        let stroke = Stroke::new(1.0_f32, Color32::from_rgb(214, 182, 158));
        let boxes = &self.network_boxes.as_ref().expect("cached above").1;
        for (ring, bbox) in net.buildings.iter().zip(boxes) {
            if ring.len() < 3 || !bbox.intersects(&view) {
                continue;
            }
            let ps: Vec<Pos2> = ring
                .iter()
                .map(|c| self.to_screen(rect, c.lat, c.lon))
                .collect();
            painter.add(egui::Shape::closed_line(ps, stroke));
        }
    }
}

/// 收集一组折线/环的所有点，用于 fit。
pub fn collect_bounds(edges: &[Vec<(f64, f64)>]) -> Vec<(f64, f64)> {
    let mut pts: Vec<(f64, f64)> = Vec::new();
    for e in edges.iter().take(400) {
        for p in e {
            pts.push(*p);
        }
    }
    pts
}

/// 地图图例：颜色方块 + 说明文字。
pub fn legend(ui: &mut egui::Ui, items: &[(&str, Color32)]) {
    ui.horizontal_wrapped(|ui| {
        ui.label("图例：");
        for (label, color) in items {
            let (rect, _) = ui.allocate_exact_size(Vec2::new(10.0, 10.0), egui::Sense::hover());
            ui.painter()
                .rect_filled(rect, egui::Rounding::same(2.0), *color);
            ui.label(*label);
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layer_keeps_one_segment_per_two_way_road_and_boxes_buildings() {
        let a = (30.0, 120.0);
        let b = (30.001, 120.001);
        let c = (30.002, 120.0);
        let edges = vec![vec![a, b], vec![b, a], vec![b, c], vec![c, b], vec![a, c]];
        let ring = vec![(31.0, 121.0), (31.001, 121.0), (31.001, 121.001)];
        let layer = MapLayer::new(&edges, &[ring.clone(), vec![a, b]]);
        assert_eq!(layer.segment_count(), 3);
        assert_eq!(layer.buildings.len(), 1, "degenerate rings are dropped");
        let near = BBox::of([(31.0005, 121.0005), (31.0006, 121.0006)]).unwrap();
        let far = BBox::of([(10.0, 10.0), (10.1, 10.1)]).unwrap();
        assert!(layer.buildings[0].0.intersects(&near));
        assert!(!layer.buildings[0].0.intersects(&far));
    }
}
