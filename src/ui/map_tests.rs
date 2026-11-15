use super::*;

fn frame(ctx: &egui::Context, map: &mut MapState, events: Vec<egui::Event>) -> (Rect, f32) {
    frame_with_height(ctx, map, events, 320.0)
}

fn frame_with_height(
    ctx: &egui::Context,
    map: &mut MapState,
    events: Vec<egui::Event>,
    height: f32,
) -> (Rect, f32) {
    let mut result = (Rect::NOTHING, 0.0);
    let mut output = ctx.run_ui(
        egui::RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(420.0, 420.0))),
            events,
            ..Default::default()
        },
        |ui| {
            let scroll = egui::ScrollArea::vertical()
                .auto_shrink([false, false])
                .max_height(height)
                .show(ui, |ui| {
                    let (rect, response) =
                        ui.allocate_exact_size(Vec2::new(220.0, 160.0), egui::Sense::drag());
                    map.interact(ui, &response);
                    ui.add_space(1000.0);
                    rect
                });
            result = (scroll.inner, scroll.state.clamped_offset().y);
        },
    );
    output.textures_delta.clear();
    result
}

fn wheel(at: Pos2) -> Vec<egui::Event> {
    vec![
        egui::Event::PointerMoved(at),
        egui::Event::MouseWheel {
            unit: egui::MouseWheelUnit::Point,
            delta: Vec2::new(0.0, -80.0),
            phase: egui::TouchPhase::Move,
            source: egui::MouseWheelSource::Wheel,
            modifiers: Default::default(),
        },
    ]
}

#[test]
fn a_clipped_map_does_not_trap_page_scrolling_before_its_center_is_visible() {
    let ctx = egui::Context::default();
    let mut map = MapState::default();
    frame_with_height(&ctx, &mut map, vec![], 60.0);
    let (rect, _) = frame_with_height(&ctx, &mut map, vec![], 60.0);
    let scale = map.scale;
    let (_, offset) = frame_with_height(
        &ctx,
        &mut map,
        wheel(rect.left_top() + Vec2::new(30.0, 20.0)),
        60.0,
    );
    assert_eq!(map.scale, scale);
    assert!(offset > 0.0);
}

#[test]
fn map_wheel_zooms_without_scrolling_the_parent_or_its_smoothing_tail() {
    let ctx = egui::Context::default();
    let mut map = MapState::default();
    frame(&ctx, &mut map, vec![]);
    let (rect, _) = frame(&ctx, &mut map, vec![]);
    let scale = map.scale;
    let (_, offset) = frame(&ctx, &mut map, wheel(rect.center()));
    assert_ne!(map.scale, scale);
    assert_eq!(offset, 0.0);
    for _ in 0..20 {
        assert_eq!(frame(&ctx, &mut map, vec![]).1, 0.0);
    }
    let outside = Pos2::new(rect.right() + 35.0, rect.center().y);
    frame(&ctx, &mut map, wheel(outside));
    for _ in 0..20 {
        frame(&ctx, &mut map, vec![]);
    }
    assert!(
        frame(&ctx, &mut map, vec![]).1 > 0.0,
        "page scrolling outside map must still work"
    );
}

#[test]
fn dragging_into_a_map_does_not_transfer_pointer_ownership() {
    let ctx = egui::Context::default();
    let mut map = MapState::default();
    frame(&ctx, &mut map, vec![]);
    let (rect, _) = frame(&ctx, &mut map, vec![]);
    let start = Pos2::new(rect.right() + 35.0, rect.center().y);
    frame(
        &ctx,
        &mut map,
        vec![
            egui::Event::PointerMoved(start),
            egui::Event::PointerButton {
                pos: start,
                button: egui::PointerButton::Primary,
                pressed: true,
                modifiers: Default::default(),
            },
        ],
    );
    frame(
        &ctx,
        &mut map,
        vec![egui::Event::PointerMoved(rect.center())],
    );
    assert_eq!(map.offset, Vec2::ZERO);
    assert!(!map.dragged);
    frame(
        &ctx,
        &mut map,
        vec![egui::Event::PointerButton {
            pos: rect.center(),
            button: egui::PointerButton::Primary,
            pressed: false,
            modifiers: Default::default(),
        }],
    );
    let (rect, _) = frame(&ctx, &mut map, vec![]);
    frame(
        &ctx,
        &mut map,
        vec![
            egui::Event::PointerMoved(rect.center()),
            egui::Event::PointerButton {
                pos: rect.center(),
                button: egui::PointerButton::Primary,
                pressed: true,
                modifiers: Default::default(),
            },
        ],
    );
    frame(
        &ctx,
        &mut map,
        vec![egui::Event::PointerMoved(
            rect.center() + Vec2::new(20.0, 10.0),
        )],
    );
    assert!(map.dragged);
    assert_ne!(map.offset, Vec2::ZERO);
}
