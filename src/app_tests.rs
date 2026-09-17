use super::*;
use egui::{Event, OutputCommand, RawInput, Rect, Vec2};

struct NoNetwork;

impl RankProvider for NoNetwork {
    fn fetch_rank(&self, _account: &Account) -> RankInfo {
        panic!("UI regression tests must not fetch ranks");
    }
}

fn test_app(count: usize) -> (tempfile::TempDir, LeagueAccountsApp) {
    let directory = tempfile::tempdir().unwrap();
    let mut manager = AccountManager::with_path(
        directory.path().join("accounts.json"),
        Arc::new(NoNetwork),
    );
    manager.accounts = (0..count)
        .map(|index| Account {
            account_id: format!("review-regression-test-{index}"),
            name: format!("Player{index}#EUW"),
            region: "euw".into(),
            region_display: "EUW".into(),
            description: "description".into(),
            ..Account::default()
        })
        .collect();
    manager.save_accounts().unwrap();
    let mut app = LeagueAccountsApp::with_manager(manager, String::new());
    app.selected = app.manager.accounts.first().map(Account::key);
    (directory, app)
}

fn frame(ctx: &Context, app: &mut LeagueAccountsApp, size: Vec2, events: Vec<Event>) -> egui::FullOutput {
    ctx.run_ui(
        RawInput {
            screen_rect: Some(Rect::from_min_size(egui::Pos2::ZERO, size)),
            focused: true,
            events,
            ..Default::default()
        },
        |ui| app.render(ui),
    )
}

fn key(key: Key, modifiers: Modifiers) -> Event {
    Event::Key {
        key,
        physical_key: None,
        pressed: true,
        repeat: false,
        modifiers,
    }
}

#[test]
fn delete_in_search_edits_text_without_deleting_selected_account() {
    let (_directory, mut app) = test_app(1);
    let ctx = Context::default();
    let size = egui::vec2(1280.0, 900.0);
    let accounts = app.manager.accounts.clone();
    let saved = std::fs::read(&app.manager.accounts_file).unwrap();
    app.search = "searchtext".into();
    let _ = frame(&ctx, &mut app, size, vec![]);
    ctx.memory_mut(|memory| memory.request_focus(egui::Id::new("search")));
    let _ = frame(
        &ctx,
        &mut app,
        size,
        vec![key(Key::Home, Modifiers::NONE), key(Key::Delete, Modifiers::NONE)],
    );
    assert_eq!(app.search, "earchtext");
    assert_eq!(app.manager.accounts, accounts);
    assert_eq!(std::fs::read(&app.manager.accounts_file).unwrap(), saved);
}

#[test]
fn delete_in_description_editor_does_not_delete_selected_account() {
    let (_directory, mut app) = test_app(1);
    let ctx = Context::default();
    let size = egui::vec2(1280.0, 900.0);
    let account = app.manager.accounts[0].clone();
    let saved = std::fs::read(&app.manager.accounts_file).unwrap();
    app.show_edit(&account, EditField::Description);
    let _ = frame(&ctx, &mut app, size, vec![]);
    ctx.memory_mut(|memory| memory.request_focus(egui::Id::new("account_edit")));
    let _ = frame(
        &ctx,
        &mut app,
        size,
        vec![key(Key::Home, Modifiers::NONE), key(Key::Delete, Modifiers::NONE)],
    );
    assert_eq!(app.edit_state.as_ref().unwrap().value, "escription");
    assert_eq!(app.manager.accounts, vec![account]);
    assert_eq!(std::fs::read(&app.manager.accounts_file).unwrap(), saved);
}

#[test]
fn copy_in_search_preserves_text_copy_instead_of_copying_credentials() {
    let (_directory, mut app) = test_app(1);
    let ctx = Context::default();
    let size = egui::vec2(1280.0, 900.0);
    app.search = "copy me".into();
    let _ = frame(&ctx, &mut app, size, vec![]);
    ctx.memory_mut(|memory| memory.request_focus(egui::Id::new("search")));
    let output = frame(
        &ctx,
        &mut app,
        size,
        vec![
            key(Key::A, Modifiers { ctrl: true, command: true, ..Modifiers::NONE }),
            Event::Copy,
        ],
    );
    assert!(output.platform_output.commands.iter().any(|command| {
        matches!(command, OutputCommand::CopyText(text) if text == "copy me")
    }));
    assert_eq!(app.copy_counter, 0);
}

fn text_is_visible(output: &egui::FullOutput, text: &str, viewport: Rect) -> bool {
    fn in_shape(shape: &egui::epaint::Shape, text: &str, clip: Rect) -> bool {
        match shape {
            egui::epaint::Shape::Text(shape) => {
                shape.galley.job.text == text
                    && clip.contains_rect(Rect::from_min_size(shape.pos, shape.galley.size()))
            }
            egui::epaint::Shape::Vec(shapes) => {
                shapes.iter().any(|shape| in_shape(shape, text, clip))
            }
            _ => false,
        }
    }
    output.shapes.iter().any(|shape| {
        in_shape(&shape.shape, text, shape.clip_rect.intersect(viewport))
    })
}

#[test]
fn forms_and_filters_are_visible_with_empty_and_full_tables() {
    for count in [0, 100] {
        let (_directory, mut app) = test_app(count);
        let ctx = Context::default();
        let size = egui::vec2(1280.0, 900.0);
        let viewport = Rect::from_min_size(egui::Pos2::ZERO, size);
        for _ in 0..3 {
            let _ = frame(&ctx, &mut app, size, vec![]);
        }
        let output = frame(&ctx, &mut app, size, vec![]);
        for text in ["Add New Account", "Multi Add", "Friend Elo:"] {
            assert!(text_is_visible(&output, text, viewport), "{text} hidden with {count} accounts");
        }
    }
}

#[test]
fn small_windows_can_scroll_to_the_filters_below_the_table() {
    let (_directory, mut app) = test_app(100);
    let ctx = Context::default();
    let size = egui::vec2(1024.0, 480.0);
    let viewport = Rect::from_min_size(egui::Pos2::ZERO, size);
    for _ in 0..3 {
        let _ = frame(&ctx, &mut app, size, vec![]);
    }
    for _ in 0..10 {
        let _ = frame(
            &ctx,
            &mut app,
            size,
            vec![
                Event::PointerMoved(egui::pos2(40.0, 440.0)),
                Event::MouseWheel {
                    unit: egui::MouseWheelUnit::Point,
                    delta: egui::vec2(0.0, -1000.0),
                    phase: egui::TouchPhase::Move,
                    modifiers: Modifiers::NONE,
                },
            ],
        );
    }
    let output = frame(&ctx, &mut app, size, vec![]);
    assert!(text_is_visible(&output, "Friend Elo:", viewport));
}
