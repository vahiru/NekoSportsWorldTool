use super::*;

struct DataDir(std::path::PathBuf);
impl DataDir {
    fn new() -> Self {
        let dir = std::env::temp_dir().join(format!("profile-ui-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        crate::platform::TEST_DATA_DIR.with(|p| p.replace(Some(dir.clone())));
        Self(dir)
    }
}
impl Drop for DataDir {
    fn drop(&mut self) {
        crate::platform::TEST_DATA_DIR.with(|p| p.replace(None));
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn late_worker_writes_original_profile_and_cannot_change_new_ui() {
    let _dir = DataDir::new();
    let ctx = egui::Context::default();
    let identity = HeaderIdentity {
        device_id: "kept-device".into(),
        ..Default::default()
    };
    let session = Session {
        uid: 7,
        token: "old-token".into(),
        ..Default::default()
    };
    model::save_identity(&identity).unwrap();
    model::save_session(&session).unwrap();
    let mut app = App::from_state(&ctx, identity, session, Config::default());
    let id = app
        .profiles
        .create("Second", &app.identity, &app.config)
        .unwrap();
    {
        let mut profiles = app.profiles.clone();
        profiles.active = id.clone();
        let _scope = crate::state::enter(profiles.root());
        model::save_session(&Session {
            uid: 8,
            token: "second-token".into(),
            ..Default::default()
        })
        .unwrap();
    }
    let (release, wait) = std::sync::mpsc::channel();
    let (done, finished) = std::sync::mpsc::channel();
    let ticket = app.jobs.start(JobKind::Records);
    app.spawn_session_job(move |tx| {
        wait.recv().unwrap();
        model::save_session(&Session {
            uid: 9,
            token: "late-old-token".into(),
            ..Default::default()
        })
        .unwrap();
        let _ = tx.send(UiEvent::Records {
            ticket,
            result: Err("old profile failure".into()),
        });
        done.send(()).unwrap();
    });
    app.switch_profile(&ctx, &id).unwrap();
    assert_eq!(app.session.as_ref().unwrap().uid, 8);
    assert_ne!(app.identity.device_id, "kept-device");
    let second_device = app.identity.device_id.clone();
    assert!(!app.show_password);
    release.send(()).unwrap();
    finished
        .recv_timeout(std::time::Duration::from_secs(5))
        .unwrap();
    {
        let _scope = crate::state::enter(app.profiles.root());
        app.poll_messages();
        assert_eq!(model::load_session().uid, 8);
    }
    assert_eq!(model::load_session().uid, 9);
    assert_eq!(app.session.as_ref().unwrap().uid, 8);
    assert!(!app.status.contains("old profile"));
    assert_eq!(app.identity.device_id, second_device);
    app.switch_profile(&ctx, "default").unwrap();
    assert_eq!(app.session.as_ref().unwrap().uid, 9);
    assert_eq!(app.identity.device_id, "kept-device");
}

#[test]
fn submissions_and_login_block_switching_until_finished() {
    let _dir = DataDir::new();
    let ctx = egui::Context::default();
    let mut app = App::from_state(
        &ctx,
        HeaderIdentity::default(),
        Session::default(),
        Config::default(),
    );
    let id = app
        .profiles
        .create("Second", &app.identity, &app.config)
        .unwrap();
    for kind in [
        JobKind::Run,
        JobKind::AiSubmit,
        JobKind::Login,
        JobKind::UpdateDownload,
    ] {
        app.jobs.start(kind);
        assert!(app.switch_profile(&ctx, &id).is_err());
        assert_eq!(app.profiles.active, "default");
        app.jobs.abandon(kind);
    }
    app.switch_profile(&ctx, &id).unwrap();
    assert_eq!(app.profiles.active, id);
}

#[test]
fn long_credentials_have_room_and_password_visibility_works_at_desktop_widths() {
    for width in [621.0, 880.0, 1280.0] {
        let ctx = egui::Context::default();
        let mut app = App::from_state(
            &ctx,
            HeaderIdentity::default(),
            Session::default(),
            Config::default(),
        );
        app.username = "123456789012345678901234".into();
        app.password = "a-long-password-123456".into();
        app.show_password = true;
        for _ in 0..2 {
            let mut output = ctx.run_ui(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(width, 720.0),
                    )),
                    ..Default::default()
                },
                |ui| {
                    app.draw_login_form(ui, false, "登录");
                },
            );
            output.textures_delta.clear();
            for text in [&app.username, &app.password] {
                let shape = output
                    .shapes
                    .iter()
                    .find_map(|clipped| match &clipped.shape {
                        egui::Shape::Text(t) if t.galley.text() == text => {
                            Some((t, clipped.clip_rect))
                        }
                        _ => None,
                    })
                    .expect("credential text must render");
                let rect = egui::Rect::from_min_size(shape.0.pos, shape.0.galley.size());
                assert!(
                    shape.1.contains_rect(rect),
                    "credential must be fully visible at {width}"
                );
            }
        }
        app.show_password = false;
        let mut output = ctx.run_ui(Default::default(), |ui| {
            app.draw_login_form(ui, false, "登录");
        });
        output.textures_delta.clear();
        assert!(!output.shapes.iter().any(|clipped| matches!(&clipped.shape, egui::Shape::Text(t) if t.galley.text() == app.password)));
    }
}

#[test]
fn account_manager_can_create_and_switch_a_profile_on_a_small_phone() {
    let _dir = DataDir::new();
    let ctx = egui::Context::default();
    let mut app = App::from_state(
        &ctx,
        HeaderIdentity::default(),
        Session::default(),
        Config::default(),
    );
    app.accounts_ui.open = true;
    app.accounts_ui.name = "Second account".into();
    let render = |app: &mut App, events| {
        let mut output = ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(320., 480.),
                )),
                events,
                ..Default::default()
            },
            |ui| app.draw_accounts_window(ui.ctx()),
        );
        output.textures_delta.clear();
        output
    };
    render(&mut app, vec![]);
    let output = render(&mut app, vec![]);
    let at = output
        .shapes
        .iter()
        .find_map(|clipped| match &clipped.shape {
            egui::Shape::Text(t) if t.galley.text() == "新增并切换" => {
                let rect = egui::Rect::from_min_size(t.pos, t.galley.size());
                clipped
                    .clip_rect
                    .contains_rect(rect)
                    .then_some(rect.center())
            }
            _ => None,
        })
        .expect("create button must be visible on a small phone");
    for pressed in [true, false] {
        render(
            &mut app,
            vec![
                egui::Event::PointerMoved(at),
                egui::Event::PointerButton {
                    pos: at,
                    button: egui::PointerButton::Primary,
                    pressed,
                    modifiers: Default::default(),
                },
            ],
        );
    }
    assert!(app.accounts_ui.pending.is_some());
    app.apply_pending_profile(&ctx);
    assert_eq!(app.profiles.entries.len(), 2);
    assert_ne!(app.profiles.active, "default");
    assert_eq!(app.profiles.name(), "Second account");
    assert!(app.username.is_empty());
    assert!(app.password.is_empty());
    assert!(app.session.is_none());
    assert!(!app.jobs.any_busy());
}

#[test]
fn logout_is_local_immediately_and_late_network_completion_preserves_next_login() {
    use std::io::{Read, Write};
    let dir = DataDir::new();
    let ctx = egui::Context::default();
    let session = Session {
        uid: 7,
        token: "old-token".into(),
        ..Default::default()
    };
    model::save_session(&session).unwrap();
    let mut app = App::from_state(&ctx, HeaderIdentity::default(), session, Config::default());
    let id = app
        .profiles
        .create("Second", &app.identity, &app.config)
        .unwrap();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    app.route_fixture = Some((
        format!("http://{}", listener.local_addr().unwrap()),
        dir.0.clone(),
    ));
    let (arrived, request) = std::sync::mpsc::channel();
    let (release, wait) = std::sync::mpsc::channel();
    let server = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        stream
            .set_read_timeout(Some(std::time::Duration::from_secs(5)))
            .unwrap();
        let mut raw = Vec::new();
        loop {
            let mut buf = [0; 4096];
            let n = stream.read(&mut buf).unwrap();
            assert!(n > 0);
            raw.extend_from_slice(&buf[..n]);
            if let Some(end) = raw.windows(4).position(|w| w == b"\r\n\r\n") {
                let header = String::from_utf8_lossy(&raw[..end]);
                let length: usize = header
                    .lines()
                    .find_map(|line| {
                        let (k, v) = line.split_once(':')?;
                        k.eq_ignore_ascii_case("content-length")
                            .then(|| v.trim().parse().unwrap())
                    })
                    .unwrap();
                if raw.len() >= end + 4 + length {
                    break;
                }
            }
        }
        arrived.send(()).unwrap();
        wait.recv_timeout(std::time::Duration::from_secs(5))
            .unwrap();
        let body = r#"{"error":10000,"data":{}}"#;
        write!(
            stream,
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        )
        .unwrap();
    });
    let (_, unused_rx) = std::sync::mpsc::channel();
    let old_rx = std::mem::replace(&mut app.rx, unused_rx);
    app.do_logout();
    request
        .recv_timeout(std::time::Duration::from_secs(5))
        .unwrap();
    assert!(app.session.is_none());
    assert!(!model::load_session().is_logged_in());
    app.switch_profile(&ctx, &id).unwrap();
    app.switch_profile(&ctx, "default").unwrap();
    assert!(
        app.session.is_none(),
        "switching back must not revive a logged-out session"
    );
    model::save_session(&Session {
        uid: 8,
        token: "new-token".into(),
        ..Default::default()
    })
    .unwrap();
    release.send(()).unwrap();
    server.join().unwrap();
    loop {
        let message = old_rx
            .recv_timeout(std::time::Duration::from_secs(5))
            .unwrap();
        if matches!(message.event, UiEvent::Log(ref text) if text == "[logout] 后台请求已结束")
        {
            break;
        }
    }
    assert_eq!(model::load_session().uid, 8);
}
