// Compiled as a unit-test module to exercise the actual application state/handlers.
use super::*;
use webkit6::prelude::WebViewExt;

pub(super) fn until(mut ready: impl FnMut() -> bool) {
    let deadline = std::time::Instant::now() + Duration::from_secs(20);
    loop {
        while gtk::glib::MainContext::default().pending() {
            gtk::glib::MainContext::default().iteration(false);
        }
        if ready() {
            return;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "application condition timed out"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
}

pub(super) fn js(grid: &TabulatorGrid, script: &str) -> serde_json::Value {
    let result = Rc::new(RefCell::new(None));
    let callback = result.clone();
    grid.webview.evaluate_javascript(
        script,
        None,
        None,
        None::<&gtk::gio::Cancellable>,
        move |value| {
            *callback.borrow_mut() = Some(value.map(|value| value.to_json(0).unwrap().to_string()));
        },
    );
    until(|| result.borrow().is_some());
    let raw = result
        .borrow_mut()
        .take()
        .unwrap()
        .expect("JavaScript evaluation");
    serde_json::from_str(&raw).unwrap()
}

fn edits_preview() -> (gtk::Window, gtk::Button) {
    let mut window = None;
    until(|| {
        window = gtk::Window::list_toplevels()
            .into_iter()
            .filter_map(|widget| widget.downcast::<gtk::Window>().ok())
            .find(|w| w.is_visible() && w.widget_name() == "table-edit-preview");
        window.is_some()
    });
    let window = window.unwrap();
    let accept = named_widget(window.upcast_ref(), "table-edit-accept")
        .unwrap()
        .downcast()
        .unwrap();
    (window, accept)
}

pub(super) fn preview_details(window: &gtk::Window) -> String {
    let text = named_widget(window.upcast_ref(), "confirmation-details")
        .unwrap()
        .downcast::<gtk::TextView>()
        .unwrap();
    let buffer = text.buffer();
    buffer
        .text(&buffer.start_iter(), &buffer.end_iter(), true)
        .into()
}

#[test]
#[ignore = "requires GTK display; no database connection"]
fn table_edit_preview_checks_snapshot_and_bulk_scope() {
    gtk::init().unwrap();
    adw::init().unwrap();
    let app = adw::Application::builder()
        .application_id("io.github.dbdeck.edit-preview-test")
        .build();
    app.register(None::<&gtk::gio::Cancellable>).unwrap();
    let runtime = Arc::new(tokio::runtime::Runtime::new().unwrap());
    let ui = build_window(&app, runtime.clone());
    let key = "edit-preview";
    let profile: ConnectionProfile = serde_json::from_value(serde_json::json!({
        "id": key, "name": "Preview fixture", "kind": "postgre_sql", "host": "localhost",
        "port": 5432, "database": "preview_db", "username": "local",
        "environment": "local", "color": "#ffffff", "read_only": false, "tls_mode": "disabled"
    }))
    .unwrap();
    let fixture_pool = || {
        let _guard = runtime.enter();
        let socket = std::env::temp_dir().join(format!("dbdeck-no-socket-{}", Uuid::new_v4()));
        DbPool::PostgreSql(
            sqlx::postgres::PgPoolOptions::new()
                .min_connections(0)
                .connect_lazy_with(sqlx::postgres::PgConnectOptions::new().socket(socket)),
        )
    };
    let session_key = profile_session_key(&profile);
    ui.sessions
        .borrow_mut()
        .insert(session_key.clone(), (profile.clone(), fixture_pool()));
    ui.table_profiles.borrow_mut().insert(key.into(), profile);
    ui.table_objects.borrow_mut().insert(
        key.into(),
        DbObject {
            schema: "public".into(),
            name: "items".into(),
            kind: database::DbObjectKind::Table,
        },
    );
    let data = Arc::new(TableData {
        columns: vec!["a".into(), "b".into(), "title".into()],
        rows: vec![vec![
            CellValue::Text("2".into()),
            CellValue::Text("3".into()),
            CellValue::Null,
        ]],
        primary_key: vec!["b".into(), "a".into()],
        cell_detail_key_columns: vec![],
        truncated: false,
        truncated_columns: false,
    });
    ui.table_data.borrow_mut().insert(key.into(), data.clone());
    let state = create_table_edit_state();
    ui.table_edit_states
        .borrow_mut()
        .insert(key.into(), state.clone());
    state
        .pending
        .borrow_mut()
        .insert((0, 2), Some("NULL".into()));

    save_table_edits(&ui, key);
    let (window, accept) = edits_preview();
    let details = preview_details(&window);
    assert!(
        details.contains(r#""b" = "3", "a" = "2" · "title""#),
        "{details}"
    );
    assert!(
        details.contains("Было: NULL\n  Станет: \"NULL\""),
        "{details}"
    );
    assert!(!state.saving.get());
    window.close();
    accept.emit_clicked();
    assert!(!state.saving.get());
    assert_eq!(state.pending.borrow()[&(0, 2)].as_deref(), Some("NULL"));

    save_table_edits(&ui, key);
    let (_, accept) = edits_preview();
    state
        .pending
        .borrow_mut()
        .insert((0, 2), Some("newer".into()));
    accept.emit_clicked();
    assert!(ui.status.text().contains("Откройте предпросмотр заново"));
    assert!(!state.saving.get());
    assert_eq!(state.pending.borrow()[&(0, 2)].as_deref(), Some("newer"));

    save_table_edits(&ui, key);
    let (_, accept) = edits_preview();
    ui.sessions.borrow_mut().get_mut(&session_key).unwrap().1 = fixture_pool();
    ui.status.set_text("");
    accept.emit_clicked();
    assert!(ui.status.text().contains("Откройте предпросмотр заново"));
    assert!(!state.saving.get());
    assert_eq!(state.pending.borrow()[&(0, 2)].as_deref(), Some("newer"));

    state.pending.borrow_mut().clear();
    *state.bulk_column.borrow_mut() = Some((2, Some("я".repeat(241))));
    save_table_edits(&ui, key);
    let (window, accept) = edits_preview();
    let details = preview_details(&window);
    assert!(
        details.contains(r#"Массовая замена столбца "title""#),
        "{details}"
    );
    assert!(
        details.contains("включая строки вне текущего фильтра"),
        "{details}"
    );
    assert!(details.contains("(фрагмент)"), "{details}");
    ui.table_data
        .borrow_mut()
        .insert(key.into(), Arc::new((*data).clone()));
    accept.emit_clicked();
    assert!(!state.saving.get());
    assert!(state.bulk_column.borrow().is_some());
    assert!(ui.status.text().contains("Откройте предпросмотр заново"));
}

#[test]
#[ignore = "requires GTK display; no database connection"]
fn cell_preview_preserves_multiline_text_and_indicates_truncation() {
    gtk::init().unwrap();
    adw::init().unwrap();
    let app = adw::Application::builder()
        .application_id("io.github.dbdeck.cell-preview-test")
        .build();
    app.register(None::<&gtk::gio::Cancellable>).unwrap();
    let ui = build_window(&app, Arc::new(tokio::runtime::Runtime::new().unwrap()));
    let data = Arc::new(TableData {
        columns: vec!["text".to_owned(), "id".to_owned()],
        rows: vec![
            vec![
                CellValue::Text("first\n<second>  ".to_owned()),
                CellValue::Text("1".to_owned()),
            ],
            vec![
                CellValue::Truncated("loaded portion".to_owned()),
                CellValue::Text("2".to_owned()),
            ],
            vec![
                CellValue::Truncated("loaded portion".to_owned()),
                CellValue::Truncated("incomplete key".to_owned()),
            ],
        ],
        primary_key: vec!["id".to_owned()],
        cell_detail_key_columns: vec![database::CellDetailKeyColumn {
            name: "id".to_owned(),
            key_type: "int".to_owned(),
        }],
        truncated: false,
        truncated_columns: false,
    });
    for key in ["preview", "sql::preview"] {
        ui.table_data
            .borrow_mut()
            .insert(key.to_owned(), data.clone());
        ui.table_objects.borrow_mut().insert(
            key.to_owned(),
            DbObject {
                schema: "public".to_owned(),
                name: "preview".to_owned(),
                kind: database::DbObjectKind::Table,
            },
        );
        let grid = TabulatorGrid::new(true, |_| {});
        grid.set_data(&data.columns, &[], false, false, false, &[], &[]);
        ui.tabulator_grids.borrow_mut().insert(key.to_owned(), grid);
    }
    let state = create_table_edit_state();
    state
        .pending
        .borrow_mut()
        .insert((0, 0), Some("edited\n<line>  ".to_owned()));
    ui.table_edit_states
        .borrow_mut()
        .insert("preview".to_owned(), state);
    let grid = ui.tabulator_grids.borrow()["preview"].clone();
    let stale_generation = grid.generation().unwrap();
    grid.set_data(&data.columns, &[], false, false, false, &[], &[]);
    handle_tabulator_message(
        &ui,
        "preview",
        &serde_json::json!({
            "type": "viewCell", "row": 1, "column": "c0", "generation": stale_generation,
        })
        .to_string(),
    );
    assert!(
        gtk::Window::list_toplevels()
            .into_iter()
            .filter_map(|window| window.downcast::<adw::Window>().ok())
            .all(|window| window.title().as_deref() != Some("Значение ячейки · text"))
    );
    for (key, row, expected, truncated, can_load) in [
        ("preview", 0, "edited\n<line>  ", false, false),
        ("preview", 1, "loaded portion", true, true),
        ("preview", 2, "loaded portion", true, false),
        ("sql::preview", 1, "loaded portion", true, false),
    ] {
        let generation = ui.tabulator_grids.borrow()[key].generation().unwrap();
        handle_tabulator_message(
            &ui,
            key,
            &serde_json::json!({ "type": "viewCell", "row": row, "column": "c0", "generation": generation }).to_string(),
        );
        let window = gtk::Window::list_toplevels()
            .into_iter()
            .filter_map(|window| window.downcast::<adw::Window>().ok())
            .find(|window| window.title().as_deref() == Some("Значение ячейки · text"))
            .unwrap();
        let content = window.content().unwrap();
        let scroll = content
            .last_child()
            .unwrap()
            .downcast::<gtk::ScrolledWindow>()
            .unwrap();
        let view = scroll.child().unwrap().downcast::<gtk::TextView>().unwrap();
        assert!(!view.is_editable());
        let buffer = view.buffer();
        assert_eq!(
            buffer.text(&buffer.start_iter(), &buffer.end_iter(), true),
            expected
        );
        let preceding = scroll.prev_sibling().unwrap();
        let button = preceding
            .clone()
            .downcast::<gtk::Box>()
            .ok()
            .and_then(|navigation| navigation.last_child())
            .and_then(|button| button.downcast::<gtk::Button>().ok());
        assert_eq!(button.is_some(), can_load);
        let note = if button.is_some() {
            preceding.prev_sibling().unwrap()
        } else {
            preceding
        }
        .downcast::<gtk::Label>()
        .ok();
        assert_eq!(
            note.as_ref()
                .map(|label| label.text().to_string())
                .as_deref(),
            truncated.then_some("Показана только загруженная часть значения")
        );
        if let Some(button) = button {
            button.emit_clicked();
            assert!(!button.is_sensitive());
            assert!(note.unwrap().text().contains("подключение закрыто"));
        }
        window.close();
    }

    let note = gtk::Label::new(Some("Показана только загруженная часть значения"));
    let view = gtk::TextView::new();
    let buffer = view.buffer();
    let previous = gtk::Button::with_label("Предыдущая");
    let next = gtk::Button::with_label("Загрузить из БД");
    let page = Rc::new(Cell::new(None));
    buffer.set_text("loaded portion");
    load_grid_cell(
        &ui.runtime,
        async { Err("Строка больше не найдена".into()) },
        &note,
        &view,
        (&previous, &next),
        &page,
        0,
    );
    assert!(!next.is_sensitive());
    until(|| next.is_sensitive());
    assert!(note.text().contains("Строка больше не найдена"));
    assert_eq!(
        buffer.text(&buffer.start_iter(), &buffer.end_iter(), true),
        "loaded portion"
    );

    load_grid_cell(
        &ui.runtime,
        async {
            Ok(CellValue::Truncated(
                "longer\nсерверное значение".to_owned(),
            ))
        },
        &note,
        &view,
        (&previous, &next),
        &page,
        0,
    );
    until(|| page.get() == Some(0));
    assert!(note.text().contains("страница 1"));
    assert!(!previous.is_sensitive());
    assert!(next.is_visible() && next.is_sensitive());
    assert_eq!(
        buffer.text(&buffer.start_iter(), &buffer.end_iter(), true),
        "longer\nсерверное значение",
    );

    load_grid_cell(
        &ui.runtime,
        async { Ok(CellValue::Text("Последняя часть".into())) },
        &note,
        &view,
        (&previous, &next),
        &page,
        1,
    );
    until(|| page.get() == Some(1));
    assert!(note.text().contains("страница 2"));
    assert!(previous.is_sensitive());
    assert!(!next.is_sensitive());
    assert_eq!(
        buffer.text(&buffer.start_iter(), &buffer.end_iter(), true),
        "Последняя часть",
    );

    load_grid_cell(
        &ui.runtime,
        async { Err("Сбой чтения".into()) },
        &note,
        &view,
        (&previous, &next),
        &page,
        0,
    );
    until(|| previous.is_sensitive());
    assert_eq!(page.get(), Some(1));
    assert!(!next.is_sensitive());
    assert_eq!(
        buffer.text(&buffer.start_iter(), &buffer.end_iter(), true),
        "Последняя часть",
    );
}

#[test]
#[ignore = "requires GTK display and local PostgreSQL; uses only a TEMP table"]
fn cell_preview_navigates_database_pages() {
    run_application_grid_scenarios(GridScenario::CellPreview);
}

#[test]
#[ignore = "requires GTK display and local Compose PostgreSQL; uses only a TEMP table"]
fn application_staged_save_discard_and_paging() {
    run_application_grid_scenarios(GridScenario::Paging);
}

#[test]
#[ignore = "requires GTK display and local PostgreSQL"]
fn application_related_rows_preserve_staged_edits() {
    run_application_grid_scenarios(GridScenario::Relations);
}

#[test]
#[ignore = "requires GTK display and local Compose PostgreSQL; uses only a TEMP table"]
fn keyboard_table_and_sql_commands() {
    run_application_grid_scenarios(GridScenario::Keyboard);
}

#[test]
#[ignore = "requires GTK display and local test PostgreSQL; uses only a TEMP table"]
fn saved_table_view_round_trip() {
    run_application_grid_scenarios(GridScenario::SavedViews);
}

fn first_widget_of_type<T: IsA<gtk::Widget>>(root: &gtk::Widget) -> Option<T> {
    if let Ok(widget) = root.clone().downcast::<T>() {
        return Some(widget);
    }
    let mut child = root.first_child();
    while let Some(widget) = child {
        if let Some(found) = first_widget_of_type(&widget) {
            return Some(found);
        }
        child = widget.next_sibling();
    }
    None
}

fn click_saved_view_action(ui: &Ui, key: &str, action: &str) {
    let (_, _, page) = find_table_page(ui, key).unwrap();
    let menu: gtk::MenuButton = named_widget(&page, "saved-table-views")
        .unwrap()
        .downcast()
        .unwrap();
    menu.popup();
    let button: gtk::Button = named_widget(menu.popover().unwrap().upcast_ref(), action)
        .unwrap()
        .downcast()
        .unwrap();
    assert!(button.is_sensitive(), "{action}");
    button.emit_clicked();
}

fn check_saved_table_views(ui: &Rc<Ui>, key: &str, object: &DbObject) {
    let profile = ui.current_profile.borrow().clone().unwrap();
    let scope = settings::TableViewScope::new(&profile, object);
    {
        let mut store = ui.store.borrow_mut();
        let store = store.as_mut().unwrap();
        store.unlock_or_create_vault("fixture password").unwrap();
        store.save_profile(profile, "").unwrap();
    }
    let panel = ui.table_filter_panels.borrow()[key].clone();
    panel.sql_mode.set_active(true);
    panel.raw.set_text("id <= 4");
    panel.apply.emit_clicked();
    until(|| ui.table_data.borrow()[key].rows.len() == 4);
    let grid = ui.tabulator_grids.borrow()[key].clone();
    until(|| grid.webview.is_sensitive());
    handle_tabulator_message(
        ui,
        key,
        r#"{"type":"sort","column":"c0","direction":"desc"}"#,
    );
    until(|| {
        grid.webview.is_sensitive()
            && ui.table_data.borrow()[key].rows[0][0] == CellValue::Text("4".to_owned())
    });
    let chooser = grid.column_chooser().popover().unwrap();
    let id_check: gtk::CheckButton = first_widget_of_type(chooser.upcast_ref()).unwrap();
    assert_eq!(id_check.label().as_deref(), Some("id"));
    id_check.set_active(false);
    // The form may contain a rejected WHERE. Saving must use the last successful read.
    panel.raw.set_text("missing_column = 1");
    panel.apply.emit_clicked();
    let mut error = None;
    until(|| {
        error = gtk::Window::list_toplevels()
            .into_iter()
            .filter_map(|widget| widget.downcast::<gtk::MessageDialog>().ok())
            .find(|dialog| {
                dialog.is_visible()
                    && dialog.text().as_deref() == Some("Не удалось прочитать таблицу")
            });
        error.is_some()
    });
    error.unwrap().close();
    click_saved_view_action(ui, key, "save-table-view");
    let dialog = gtk::Window::list_toplevels()
        .into_iter()
        .find(|widget| widget.widget_name() == "save-table-view-dialog")
        .unwrap();
    let name: gtk::Entry = named_widget(&dialog, "table-view-name")
        .unwrap()
        .downcast()
        .unwrap();
    name.set_text("Последние четыре");
    dialog
        .downcast::<gtk::Dialog>()
        .unwrap()
        .response(gtk::ResponseType::Accept);

    assert!(
        ui.store
            .borrow()
            .as_ref()
            .unwrap()
            .saved_table_views(&scope)
            .next()
            .is_some()
    );
    // A stale page response after close cannot clear a reopened tab's request state.
    ui.table_more_loading.borrow_mut().insert(key.to_owned());
    close_table_tab(ui, key);
    assert!(!ui.table_more_loading.borrow().contains(key));
    open_table(ui, object.clone(), true);
    until(|| ui.table_data.borrow().contains_key(key));
    let grid = ui.tabulator_grids.borrow()[key].clone();
    until(|| {
        js(
            &grid,
            "Boolean(typeof table !== 'undefined' && table && !updatingData)",
        ) == true
            && grid.webview.is_sensitive()
    });

    let edit = ui.table_edit_states.borrow()[key].clone();
    edit.pending
        .borrow_mut()
        .insert((0, 1), Some("pending".to_owned()));
    click_saved_view_action(ui, key, "apply-table-view");
    assert_eq!(edit.pending.borrow()[&(0, 1)].as_deref(), Some("pending"));
    let error = gtk::Window::list_toplevels()
        .into_iter()
        .filter_map(|widget| widget.downcast::<gtk::MessageDialog>().ok())
        .find(|dialog| dialog.text().as_deref() == Some("Не удалось применить вид"))
        .unwrap();
    error.close();
    edit.pending.borrow_mut().clear();

    // The JavaScript lock precedes GTK input blocking. Tabulator's own editor must honor it.
    assert_eq!(
        js(
            &grid,
            "window.dbdeckPrepareViewChange('test'); table.getRow(0).getCell('c1').edit(); Boolean(locked && !editingCell)"
        ),
        true
    );
    js(&grid, "window.dbdeckSetLocked(false); true");

    // A previously failed page request can leave the WebKit pagination flag set.
    js(&grid, "loadingMore = true; true");
    click_saved_view_action(ui, key, "apply-table-view");
    until(|| grid.webview.is_sensitive() && ui.table_data.borrow()[key].rows.len() == 4);
    let panel = ui.table_filter_panels.borrow()[key].clone();
    assert!(panel.sql_mode.is_active());
    assert_eq!(panel.raw.text(), "id <= 4");
    assert_eq!(panel.summary.text(), "WHERE id <= 4");
    assert_eq!(
        ui.table_data.borrow()[key].rows[0][0],
        CellValue::Text("4".to_owned())
    );
    assert_eq!(grid.hidden_column_names(), ["id"]);
    assert_eq!(js(&grid, "table.getColumn('c0').isVisible()"), false);
    assert_eq!(
        js(
            &grid,
            "table.getColumn('c0').getElement().getAttribute('aria-sort')"
        ),
        "descending"
    );

    let mut invalid = ui
        .store
        .borrow()
        .as_ref()
        .unwrap()
        .saved_table_views(&scope)
        .next()
        .unwrap()
        .clone();
    invalid.layout.view.sort.as_mut().unwrap().column = "removed_column".to_owned();
    ui.store
        .borrow_mut()
        .as_mut()
        .unwrap()
        .save_table_view(invalid)
        .unwrap();
    click_saved_view_action(ui, key, "apply-table-view");
    until(|| {
        gtk::Window::list_toplevels()
            .into_iter()
            .filter_map(|widget| widget.downcast::<gtk::MessageDialog>().ok())
            .any(|dialog| dialog.text().as_deref() == Some("Не удалось прочитать таблицу"))
    });
    assert_eq!(
        ui.table_view_states.borrow()[key]
            .sort
            .as_ref()
            .unwrap()
            .column,
        "id"
    );
    assert_eq!(ui.table_data.borrow()[key].rows.len(), 4);
    assert!(grid.webview.is_sensitive());
    for dialog in gtk::Window::list_toplevels()
        .into_iter()
        .filter_map(|widget| widget.downcast::<gtk::MessageDialog>().ok())
    {
        dialog.close();
    }
    click_saved_view_action(ui, key, "delete-table-view");
    let confirmation = gtk::Window::list_toplevels()
        .into_iter()
        .filter_map(|widget| widget.downcast::<gtk::MessageDialog>().ok())
        .find(|dialog| dialog.text().as_deref() == Some("Удалить вид «Последние четыре»?"))
        .unwrap();
    confirmation.response(gtk::ResponseType::Yes);
    assert!(
        ui.store
            .borrow()
            .as_ref()
            .unwrap()
            .saved_table_views(&scope)
            .next()
            .is_none()
    );
}

#[derive(PartialEq, Eq)]
enum GridScenario {
    Paging,
    Keyboard,
    SavedViews,
    Relations,
    CellPreview,
}

fn run_application_grid_scenarios(scenario: GridScenario) {
    let keyboard_only = scenario == GridScenario::Keyboard;
    gtk::init().unwrap();
    adw::init().unwrap();
    let runtime = Arc::new(
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap(),
    );
    let (pool, object) = runtime.block_on(async {
        let options = sqlx::postgres::PgConnectOptions::new()
            .port(55432).database("dbdeck_test").username("dbdeck_test");
        let options = if let Some(socket) = std::env::var_os("DBDECK_PG_TEST_SOCKET") {
            options.socket(socket)
        } else {
            options.host("127.0.0.1")
                .password(&std::env::var("DBDECK_PG_PASSWORD").expect("set DBDECK_PG_PASSWORD from .env"))
        };
        let pool = sqlx::postgres::PgPoolOptions::new().max_connections(1)
            .idle_timeout(None).max_lifetime(None).connect_with(options).await.unwrap();
        sqlx::query("CREATE TEMP TABLE dbdeck_grid_test (id integer PRIMARY KEY, value text NOT NULL)").execute(&pool).await.unwrap();
        sqlx::query("INSERT INTO dbdeck_grid_test SELECT i, 'original-' || i FROM generate_series(1, 260) i").execute(&pool).await.unwrap();
        if scenario == GridScenario::CellPreview {
            sqlx::query("UPDATE dbdeck_grid_test SET value = REPEAT('ю', 16384) || REPEAT('я', 16384) || 'конец' WHERE id = 1")
                .execute(&pool).await.unwrap();
        }
        if scenario == GridScenario::Relations {
            sqlx::query("CREATE TEMP TABLE dbdeck_grid_child (id integer PRIMARY KEY, parent integer REFERENCES dbdeck_grid_test(id))")
                .execute(&pool).await.unwrap();
            sqlx::query("INSERT INTO dbdeck_grid_child VALUES (10, 1), (20, 2)")
                .execute(&pool).await.unwrap();
        }
        let schema: String = sqlx::query_scalar("SELECT pg_my_temp_schema()::regnamespace::text").fetch_one(&pool).await.unwrap();
        (pool, DbObject {schema, name:"dbdeck_grid_test".to_owned(), kind:database::DbObjectKind::Table})
    });
    let app = adw::Application::builder()
        .application_id("io.github.dbdeck.grid-test")
        .build();
    app.register(None::<&gtk::gio::Cancellable>).unwrap();
    let ui = build_window(&app, runtime.clone());
    let profile = ConnectionProfile {
        id: "grid-test".to_owned(),
        name: "grid-test".to_owned(),
        kind: DatabaseKind::PostgreSql,
        host: "127.0.0.1".to_owned(),
        port: 55432,
        database: "dbdeck_test".to_owned(),
        username: "dbdeck_test".to_owned(),
        environment: EnvironmentTag::Local,
        color: "#4f8fe8".to_owned(),
        read_only: false,
        tls_mode: TlsMode::Disabled,
        password_saved: false,
        gopass_entry: None,
        tls_files: Default::default(),
        native_tools_dir: None,
        transport: Default::default(),
    };
    let session_key = profile_session_key(&profile);
    let group_key = workspace_ui::key(&profile, Some(&object.schema));
    let key = format!("{}:{}.{}", session_key, object.schema, object.name);
    ui.sessions.borrow_mut().insert(
        session_key.clone(),
        (profile.clone(), DbPool::PostgreSql(pool.clone())),
    );
    *ui.current_profile.borrow_mut() = Some(profile);
    *ui.pool.borrow_mut() = Some(DbPool::PostgreSql(pool.clone()));
    ui.landing.set_visible(false);
    open_table(&ui, object.clone(), true);
    until(|| ui.table_data.borrow().get(&key).is_some());
    let grid = ui.tabulator_grids.borrow().get(&key).unwrap().clone();
    until(|| {
        js(
            &grid,
            "Boolean(typeof table !== 'undefined' && table && !updatingData)",
        ) == true
    });
    if scenario == GridScenario::CellPreview {
        let generation = grid.generation().unwrap();
        handle_tabulator_message(
            &ui,
            &key,
            &serde_json::json!({"type": "viewCell", "row": 0, "column": "c1", "generation": generation}).to_string(),
        );
        let window = gtk::Window::list_toplevels()
            .into_iter()
            .filter_map(|window| window.downcast::<adw::Window>().ok())
            .find(|window| window.title().as_deref() == Some("Значение ячейки · value"))
            .unwrap();
        let content = window.content().unwrap();
        let scroll = content
            .last_child()
            .unwrap()
            .downcast::<gtk::ScrolledWindow>()
            .unwrap();
        let view = scroll.child().unwrap().downcast::<gtk::TextView>().unwrap();
        let buffer = view.buffer();
        let navigation = scroll.prev_sibling().unwrap();
        let previous = navigation
            .first_child()
            .unwrap()
            .downcast::<gtk::Button>()
            .unwrap();
        let next = navigation
            .last_child()
            .unwrap()
            .downcast::<gtk::Button>()
            .unwrap();
        assert_eq!(next.label().as_deref(), Some("Загрузить из БД"));
        next.emit_clicked();
        until(|| buffer.char_count() as usize == database::MAX_CELL_DETAIL_CHARS);
        assert!(!previous.is_sensitive() && next.is_sensitive());
        let vertical = scroll.vadjustment();
        until(|| vertical.upper() > vertical.page_size());
        vertical.set_value(vertical.upper() - vertical.page_size());
        assert!(vertical.value() > 0.0);
        next.emit_clicked();
        until(|| buffer.start_iter().char() == 'я');
        // GtkTextView may align the first line just below the top padding.
        until(|| vertical.value() <= f64::from(view.top_margin()));
        assert!(previous.is_sensitive() && next.is_sensitive());
        next.emit_clicked();
        until(|| buffer.text(&buffer.start_iter(), &buffer.end_iter(), false) == "конец");
        assert!(previous.is_sensitive() && !next.is_sensitive());
        previous.emit_clicked();
        until(|| buffer.start_iter().char() == 'я');
        assert!(previous.is_sensitive() && next.is_sensitive());
        previous.emit_clicked();
        until(|| buffer.start_iter().char() == 'ю');
        assert!(!previous.is_sensitive() && next.is_sensitive());
        window.close();
        ui.window.close();
        return;
    }
    if scenario == GridScenario::SavedViews {
        check_saved_table_views(&ui, &key, &object);
        ui.window.close();
        return;
    }
    if scenario == GridScenario::Relations {
        let state = ui.table_edit_states.borrow()[&key].clone();
        handle_tabulator_message(
            &ui,
            &key,
            r#"{"type":"edit","row":0,"column":"c1","value":"staged"}"#,
        );
        assert!(!state.pending.borrow().is_empty());

        js(
            &grid,
            "table.getRow(0).getCell('c1').getElement().dispatchEvent(new MouseEvent('contextmenu', {bubbles:true, cancelable:true, button:2, clientX:200, clientY:200})); true",
        );
        until(|| js(&grid, "Boolean(document.querySelector('.tabulator-menu'))") == true);
        assert_eq!(
            js(
                &grid,
                "[...document.querySelectorAll('.tabulator-menu-item')].find(item => item.textContent === 'Связанные строки…').click(); true"
            ),
            true
        );
        let mut dialog = None;
        until(|| {
            dialog = gtk::Window::list_toplevels()
                .into_iter()
                .filter_map(|widget| widget.downcast::<gtk::Window>().ok())
                .find(|window| window.is_visible() && window.widget_name() == "table-relations");
            dialog.is_some()
        });
        let dialog = dialog.unwrap();
        let status = named_widget(dialog.upcast_ref(), "table-relations-status")
            .unwrap()
            .downcast::<gtk::Label>()
            .unwrap();
        let show = named_widget(dialog.upcast_ref(), "table-relations-show")
            .unwrap()
            .downcast::<gtk::Button>()
            .unwrap();
        until(|| show.is_sensitive() || status.text().starts_with("Не удалось"));
        assert!(show.is_sensitive(), "{}", status.text());
        show.emit_clicked();
        until(|| show.is_sensitive());
        assert_eq!(status.text(), "Строк: 1");
        assert_eq!(
            state.pending.borrow().get(&(0, 1)),
            Some(&Some("staged".to_owned()))
        );
        assert_eq!(
            ui.tabulator_grids.borrow()[&key].generation(),
            grid.generation()
        );
        dialog.close();
        ui.window.close();
        return;
    }
    println!("CHECKPOINT initial grid ready");
    let initial_panel = ui.table_filter_panels.borrow()[&key].clone();
    assert!(!initial_panel.toggle.is_active());
    assert!(!initial_panel.summary.is_visible());
    layout_snapshot(&ui.window, "target/table-compact-filters.png");
    initial_panel.toggle.emit_clicked();
    assert!(initial_panel.toggle.is_active());
    until(|| initial_panel.controls.borrow()[0].column.width() > 0);
    let single_gap = {
        let rows = initial_panel.controls.borrow();
        let row = rows[0].row.compute_bounds(&ui.window).unwrap();
        let table = grid.webview.compute_bounds(&ui.window).unwrap();
        let column = rows[0].column.compute_bounds(&ui.window).unwrap();
        assert!(
            (column.x() - table.x()).abs() <= 1.0,
            "single filter starts at table edge"
        );
        let gap = table.y() - row.y() - row.height();
        assert!(
            gap <= 6.0,
            "single-row filter has excess bottom padding: {gap}"
        );
        gap
    };
    layout_snapshot(&ui.window, "target/table-single-filter.png");
    assert!(shortcuts::dispatch(
        &ui,
        gtk::gdk::Key::l,
        gtk::gdk::ModifierType::CONTROL_MASK
    ));
    let filter_entry = ui.table_filter_panels.borrow()[&key].controls.borrow()[0]
        .value
        .clone();
    let focus = gtk::prelude::RootExt::focus(&ui.window).unwrap();
    assert!(
        focus == filter_entry.clone().upcast::<gtk::Widget>() || focus.is_ancestor(&filter_entry)
    );
    assert!(shortcuts::dispatch(
        &ui,
        gtk::gdk::Key::_1,
        gtk::gdk::ModifierType::CONTROL_MASK
    ));
    let focus = gtk::prelude::RootExt::focus(&ui.window).unwrap();
    assert!(
        focus == grid.webview.clone().upcast::<gtk::Widget>() || focus.is_ancestor(&grid.webview)
    );

    let state = ui.table_edit_states.borrow().get(&key).unwrap().clone();
    assert!(shortcuts::dispatch(
        &ui,
        gtk::gdk::Key::r,
        gtk::gdk::ModifierType::CONTROL_MASK
    ));
    assert!(
        !grid.webview.is_sensitive(),
        "refresh must lock immediately"
    );
    handle_tabulator_message(
        &ui,
        &key,
        r#"{"type":"edit","row":0,"column":"c1","value":"late-edit"}"#,
    );
    assert!(
        state.pending.borrow().is_empty(),
        "late edit during refresh must be rejected"
    );
    until(|| grid.webview.is_sensitive() && js(&grid, "!updatingData") == true);
    assert!(state.pending.borrow().is_empty());

    assert!(
        !state.toolbar.is_visible(),
        "no pending changes must hide edit actions"
    );
    js(&grid, "table.getRow(0).getCell('c1').edit(); true");
    assert!(
        !state.toolbar.is_visible(),
        "opening an unchanged editor must not show Save"
    );
    js(&grid, "table.getRow(0).getCell('c1').cancelEdit(); true");
    assert_eq!(js(&grid, "table.getRanges().length"), 0);
    js(
        &grid,
        "table.addRange(table.getRow(0).getCell('c1'),table.getRow(0).getCell('c1')); true",
    );
    until(|| js(&grid, "table.getRanges()[0]?.getRows().length === 1") == true);
    js(&grid, "window.dbdeckPaste('staged'); true");
    until(|| {
        state
            .pending
            .borrow()
            .get(&(0, 1))
            .is_some_and(|v| v.as_deref() == Some("staged"))
    });
    println!("CHECKPOINT staged edit reached Rust");
    let read_value = || {
        runtime
            .block_on(
                sqlx::query_scalar::<_, String>("SELECT value FROM dbdeck_grid_test WHERE id=1")
                    .fetch_one(&pool),
            )
            .unwrap()
    };
    assert_eq!(read_value(), "original-1", "staging must not write to DB");
    assert!(state.toolbar.is_visible());
    println!("CHECKPOINT discard request");
    assert!(shortcuts::dispatch(
        &ui,
        gtk::gdk::Key::BackSpace,
        gtk::gdk::ModifierType::CONTROL_MASK | gtk::gdk::ModifierType::ALT_MASK
    ));
    until(|| {
        state.pending.borrow().is_empty()
            && js(&grid, "table.getRow(0).getData().c1") == "original-1"
    });
    assert!(!state.toolbar.is_visible());
    assert_eq!(read_value(), "original-1", "discard must not write to DB");
    js(
        &grid,
        "table.getRow(0).getCell('c1').edit(); document.querySelector('.tabulator-editing input').value='saved-inline'; document.querySelector('.tabulator-editing input').dispatchEvent(new Event('input')); true",
    );
    until(|| state.grid_editing.get() && state.save_button.is_sensitive());
    println!("CHECKPOINT save active editor request");
    assert!(shortcuts::dispatch(
        &ui,
        gtk::gdk::Key::s,
        gtk::gdk::ModifierType::CONTROL_MASK
    ));
    let (window, accept) = edits_preview();
    assert!(preview_details(&window).contains("Станет: \"saved-inline\""));
    assert_eq!(read_value(), "original-1");
    accept.emit_clicked();
    until(|| state.saving.get());
    assert!(
        !grid.webview.is_sensitive(),
        "saving must lock the real WebView"
    );
    until(|| {
        !state.saving.get() && state.pending.borrow().is_empty() && grid.webview.is_sensitive()
    });
    println!("CHECKPOINT save completed");
    assert_eq!(read_value(), "saved-inline");
    until(|| {
        js(
            &grid,
            "!updatingData && table.getRow(0).getData().c1 === 'saved-inline'",
        ) == true
    });
    js(
        &grid,
        "table.getRow(0).getCell('c1').edit(); document.querySelector('.tabulator-editing input').value='discard-inline'; document.querySelector('.tabulator-editing input').dispatchEvent(new Event('input')); true",
    );
    until(|| state.grid_editing.get());
    // Wait for Discard's replacement, rather than the earlier editor-cancel signal.
    js(
        &grid,
        "window.testDiscardDone=false; window.testDiscardSetData=window.dbdeckSetData; window.dbdeckSetData=(...args)=>window.testDiscardSetData(...args).then(()=>{window.testDiscardDone=true}); true",
    );
    assert!(shortcuts::dispatch(
        &ui,
        gtk::gdk::Key::BackSpace,
        gtk::gdk::ModifierType::CONTROL_MASK | gtk::gdk::ModifierType::ALT_MASK
    ));
    until(|| {
        !state.grid_editing.get()
            && js(
                &grid,
                "window.testDiscardDone && !updatingData && table.getRow(0).getData().c1 === 'saved-inline'",
            ) == true
    });
    js(
        &grid,
        "window.dbdeckSetData=window.testDiscardSetData; true",
    );
    assert_eq!(read_value(), "saved-inline");
    let applied_view = state.loaded_view.borrow().clone().unwrap();
    let mut failing_view = applied_view.clone();
    failing_view.raw_condition = Some("missing_column = 1".to_owned());
    state.loaded_view.replace(Some(failing_view));
    js(
        &grid,
        "table.scrollToRow(199); document.querySelector('.tabulator-tableholder').dispatchEvent(new Event('scroll')); true",
    );
    until(|| {
        ui.status
            .text()
            .starts_with("Не удалось загрузить следующую страницу")
            && js(
                &grid,
                "!loadingMore && hasMore && table.getDataCount() === 200",
            ) == true
    });
    state.loaded_view.replace(Some(applied_view));
    // A failed replacement view can leave draft filters different from the displayed rows.
    ui.table_view_states
        .borrow_mut()
        .get_mut(&key)
        .unwrap()
        .raw_condition = Some("missing_column = 1".to_owned());
    js(
        &grid,
        "table.addRange(table.getRow(0).getCell('c1'),table.getRow(1).getCell('c1')); table.scrollToRow(199); document.querySelector('.tabulator-tableholder').dispatchEvent(new Event('scroll')); true",
    );
    until(|| {
        ui.table_data
            .borrow()
            .get(&key)
            .is_some_and(|data| data.rows.len() == 260)
    });
    until(|| {
        js(
            &grid,
            "!updatingData && table.getDataCount() === 260 && table.getRanges()[0]?.getRows().length === 2",
        ) == true
    });
    ui.table_view_states
        .borrow_mut()
        .get_mut(&key)
        .unwrap()
        .raw_condition = None;
    js(
        &grid,
        "table.getColumn('c0').getElement().querySelector('.tabulator-col-sorter').click(); true",
    );
    until(|| {
        js(
            &grid,
            "!updatingData && table.getColumn('c0').getElement().getAttribute('aria-sort') === 'ascending'",
        ) == true
    });
    js(
        &grid,
        "table.getColumn('c0').getElement().querySelector('.tabulator-col-sorter').click(); true",
    );
    until(|| {
        js(
            &grid,
            "!updatingData && table.getColumn('c0').getElement().getAttribute('aria-sort') === 'descending' && table.getRow(0).getData().c0 === '260'",
        ) == true
    });
    js(
        &grid,
        "table.scrollToRow(199); document.querySelector('.tabulator-tableholder').dispatchEvent(new Event('scroll')); true",
    );
    until(|| {
        js(
            &grid,
            "!updatingData && table.getDataCount() === 260 && table.getRow(200).getData().c0 === '60' && table.getRow(259).getData().c0 === '1'",
        ) == true
    });
    println!("Server sorting and acknowledged arrow directions: passed");
    println!(
        "Full application: staging, unchanged DB before save, discard, active-editor save, locked WebView, DB persistence, active-editor discard, 200→260 server paging and preserved selection: passed"
    );
    let panel = ui.table_filter_panels.borrow()[&key].clone();
    panel.sql_mode.set_active(true);
    panel
        .raw
        .set_text("WHERE id != 4 AND (id > 255 OR value = 'saved-inline')");
    assert!(shortcuts::dispatch(
        &ui,
        gtk::gdk::Key::Return,
        gtk::gdk::ModifierType::CONTROL_MASK
    ));
    until(|| ui.table_data.borrow()[&key].rows.len() == 6);
    assert!(
        ui.table_data.borrow()[&key]
            .rows
            .iter()
            .all(|row| row[0] != CellValue::Text("4".into()))
    );
    until(|| js(&grid, "table.getRows().length === 6 && !updatingData") == true);
    if keyboard_only {
        shortcuts::palette(&ui);
        let palette = gtk::Window::list_toplevels()
            .into_iter()
            .filter_map(|widget| widget.downcast::<adw::Window>().ok())
            .find(|window| {
                window.title().as_deref() == Some("Палитра команд") && window.is_visible()
            })
            .unwrap();
        until(|| palette.is_mapped());
        let search = named_widget(palette.upcast_ref(), "command-search")
            .unwrap()
            .downcast::<gtk::SearchEntry>()
            .unwrap();
        let list = named_widget(palette.upcast_ref(), "command-list")
            .unwrap()
            .downcast::<gtk::ListBox>()
            .unwrap();
        search.set_text("формат");
        assert!(list.selected_row().is_none());
        search.set_text("фильтр применить");
        assert!(list.selected_row().is_some());
        panel.raw.set_text("WHERE id = 255");
        search.emit_activate();
        assert!(!palette.is_visible());
        until(|| ui.table_data.borrow()[&key].rows.len() == 1);
        assert_eq!(
            ui.table_data.borrow()[&key].rows[0][0],
            CellValue::Text("255".into())
        );

        // Closing a SQL tab saves drafts, which needs an unlocked vault.
        ui.store
            .borrow_mut()
            .as_mut()
            .unwrap()
            .unlock_or_create_vault("synthetic keyboard test key")
            .unwrap();
        create_sql_tab(&ui, &session_key);
        let sql_id = ui.sql_tabs.borrow().keys().next().unwrap().clone();
        if let Ok(helper) = std::env::var("DBDECK_POINTER_HELPER") {
            ui.window.maximize();
            until(|| ui.window.is_maximized() && ui.window.width() == 1600);
            let page = ui.sql_tabs.borrow()[&sql_id].page.clone();
            let split = page
                .first_child()
                .unwrap()
                .downcast::<gtk::Paned>()
                .unwrap();
            until(|| split.is_mapped() && split.height() > 400);
            let bounds = split.compute_bounds(&ui.window).unwrap();
            let start = split
                .start_child()
                .unwrap()
                .compute_bounds(&ui.window)
                .unwrap();
            let end = split
                .end_child()
                .unwrap()
                .compute_bounds(&ui.window)
                .unwrap();
            let x = bounds.x() + bounds.width() / 2.0;
            let y = (start.y() + start.height() + end.y()) / 2.0;
            let before = split.position();
            let mut process = std::process::Command::new(helper)
                .args([x.to_string(), y.to_string(), "100".to_owned()])
                .spawn()
                .unwrap();
            until(|| process.try_wait().unwrap().is_some());
            until(|| split.position() >= before + 80);
            println!("Real pointer SQL drag: {before} -> {}", split.position());
            assert_eq!(ui.layout.borrow().sql_position, split.position());
            ui.layout.borrow().save().unwrap();
            assert_eq!(ui_layout::Layout::load().sql_position, split.position());
            layout_snapshot(&ui.window, "target/sql-splitter-dragged.png");
            ui.window.unmaximize();
            until(|| !ui.window.is_maximized());
        }

        ui.sql_tabs.borrow()[&sql_id]
            .buffer
            .set_text("SELECT 1 AS keyboard_test");
        assert!(shortcuts::dispatch(
            &ui,
            gtk::gdk::Key::Return,
            gtk::gdk::ModifierType::CONTROL_MASK
        ));
        until(|| ui.sql_tabs.borrow()[&sql_id].result.is_some());
        assert_eq!(
            ui.sql_tabs.borrow()[&sql_id]
                .result
                .as_ref()
                .unwrap()
                .columns,
            ["keyboard_test"]
        );
        assert!(shortcuts::dispatch(
            &ui,
            gtk::gdk::Key::Tab,
            gtk::gdk::ModifierType::CONTROL_MASK | gtk::gdk::ModifierType::SHIFT_MASK
        ));
        assert_eq!(
            ui.session_table_tabs.borrow()[&group_key].current_page(),
            Some(0)
        );
        assert!(shortcuts::dispatch(
            &ui,
            gtk::gdk::Key::Tab,
            gtk::gdk::ModifierType::CONTROL_MASK
        ));
        assert_eq!(
            ui.session_table_tabs.borrow()[&group_key].current_page(),
            Some(1)
        );
        assert!(shortcuts::dispatch(
            &ui,
            gtk::gdk::Key::w,
            gtk::gdk::ModifierType::CONTROL_MASK
        ));
        assert!(
            !ui.sql_tabs.borrow().contains_key(&sql_id),
            "Ctrl+W kept the SQL tab: status={:?}, tab status={:?}",
            ui.status.text(),
            ui.sql_tabs
                .borrow()
                .get(&sql_id)
                .map(|tab| tab.status.text())
        );
        ui.window.close();
        runtime.block_on(pool.close());
        return;
    }
    fn strip_subtitles(widget: &gtk::Widget, found: &mut Vec<String>) {
        if let Some(label) = widget.downcast_ref::<gtk::Label>()
            && label.has_css_class("tab-strip-subtitle")
        {
            found.push(label.text().to_string());
        }
        let mut child = widget.first_child();
        while let Some(current) = child {
            strip_subtitles(&current, found);
            child = current.next_sibling();
        }
    }
    until(|| {
        let mut subtitles = Vec::new();
        strip_subtitles(ui.tab_strip.row.upcast_ref(), &mut subtitles);
        subtitles
            .iter()
            .any(|text| text == "grid-test / dbdeck_test")
    });
    assert!(
        ui.tab_strip.add_button.is_sensitive(),
        "new SQL tab button is enabled with an open connection"
    );
    // The tab stays left of "+" instead of overlapping it.
    let tab = ui.tab_strip.row.first_child().expect("strip shows the tab");
    until(|| tab.width() > 0 && ui.tab_strip.add_button.width() > 0);
    let tab_bounds = tab.compute_bounds(&ui.tab_strip.root).unwrap();
    let add_bounds = ui
        .tab_strip
        .add_button
        .compute_bounds(&ui.tab_strip.root)
        .unwrap();
    assert!(
        tab_bounds.x() >= 0.0 && tab_bounds.x() + tab_bounds.width() <= add_bounds.x(),
        "tab overlaps the new SQL tab button: {tab_bounds:?} vs {add_bounds:?}"
    );
    panel.toggle.set_active(true);
    until(|| panel.raw.is_mapped() && panel.raw.width() > 0);
    let apply_icon = panel
        .apply
        .child()
        .unwrap()
        .downcast::<gtk::Image>()
        .unwrap();
    let settings = gtk::prelude::WidgetExt::settings(&panel.apply);
    let animations = settings.property::<bool>("gtk-enable-animations");
    settings.set_property("gtk-enable-animations", false);
    panel
        .apply
        .set_state_flags(gtk::StateFlags::PRELIGHT, false);
    assert_eq!(
        apply_icon.style_context().color(),
        panel.apply.style_context().color(),
        "filter icon should use the button foreground on hover"
    );
    panel.apply.unset_state_flags(gtk::StateFlags::PRELIGHT);
    settings.set_property("gtk-enable-animations", animations);
    let raw_bounds = panel.raw.compute_bounds(&ui.window).unwrap();
    let table_bounds = grid.webview.compute_bounds(&ui.window).unwrap();
    assert!(
        (raw_bounds.x() - table_bounds.x()).abs() <= 1.0,
        "SQL input starts at table edge"
    );
    assert_eq!(
        panel.raw.style_context().border().left(),
        1,
        "SQL border is 1px"
    );
    layout_snapshot(&ui.window, "target/table-raw-filter.png");
    // The applied raw predicate also survives subsequent server sorting/paging.
    panel.clear.emit_clicked();
    until(|| ui.table_data.borrow()[&key].rows.len() == 200);
    assert!(ui.table_view_states.borrow()[&key].raw_condition.is_none());
    panel.sql_mode.set_active(false);
    {
        let controls = panel.controls.borrow();
        controls[0].operator.set_active(Some(6));
        controls[0].value.set_text("258");
    }
    panel.add.emit_clicked();
    {
        let controls = panel.controls.borrow();
        controls[1].operator.set_active(Some(0));
        controls[1].logic.set_active(Some(1));
        controls[1].value.set_text("1");
    }
    panel.apply.emit_clicked();
    until(|| ui.table_data.borrow()[&key].rows.len() == 3);
    assert!(ui.table_view_states.borrow()[&key].raw_condition.is_none());
    assert_eq!(ui.table_view_states.borrow()[&key].filters.len(), 2);
    until(|| js(&grid, "table.getRows().length === 3 && !updatingData") == true);
    assert!(!panel.toggle.is_active());
    assert!(panel.summary.is_visible());
    assert!(panel.summary.text().contains(" OR "));
    layout_snapshot(&ui.window, "target/table-active-filters.png");
    ui.window.set_default_size(930, 600);
    until(|| ui.window.width() == 930);
    layout_snapshot(&ui.window, "target/table-active-filters-narrow.png");
    ui.window.set_default_size(1280, 820);
    until(|| ui.window.width() == 1280);
    until(|| js(&grid, "table.getRows().length === 3 && !updatingData") == true);
    for _ in 0..2 {
        panel.add.emit_clicked();
    }
    until(|| panel.controls.borrow().last().unwrap().value.width() > 0);
    {
        let rows = panel.controls.borrow();
        for row in rows.iter().skip(1) {
            for (first, current) in [
                (
                    rows[0].column.upcast_ref::<gtk::Widget>(),
                    row.column.upcast_ref::<gtk::Widget>(),
                ),
                (
                    rows[0].operator.upcast_ref::<gtk::Widget>(),
                    row.operator.upcast_ref::<gtk::Widget>(),
                ),
                (
                    rows[0].value.upcast_ref::<gtk::Widget>(),
                    row.value.upcast_ref::<gtk::Widget>(),
                ),
                (
                    rows[0].remove.upcast_ref::<gtk::Widget>(),
                    row.remove.upcast_ref::<gtk::Widget>(),
                ),
            ] {
                let first = first.compute_bounds(&ui.window).unwrap();
                let current = current.compute_bounds(&ui.window).unwrap();
                assert!(
                    (first.x() - current.x()).abs() < 1.0,
                    "filter columns must align: {first:?} vs {current:?}"
                );
                assert!((first.width() - current.width()).abs() < 1.0);
            }
        }
        let first_row = rows[0].row.compute_bounds(&ui.window).unwrap();
        for action in [&panel.add, &panel.apply] {
            let bounds = action.compute_bounds(&ui.window).unwrap();
            assert!(
                (bounds.y() + bounds.height() / 2.0 - first_row.y() - first_row.height() / 2.0)
                    .abs()
                    <= 1.0,
                "filter actions must align with the first row: {bounds:?} vs {first_row:?}"
            );
        }
        let filter_bounds = rows[0].row.compute_bounds(&ui.window).unwrap();
        let grid_bounds = grid.webview.compute_bounds(&ui.window).unwrap();
        let mode_bounds = panel.sql_mode.compute_bounds(&ui.window).unwrap();
        let column_bounds = rows[0].column.compute_bounds(&ui.window).unwrap();
        assert!(
            (column_bounds.x() - grid_bounds.x()).abs() <= 1.0,
            "multiple filters start at table edge"
        );
        let last = rows.last().unwrap().row.compute_bounds(&ui.window).unwrap();
        let multiple_gap = grid_bounds.y() - last.y() - last.height();
        assert!(
            (single_gap - multiple_gap).abs() <= 1.0,
            "bottom padding must match: single={single_gap}, multiple={multiple_gap}"
        );
        assert_eq!(
            rows[0].value.style_context().border().left(),
            1,
            "value border is 1px"
        );

        assert!(
            filter_bounds.y() < grid_bounds.y(),
            "filters must be above column headers"
        );
        assert!(
            mode_bounds.y() < grid_bounds.y(),
            "SQL mode belongs to the filter builder"
        );
        assert!(panel.sql_mode.has_css_class("table-sql-mode"));
    }
    layout_snapshot(&ui.window, "target/table-structured-filter.png");
    panel.clear.emit_clicked();
    until(|| ui.table_data.borrow()[&key].rows.len() == 200);
    println!("Raw/structured filters, server sort, reset and dirty-only edit controls: passed");
    create_sql_tab(&ui, &session_key);
    let sql_id = ui.sql_tabs.borrow().keys().next().unwrap().clone();
    assert!(shortcuts::dispatch(
        &ui,
        gtk::gdk::Key::Tab,
        gtk::gdk::ModifierType::CONTROL_MASK | gtk::gdk::ModifierType::SHIFT_MASK
    ));
    assert!(ui.table_filter_panels.borrow().contains_key(&key));
    assert!(shortcuts::dispatch(
        &ui,
        gtk::gdk::Key::Tab,
        gtk::gdk::ModifierType::CONTROL_MASK
    ));
    assert_eq!(
        ui.session_table_tabs.borrow()[&group_key].current_page(),
        Some(1)
    );

    let (sql_buffer, sql_indicator) = {
        let tabs = ui.sql_tabs.borrow();
        (
            tabs[&sql_id].buffer.clone(),
            tabs[&sql_id].risk_indicator.clone(),
        )
    };
    let theme_provider = install_css();
    for (mode, expected) in [
        (ThemeMode::CatppuccinMocha, "dbdeck-mocha"),
        (ThemeMode::Dark, "dbdeck-dark"),
        (ThemeMode::TokyoNight, "dbdeck-tokyo-night"),
        (ThemeMode::GruvboxDark, "dbdeck-gruvbox-dark"),
        (ThemeMode::Nord, "dbdeck-nord"),
        (ThemeMode::RosePine, "dbdeck-rose-pine"),
        (ThemeMode::Noctalia, "dbdeck-noctalia"),
        (ThemeMode::System, "Adwaita"),
        (ThemeMode::CatppuccinMocha, "dbdeck-mocha"),
    ] {
        ui.active_theme.set(mode);
        theme::apply_mode(mode, &theme_provider);
        refresh_sql_editor_themes(&ui);
        let color = gtk::gdk::RGBA::parse(theme::palette(mode).bg).unwrap();
        let expected_background = format!(
            "rgb({}, {}, {})",
            (color.red() * 255.0).round() as u32,
            (color.green() * 255.0).round() as u32,
            (color.blue() * 255.0).round() as u32
        );
        until(|| {
            js(&grid, "getComputedStyle(document.body).backgroundColor") == expected_background
        });
        let scheme = sql_buffer.style_scheme().unwrap();
        let expected = if mode == ThemeMode::System {
            theme::sql_scheme_id(mode, adw::StyleManager::default().is_dark())
        } else {
            expected
        };
        assert_eq!(scheme.id(), expected);
        assert_eq!(sql_buffer.language().unwrap().id(), "sql");
        let text = "-- comment\nselect id, 'active', 42 from sample_records where id > 4;";
        sql_buffer.set_text(text);
        sql_buffer.ensure_highlight(&sql_buffer.start_iter(), &sql_buffer.end_iter());
        let colors: Vec<_> = ["comment", "select", "active", "42"]
            .iter()
            .map(|token| {
                let offset = text[..text.find(token).unwrap()].chars().count() as i32;
                sql_buffer
                    .iter_at_offset(offset)
                    .tags()
                    .iter()
                    .filter(|tag| tag.is_foreground_set())
                    .filter_map(|tag| tag.foreground_rgba())
                    .last()
                    .expect("syntax token must have a color")
            })
            .collect();
        assert_ne!(colors[0], colors[1], "comments and keywords must differ");
        assert_ne!(colors[1], colors[2], "keywords and strings must differ");
        assert_ne!(colors[2], colors[3], "strings and numbers must differ");
        let frame = Rc::new(Cell::new(false));
        let rendered = frame.clone();
        gtk::glib::timeout_add_local_once(Duration::from_millis(80), move || rendered.set(true));
        until(|| frame.get());
        layout_snapshot(&ui.window, &format!("target/sql-editor-{}.png", mode.id()));
        if mode == ThemeMode::Nord {
            ui.window.set_default_size(930, 600);
            until(|| ui.window.width() == 930);
            layout_snapshot(&ui.window, "target/sql-editor-nord-narrow.png");
            ui.window.set_default_size(1280, 820);
            until(|| ui.window.width() == 1280);
        }
        if mode == ThemeMode::System {
            let manager = adw::StyleManager::default();
            manager.set_color_scheme(adw::ColorScheme::ForceLight);
            assert_eq!(sql_buffer.style_scheme().unwrap().id(), "Adwaita");
            manager.set_color_scheme(adw::ColorScheme::ForceDark);
            assert_eq!(sql_buffer.style_scheme().unwrap().id(), "Adwaita-dark");
        }
    }
    for (text, visible) in [
        ("nonsense", false),
        ("SELECT 'UPDATE'", false),
        ("DELETE FROM t WHERE id=1", true),
        (
            "WITH x AS (SELECT replace(name,'a','b') FROM t) SELECT * FROM x",
            false,
        ),
        ("SELECT 1; UPDATE t SET x=1", true),
        ("-- DELETE FROM t", false),
    ] {
        sql_buffer.set_text(text);
        assert_eq!(sql_indicator.is_visible(), visible, "{text}");
    }
    sql_buffer.set_text("SELECT 1; DELETE FROM t");
    sql_buffer.select_range(&sql_buffer.start_iter(), &sql_buffer.iter_at_offset(8));
    assert!(
        !sql_indicator.is_visible(),
        "only the selected SQL will execute"
    );
    sql_buffer.place_cursor(&sql_buffer.end_iter());
    assert!(sql_indicator.is_visible());
    sql_buffer.set_text("-- SQL");
    println!("SQL theme switching, real syntax tags and live mutation badge: passed");
    assert_eq!(
        ui.sql_tabs.borrow()[&sql_id]
            .result_stack
            .visible_child_name()
            .as_deref(),
        Some("empty")
    );
    until(|| ui.sql_tabs.borrow()[&sql_id].result_stack.width() > 0);
    layout_snapshot(&ui.window, "target/sql-empty-shortcuts.png");
    for (query, expected) in [
        ("SELECT 42 AS answer", "data"),
        ("SELECT 1 WHERE false", "empty"),
        ("invalid sql", "empty"),
    ] {
        ui.sql_tabs.borrow()[&sql_id].buffer.set_text(query);
        assert!(shortcuts::dispatch(
            &ui,
            gtk::gdk::Key::Return,
            gtk::gdk::ModifierType::CONTROL_MASK
        ));
        until(|| ui.sql_tabs.borrow()[&sql_id].run_button.is_sensitive());
        let tabs = ui.sql_tabs.borrow();
        let tab = &tabs[&sql_id];
        assert_eq!(
            tab.result_stack.visible_child_name().as_deref(),
            Some(expected)
        );
        assert_eq!(tab.copy_button.is_sensitive(), expected == "data");
        if expected == "data" {
            until(|| {
                js(
                    &tab.tabulator,
                    "typeof table !== 'undefined' && table && table.getRows().length === 1",
                ) == true
            });
            until(|| tab.run_button.width() > 0);
            assert!(tab.run_button.height() <= 32, "Execute should stay compact");
            let mut sibling = tab.run_button.next_sibling();
            let menu_button = loop {
                let widget = sibling.expect("SQL actions menu must exist");
                if let Ok(button) = widget.clone().downcast::<gtk::MenuButton>() {
                    break button;
                }
                sibling = widget.next_sibling();
            };
            menu_button.popup();
            until(|| menu_button.popover().unwrap().is_mapped());
            let popover = menu_button.popover().unwrap();
            let content = popover.child().unwrap();
            let mut child = content.first_child();
            let mut actions = 0;
            while let Some(widget) = child {
                if let Ok(button) = widget.clone().downcast::<gtk::Button>() {
                    assert!(
                        button
                            .child()
                            .unwrap()
                            .first_child()
                            .unwrap()
                            .is::<gtk::Image>()
                    );
                    actions += 1;
                }
                child = widget.next_sibling();
            }
            // Six file/result actions, Markdown batch, formatting, EXPLAIN, four
            // transaction/session actions and explicit write execution.
            assert_eq!(actions, 14);
            let paintable = gtk::WidgetPaintable::new(Some(&content));
            let mut menu_node = None;
            until(|| {
                let snapshot = gtk::Snapshot::new();
                paintable.snapshot(&snapshot, content.width() as f64, content.height() as f64);
                menu_node = snapshot.to_node();
                menu_node.is_some()
            });
            ui.window
                .renderer()
                .unwrap()
                .render_texture(&menu_node.unwrap(), None)
                .save_to_png("target/sql-actions-menu.png")
                .unwrap();
            tab.copy_button.emit_clicked();
            until(|| !popover.is_visible());
        }
    }
    println!("SQL toolbar: compact Execute, menu, populated/empty/error result states: passed");
    ui.window.close();
    runtime.block_on(pool.close()); // PostgreSQL automatically removes this connection's TEMP table.
}

pub(super) fn layout_snapshot(window: &impl IsA<gtk::Window>, path: &str) {
    let window: &gtk::Window = window.as_ref();
    window.queue_draw();
    let paintable = gtk::WidgetPaintable::new(Some(window));
    let mut node = None;
    until(|| {
        let snapshot = gtk::Snapshot::new();
        paintable.snapshot(&snapshot, window.width() as f64, window.height() as f64);
        node = snapshot.to_node();
        if node.is_none() {
            window.queue_draw();
        }
        node.is_some()
    });
    let node = node.unwrap();
    window
        .renderer()
        .unwrap()
        .render_texture(&node, None)
        .save_to_png(path)
        .unwrap();
}

#[test]
#[ignore = "requires GTK display; no database connection"]
fn workspace_controls_layout_and_risk() {
    gtk::init().unwrap();
    adw::init().unwrap();
    let runtime = Arc::new(
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap(),
    );
    let app = adw::Application::builder()
        .application_id("io.github.dbdeck.layout-test")
        .build();
    app.register(None::<&gtk::gio::Cancellable>).unwrap();
    let ui = build_window(&app, runtime);
    let nav = &ui.connections_nav;
    until(|| nav.width() > 0);
    let rail = nav.parent().unwrap();
    let nav_bounds = nav.compute_bounds(&rail).unwrap();
    assert!(
        (nav_bounds.x() + nav_bounds.width() / 2.0 - rail.width() as f32 / 2.0).abs() < 1.5,
        "navigation is not centered"
    );
    for name in [
        "database",
        "tables",
        "settings",
        "add",
        "edit",
        "delete",
        "status-idle",
    ] {
        let image = penpot_icon_image(name);
        assert_eq!(image.pixel_size(), 28, "{name} rendered size");
        let name = image.icon_name().unwrap();
        let icons = gtk::IconTheme::for_display(&gtk::gdk::Display::default().unwrap());
        assert!(icons.has_icon(&name), "{name} must be installed as SVG");
        for scale in [1, 2] {
            let paintable = icons.lookup_icon(
                &name,
                &[],
                28,
                scale,
                gtk::TextDirection::Ltr,
                gtk::IconLookupFlags::empty(),
            );
            assert!(paintable.is_symbolic());
            assert_eq!(
                paintable
                    .file()
                    .unwrap()
                    .path()
                    .unwrap()
                    .extension()
                    .unwrap(),
                "svg"
            );
        }
    }
    let object = DbObject {
        schema: "public".to_owned(),
        name: "sample_records".to_owned(),
        kind: database::DbObjectKind::Table,
    };
    let object_button = table_object_button(&object);
    let icon = object_button
        .child()
        .unwrap()
        .first_child()
        .unwrap()
        .downcast::<gtk::Image>()
        .unwrap();
    assert_eq!(
        icon.pixel_size(),
        16,
        "tree icons stay proportional to text"
    );
    let folder = penpot_icon_image("folder-closed");
    folder.set_pixel_size(16);
    set_penpot_icon(&folder, "folder-open", false);
    assert_eq!(
        folder.pixel_size(),
        16,
        "expanding a schema must not enlarge its icon"
    );
    // Tabs live in the header strip now; its "+" must stay inside the window.
    let add = ui.tab_strip.add_button.clone();
    until(|| add.width() > 0 && ui.tab_strip.root.width() > 0);
    let button = add.compute_bounds(&ui.window).unwrap();
    assert!(
        button.height() <= 32.0,
        "SQL button should not enlarge the tab strip: {button:?}"
    );
    layout_snapshot(&ui.window, "target/workspace-layout-wide.png");
    fn first_paned(widget: &gtk::Widget) -> Option<gtk::Paned> {
        if let Some(paned) = widget.downcast_ref::<gtk::Paned>() {
            return Some(paned.clone());
        }
        let mut child = widget.first_child();
        while let Some(current) = child {
            if let Some(found) = first_paned(&current) {
                return Some(found);
            }
            child = current.next_sibling();
        }
        None
    }
    let body = first_paned(&ui.window.content().unwrap()).unwrap();
    assert_eq!(body.orientation(), gtk::Orientation::Horizontal);
    body.set_position(500);
    until(|| {
        let bounds = add.compute_bounds(&ui.window).unwrap();
        bounds.x() + bounds.width() < ui.window.width() as f32
    });
    println!("sidebar resize: SQL button stays inside the window");
    assert!(shortcuts::dispatch(
        &ui,
        gtk::gdk::Key::b,
        gtk::gdk::ModifierType::CONTROL_MASK
    ));
    until(|| body.position() <= 60);
    assert!(ui.connections_nav.is_mapped() && ui.tables_nav.is_mapped());
    assert_eq!(ui.layout.borrow().sidebar_width, 500);
    layout_snapshot(&ui.window, "target/workspace-sidebar-collapsed.png");
    ui.sidebar_toggle.set_active(true);
    until(|| body.position() == 500);
    assert!(!ui.layout.borrow().sidebar_collapsed);
    ui.window.set_default_size(930, 600);
    until(|| {
        let bounds = add.compute_bounds(&ui.window).unwrap();
        ui.window.width() == 930 && bounds.x() + bounds.width() <= 930.0
    });
    println!("real application window resizes to 930px");
    let workspace = body.end_child().unwrap();
    fn assert_labels_fit(panel: &gtk::Box, workspace: &gtk::Widget) {
        let mut child = panel.first_child();
        let mut labels = 0;
        while let Some(widget) = child {
            child = widget.next_sibling();
            let Ok(label) = widget.downcast::<gtk::Label>() else {
                continue;
            };
            let bounds = label.compute_bounds(workspace).unwrap();
            assert!(
                bounds.x() >= 0.0 && bounds.x() + bounds.width() <= workspace.width() as f32 + 1.0,
                "empty-state label exceeds workspace: {}",
                label.text()
            );
            assert!(
                label.layout().pixel_size().0 <= label.width(),
                "empty-state text is clipped (text {}, widget {}): {}",
                label.layout().pixel_size().0,
                label.width(),
                label.text()
            );
            labels += 1;
        }
        assert!(labels > 0, "empty-state labels must be checked");
    }
    assert_labels_fit(&ui.landing, &workspace);
    ui.landing.set_visible(false);
    ui.empty_session_hint.set_text(
        "Рабочая область подключения с длинным названием: development / analytics_reporting_database. Откройте таблицу или создайте SQL-запрос.",
    );
    ui.empty_session.set_visible(true);
    until(|| ui.empty_session_hint.width() > 0 && ui.empty_session_hint.layout().is_wrapped());
    assert_labels_fit(&ui.empty_session, &workspace);
    layout_snapshot(&ui.window, "target/workspace-empty-session-narrow.png");
    ui.empty_session.set_visible(false);
    ui.landing.set_visible(true);
    until(|| ui.landing.is_mapped());
    // Overflowing tabs scroll inside the strip instead of pushing "+" out of the window.
    for index in 0..16 {
        ui.tab_strip
            .row
            .append(&gtk::Label::new(Some(&format!("Long query tab {index}"))));
    }
    until(|| {
        let bounds = add.compute_bounds(&ui.window).unwrap();
        ui.tab_strip.row.width() > ui.tab_strip.scroller.width()
            && bounds.x() >= 0.0
            && bounds.x() + bounds.width() <= ui.window.width() as f32
    });
    assert!(
        add.compute_bounds(&ui.tab_strip.root).unwrap().x() + add.width() as f32
            <= ui.tab_strip.root.width() as f32,
        "SQL button clipped by the tab strip"
    );
    layout_snapshot(&ui.window, "target/workspace-layout-narrow.png");
    let buffer = sourceview5::Buffer::new(None);
    let indicator = gtk::Label::new(None);
    for (text, expected) in [
        ("фвавыа", "SQL не распознан"),
        ("SELECT 'DELETE'", "Читающий запрос"),
        ("DELETE FROM t", "⚠ Может изменить данные"),
        ("-- UPDATE t", "Ожидает запрос"),
    ] {
        buffer.set_text(text);
        update_sql_risk_indicator_for(&buffer, &indicator, sql_risk::Dialect::PostgreSql);
        assert_eq!(indicator.text(), expected);
        assert_eq!(indicator.is_visible(), text.starts_with("DELETE"));
    }
    buffer.set_text("SELECT 1; DELETE FROM t;");
    buffer.select_range(&buffer.start_iter(), &buffer.iter_at_offset(8));
    update_sql_risk_indicator_for(&buffer, &indicator, sql_risk::Dialect::PostgreSql);
    assert_eq!(
        indicator.text(),
        "Читающий запрос",
        "risk should match selected executable SQL"
    );

    fn check_form_entries(widget: &gtk::Widget) {
        if let Some(entry) = widget.downcast_ref::<gtk::Entry>() {
            assert!(entry.activates_default());
        }
        if let Some(entry) = widget.downcast_ref::<gtk::PasswordEntry>() {
            assert!(entry.activates_default());
        }
        let mut child = widget.first_child();
        while let Some(current) = child {
            check_form_entries(&current);
            child = current.next_sibling();
        }
    }
    let profile = ConnectionProfile {
        id: "layout-test".into(),
        name: "layout-test".into(),
        kind: DatabaseKind::PostgreSql,
        host: "127.0.0.1".into(),
        port: 5432,
        database: String::new(),
        username: "postgres".into(),
        environment: EnvironmentTag::Local,
        color: "#4f8fe8".into(),
        read_only: false,
        tls_mode: TlsMode::VerifyFull,
        password_saved: false,
        gopass_entry: None,
        tls_files: Default::default(),
        native_tools_dir: None,
        transport: Default::default(),
    };
    for (existing, title, screenshot) in [
        (None, "Новое подключение", "target/connection-new.png"),
        (
            Some(profile),
            "Изменить подключение",
            "target/connection-edit.png",
        ),
    ] {
        show_connection_form(&ui, existing);
        let form = gtk::Window::list_toplevels()
            .into_iter()
            .filter_map(|widget| widget.downcast::<adw::Window>().ok())
            .find(|window| window.title().as_deref() == Some(title))
            .unwrap();
        let default = gtk::prelude::GtkWindowExt::default_widget(&form)
            .unwrap()
            .downcast::<gtk::Button>()
            .unwrap();
        until(|| form.is_mapped() && default.width() > 0);
        assert_eq!(default.label().as_deref(), Some("Сохранить"));
        let scroll = form
            .content()
            .unwrap()
            .downcast::<gtk::ScrolledWindow>()
            .unwrap();
        for (axis, adjustment) in [
            ("width", scroll.hadjustment()),
            ("height", scroll.vadjustment()),
        ] {
            assert!(
                adjustment.upper() - adjustment.page_size() <= 1.0,
                "{title}: initial {axis} clips the form ({} of {})",
                adjustment.page_size(),
                adjustment.upper()
            );
        }
        check_form_entries(form.upcast_ref());
        layout_snapshot(&form, screenshot);
        form.close();
    }
    assert!(shortcuts::dispatch(
        &ui,
        gtk::gdk::Key::question,
        gtk::gdk::ModifierType::CONTROL_MASK | gtk::gdk::ModifierType::SHIFT_MASK
    ));
    let help = gtk::Window::list_toplevels()
        .into_iter()
        .filter_map(|widget| widget.downcast::<adw::Window>().ok())
        .find(|window| window.title().as_deref() == Some("Сочетания клавиш"))
        .unwrap();
    until(|| help.is_mapped());
    layout_snapshot(&help, "target/keyboard-shortcuts.png");
    help.close();
    ui.sidebar_toggle.set_active(false);
    ui.window.close();
    let saved = ui_layout::Layout::load();
    assert!(saved.sidebar_collapsed);
    assert_eq!(saved.sidebar_width, 500);
    let reopened = build_window(&app, ui.runtime.clone());
    until(|| reopened.window.is_mapped());
    assert!(!reopened.sidebar_toggle.is_active());
    assert!(reopened.connections_nav.is_mapped());
    reopened.sidebar_toggle.set_active(true);
    let restored_body = first_paned(&reopened.window.content().unwrap()).unwrap();
    until(|| restored_body.position() == 500);
    reopened.window.close();
}

pub(super) fn named_widget(root: &gtk::Widget, name: &str) -> Option<gtk::Widget> {
    if root.widget_name() == name {
        return Some(root.clone());
    }
    let mut child = root.first_child();
    while let Some(widget) = child {
        if let Some(found) = named_widget(&widget, name) {
            return Some(found);
        }
        child = widget.next_sibling();
    }
    None
}

#[test]
#[ignore = "requires GTK display; settings are stored only in isolated XDG test directories"]
fn settings_theme_controls() {
    gtk::init().unwrap();
    adw::init().unwrap();
    let runtime = Arc::new(
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap(),
    );
    let app = adw::Application::builder()
        .application_id("io.github.dbdeck.settings-test")
        .build();
    app.register(None::<&gtk::gio::Cancellable>).unwrap();
    let ui = build_window(&app, runtime);
    ui.preferences.present();
    until(|| ui.preferences.is_mapped());
    let content = ui.preferences.content().unwrap();
    let pages = named_widget(&content, "settings-pages")
        .unwrap()
        .downcast::<gtk::Stack>()
        .unwrap();
    let selector = named_widget(&content, "settings-theme-selector")
        .unwrap()
        .downcast::<gtk::ComboBoxText>()
        .unwrap();
    pages.set_visible_child_name("appearance");
    selector.set_active_id(Some("tokyo_night"));
    until(|| selector.width() > 0 && ui.active_theme.get() == ThemeMode::TokyoNight);
    assert_eq!(
        theme::load_mode(),
        ThemeMode::TokyoNight,
        "theme selection is persisted"
    );
    let active_icon = ui
        .connections_nav
        .child()
        .unwrap()
        .downcast::<gtk::Image>()
        .unwrap();
    let icons = gtk::IconTheme::for_display(&gtk::gdk::Display::default().unwrap());
    let table_icon = penpot_icon_image("tables-active");
    assert!(icons.has_icon(&table_icon.icon_name().unwrap()));
    for scale in [1, 2] {
        let vector = icons.lookup_icon(
            &table_icon.icon_name().unwrap(),
            &[],
            28,
            scale,
            gtk::TextDirection::Ltr,
            gtk::IconLookupFlags::empty(),
        );
        assert!(vector.is_symbolic());
        assert_eq!(
            vector.file().unwrap().path().unwrap().extension().unwrap(),
            "svg"
        );
    }
    let database_icon = profile_tab_icon("database", "#df626b");
    let database_name = database_icon.icon_name().unwrap();
    let database_vector = icons.lookup_icon(
        &database_name,
        &[],
        18,
        2,
        gtk::TextDirection::Ltr,
        gtk::IconLookupFlags::empty(),
    );
    assert!(
        !database_vector.is_symbolic(),
        "DB colors must bypass symbolic recoloring"
    );
    let svg = fs::read_to_string(database_vector.file().unwrap().path().unwrap()).unwrap();
    assert!(svg.contains("#df626b"));
    for (id, color) in [
        ("dark", "#42b8b0"),
        ("catppuccin_mocha", "#94e2d5"),
        ("tokyo_night", "#7aa2f7"),
    ] {
        selector.set_active_id(Some(id));
        let expected = gtk::gdk::RGBA::parse(color).unwrap();
        until(|| active_icon.style_context().color() == expected);
        assert_eq!(database_icon.icon_name().unwrap(), database_name);
    }

    assert!(
        selector.height() <= 34,
        "theme selector must stay compact: {}",
        selector.height()
    );
    let frame = Rc::new(Cell::new(false));
    let rendered = frame.clone();
    gtk::glib::timeout_add_local_once(Duration::from_millis(100), move || rendered.set(true));
    until(|| frame.get());
    layout_snapshot(&ui.preferences, "target/settings-tokyo-night.png");
    pages.set_visible_child_name("general");
    let frame = Rc::new(Cell::new(false));
    let rendered = frame.clone();
    gtk::glib::timeout_add_local_once(Duration::from_millis(100), move || rendered.set(true));
    until(|| frame.get());
    layout_snapshot(&ui.preferences, "target/settings-general.png");
    show_vault_dialog(&ui, false, || {});
    let vault = gtk::Window::list_toplevels()
        .into_iter()
        .find_map(|widget| {
            widget
                .downcast::<gtk::Window>()
                .ok()
                .filter(|window| window.title().as_deref() == Some("Создать хранилище паролей"))
        })
        .expect("vault password window");
    until(|| vault.is_mapped());
    assert_eq!(
        vault.transient_for(),
        Some(ui.preferences.clone().upcast()),
        "password window must stay above settings"
    );
    fn passwords(widget: &gtk::Widget, found: &mut Vec<gtk::PasswordEntry>) {
        if let Ok(entry) = widget.clone().downcast::<gtk::PasswordEntry>() {
            found.push(entry);
        }
        let mut child = widget.first_child();
        while let Some(current) = child {
            passwords(&current, found);
            child = current.next_sibling();
        }
    }
    let mut entries = Vec::new();
    passwords(vault.upcast_ref(), &mut entries);
    assert_eq!(entries.len(), 2);
    until(|| entries.iter().all(|entry| entry.height() > 0));
    for entry in entries {
        assert!(
            entry.height() <= 34,
            "password input must stay compact: {}",
            entry.height()
        );
    }
    layout_snapshot(&vault, "target/settings-password.png");
    vault.close();
    ui.preferences.close();
    assert!(!ui.preferences.is_visible());
    ui.preferences.present();
    until(|| ui.preferences.is_mapped());
    assert_eq!(selector.active_id().as_deref(), Some("tokyo_night"));
    ui.preferences.close();
    ui.window.close();
}

#[test]
#[ignore = "requires GTK display and Compose PostgreSQL; writes only a connection-local TEMP table"]
fn sql_result_staging_and_delete() {
    gtk::init().unwrap();
    adw::init().unwrap();
    let runtime = Arc::new(
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap(),
    );
    let pool=runtime.block_on(async {
        let options=sqlx::postgres::PgConnectOptions::new().host("127.0.0.1").port(55432).database("dbdeck_test").username("dbdeck_test")
            .password(&std::env::var("DBDECK_PG_PASSWORD").expect("test password from .env"));
        let pool=sqlx::postgres::PgPoolOptions::new().max_connections(1).idle_timeout(None).max_lifetime(None).connect_with(options).await.unwrap();
        sqlx::raw_sql("CREATE TEMP TABLE dbdeck_sql_edit_test (id int PRIMARY KEY, value text, computed text GENERATED ALWAYS AS (upper(value)) STORED); INSERT INTO dbdeck_sql_edit_test VALUES (1,'one'),(2,'two');").execute(&pool).await.unwrap();
        pool
    });
    let app = adw::Application::builder()
        .application_id("io.github.dbdeck.sql-edit-test")
        .build();
    app.register(None::<&gtk::gio::Cancellable>).unwrap();
    let ui = build_window(&app, runtime.clone());
    let profile = ConnectionProfile {
        id: "sql-edit-test".into(),
        name: "SQL edit test".into(),
        kind: DatabaseKind::PostgreSql,
        host: "127.0.0.1".into(),
        port: 55432,
        database: "dbdeck_test".into(),
        username: "dbdeck_test".into(),
        environment: EnvironmentTag::Local,
        color: "#4f8fe8".into(),
        read_only: false,
        tls_mode: TlsMode::Disabled,
        password_saved: false,
        gopass_entry: None,
        tls_files: Default::default(),
        native_tools_dir: None,
        transport: Default::default(),
    };
    let session = profile_session_key(&profile);
    ui.sessions.borrow_mut().insert(
        session.clone(),
        (profile.clone(), DbPool::PostgreSql(pool.clone())),
    );
    {
        let mut store = ui.store.borrow_mut();
        let store = store.as_mut().unwrap();
        store
            .unlock_or_create_vault("synthetic master password")
            .unwrap();
        store.save_profile(profile.clone(), "").unwrap();
    }
    *ui.current_profile.borrow_mut() = Some(profile.clone());
    ui.landing.set_visible(false);
    create_sql_tab(&ui, &session);
    let id = ui.sql_tabs.borrow().keys().next().unwrap().clone();
    let key = format!("sql::{id}");
    let buffer = ui.sql_tabs.borrow()[&id].buffer.clone();
    let grid = ui.sql_tabs.borrow()[&id].tabulator.clone();
    buffer.set_text("SELECT * FROM dbdeck_sql_edit_test ORDER BY id LIMIT 2");
    run_sql_tab(&ui, &id);
    until(|| ui.sql_tabs.borrow()[&id].result.is_some());
    until(|| {
        js(
            &grid,
            "Boolean(typeof table !== 'undefined' && table && !updatingData && !locked)",
        ) == true
    });
    assert!(can_edit_table_cells(
        &ui,
        &key,
        &ui.table_data.borrow()[&key]
    ));
    assert_eq!(ui.table_data.borrow()[&key].primary_key, vec!["id"]);
    assert_eq!(
        js(
            &grid,
            "table.getColumn('c0').getDefinition().editor === false && table.getColumn('c2').getDefinition().editor === false"
        ),
        true
    );
    js(
        &grid,
        "applyValue(table.getRow(0).getCell('c1'),'discard-me'); true",
    );
    until(|| {
        !ui.table_edit_states.borrow()[&key]
            .pending
            .borrow()
            .is_empty()
    });
    let owned_session = ui.sql_tabs.borrow()[&id].session.clone();
    assert!(
        Arc::ptr_eq(
            ui.sql_tabs.borrow()[&id].result.as_ref().unwrap(),
            &ui.table_data.borrow()[&key]
        ),
        "SQL result backing data must be shared"
    );
    let original = runtime
        .block_on(owned_session.execute("SELECT value FROM dbdeck_sql_edit_test WHERE id=1"))
        .unwrap();
    assert_eq!(original.data.rows[0][0], CellValue::Text("one".into()));
    let request = ui.sql_tabs.borrow()[&id].request_id.clone();
    run_sql_tab(&ui, &id);
    assert_eq!(ui.sql_tabs.borrow()[&id].request_id, request);
    assert!(
        !ui.table_edit_states.borrow()[&key]
            .pending
            .borrow()
            .is_empty()
    );
    close_sql_tab(&ui, &id);
    assert!(ui.sql_tabs.borrow().contains_key(&id));
    discard_table_edits(&ui, &key);
    until(|| {
        js(
            &grid,
            "!updatingData && table.getRow(0).getData().c1 === 'one'",
        ) == true
    });
    assert!(!ui.table_edit_states.borrow()[&key].toolbar.is_visible());
    // Stale values must never overwrite a later database change. The whole
    // batch rolls back, including an earlier valid edit in the other row.
    js(
        &grid,
        "applyValue(table.getRow(0).getCell('c1'),'mine-one'); applyValue(table.getRow(1).getCell('c1'),'mine-two'); true",
    );
    until(|| ui.table_edit_states.borrow()[&key].pending.borrow().len() == 2);
    runtime
        .block_on(
            owned_session
                .execute("UPDATE dbdeck_sql_edit_test SET value='changed-outside' WHERE id=2"),
        )
        .unwrap();
    save_table_edits(&ui, &key);
    edits_preview().1.emit_clicked();
    until(|| !ui.table_edit_states.borrow()[&key].saving.get());
    assert!(
        ui.table_edit_states.borrow()[&key]
            .status
            .text()
            .contains("Конфликт")
    );
    assert_eq!(
        ui.table_edit_states.borrow()[&key].pending.borrow().len(),
        2
    );
    until(|| js(&grid, "!locked && !updatingData") == true);
    assert!(
        ui.table_edit_states.borrow()[&key]
            .save_button
            .is_sensitive()
    );

    let current = runtime
        .block_on(owned_session.execute("SELECT value FROM dbdeck_sql_edit_test ORDER BY id"))
        .unwrap();
    assert_eq!(current.data.rows[0][0], CellValue::Text("one".into()));
    assert_eq!(
        current.data.rows[1][0],
        CellValue::Text("changed-outside".into())
    );
    runtime
        .block_on(owned_session.execute("UPDATE dbdeck_sql_edit_test SET value='two' WHERE id=2"))
        .unwrap();
    discard_table_edits(&ui, &key);
    until(|| {
        js(
            &grid,
            "!updatingData && !locked && table.getRow(0).getData().c1 === 'one'",
        ) == true
    });
    js(
        &grid,
        "applyValue(table.getRow(0).getCell('c1'),'saved'); true",
    );
    until(|| {
        !ui.table_edit_states.borrow()[&key]
            .pending
            .borrow()
            .is_empty()
    });
    // A changed editor must not be executed by Save/refresh.
    buffer.set_text("DELETE FROM dbdeck_sql_edit_test");
    save_table_edits(&ui, &key);
    edits_preview().1.emit_clicked();
    until(|| {
        ui.table_data
            .borrow()
            .get(&key)
            .is_some_and(|data| data.rows[0][1] == CellValue::Text("saved".into()))
    });
    until(|| js(&grid, "!updatingData && !locked") == true);
    assert_eq!(ui.table_data.borrow()[&key].rows.len(), 2);
    js(
        &grid,
        "table.getRow(0).getElement().dispatchEvent(new MouseEvent('contextmenu',{bubbles:true,clientX:100,clientY:100})); true",
    );
    until(|| js(&grid, "document.querySelector('.tabulator-menu') !== null") == true);
    assert_eq!(
        js(
            &grid,
            "Array.from(document.querySelectorAll('.tabulator-menu-item')).map(e=>e.textContent)"
        ),
        serde_json::json!(["Удалить строку"])
    );
    run_table_row_action(&ui, &key, 0, "Удалить строку");
    until(|| {
        ui.table_data
            .borrow()
            .get(&key)
            .is_some_and(|data| data.rows.len() == 1)
    });
    assert_eq!(
        ui.table_data.borrow()[&key].rows[0][0],
        CellValue::Text("2".into())
    );
    buffer.set_text("SELECT count(*) FROM dbdeck_sql_edit_test");
    run_sql_tab(&ui, &id);
    until(|| ui.sql_tabs.borrow()[&id].run_button.is_sensitive());
    assert!(!can_edit_table_cells(
        &ui,
        &key,
        &ui.table_data.borrow()[&key]
    ));
    assert!(
        ui.sql_tabs.borrow()[&id]
            .status
            .text()
            .contains("только чтение")
    );
    until(|| js(&grid, "Boolean(table && !updatingData && !locked)") == true);
    assert_eq!(
        js(&grid, "table.options.rowContextMenu()[0].disabled"),
        true
    );
    runtime.block_on(owned_session.execute("CREATE TEMP TABLE dbdeck_sql_composite (key_a int, key_b int, value text, PRIMARY KEY(key_a,key_b)); INSERT INTO dbdeck_sql_composite VALUES (1,1,'sibling'),(1,2,'target');")).unwrap();
    buffer.set_text("SELECT key_b, value, key_a FROM dbdeck_sql_composite ORDER BY key_b DESC");
    run_sql_tab(&ui, &id);
    until(|| ui.sql_tabs.borrow()[&id].result.is_some());
    until(|| {
        js(
            &grid,
            "!updatingData && !locked && table.getDataCount()===2",
        ) == true
    });
    js(
        &grid,
        "applyValue(table.getRow(0).getCell('c1'),'composite-saved'); true",
    );
    until(|| {
        !ui.table_edit_states.borrow()[&key]
            .pending
            .borrow()
            .is_empty()
    });
    save_table_edits(&ui, &key);
    edits_preview().1.emit_clicked();
    until(|| {
        ui.table_data
            .borrow()
            .get(&key)
            .is_some_and(|data| data.rows[0][1] == CellValue::Text("composite-saved".into()))
    });
    until(|| js(&grid, "!updatingData && !locked") == true);
    run_table_row_action(&ui, &key, 0, "Удалить строку");
    until(|| {
        ui.table_data
            .borrow()
            .get(&key)
            .is_some_and(|data| data.rows.len() == 1)
    });
    assert_eq!(
        ui.table_data.borrow()[&key].rows[0][1],
        CellValue::Text("sibling".into())
    );
    // Managed transactions, close guards, cancellation and command palette all work without F keys.
    let ctrl = gtk::gdk::ModifierType::CONTROL_MASK;
    let alt = gtk::gdk::ModifierType::ALT_MASK;
    let shift = gtk::gdk::ModifierType::SHIFT_MASK;
    assert!(shortcuts::dispatch(&ui, gtk::gdk::Key::t, ctrl | alt));
    until(|| ui.sql_tabs.borrow()[&id].run_button.is_sensitive());
    assert!(ui.sql_tabs.borrow()[&id].transaction_state.manual);
    buffer.set_text("UPDATE dbdeck_sql_edit_test SET value='transaction-edit' WHERE id=2");
    assert!(shortcuts::dispatch(&ui, gtk::gdk::Key::Return, ctrl));
    until(|| ui.sql_tabs.borrow()[&id].run_button.is_sensitive());
    assert!(ui.sql_tabs.borrow()[&id].transaction_state.pending);
    close_sql_tab(&ui, &id);
    assert!(ui.sql_tabs.borrow().contains_key(&id));
    close_session(&ui, &session);
    assert!(ui.sessions.borrow().contains_key(&session));
    assert!(shortcuts::dispatch(&ui, gtk::gdk::Key::r, ctrl | alt));
    until(|| ui.sql_tabs.borrow()[&id].run_button.is_sensitive());
    assert!(!ui.sql_tabs.borrow()[&id].transaction_state.pending);
    assert!(shortcuts::dispatch(&ui, gtk::gdk::Key::t, ctrl | alt));
    until(|| ui.sql_tabs.borrow()[&id].run_button.is_sensitive());
    assert!(!ui.sql_tabs.borrow()[&id].transaction_state.manual);
    let result = runtime
        .block_on(owned_session.execute("SELECT value FROM dbdeck_sql_edit_test WHERE id=2"))
        .unwrap();
    assert_eq!(result.data.rows[0][0], CellValue::Text("two".into()));
    buffer.set_text("SELECT pg_sleep(20)");
    assert!(shortcuts::dispatch(&ui, gtk::gdk::Key::Return, ctrl));
    let request = ui.sql_tabs.borrow()[&id].request_id.clone();
    assert!(ui.sql_tabs.borrow()[&id].stop_button.is_sensitive());
    assert!(
        ui.sql_tabs.borrow()[&id]
            .transaction_buttons
            .iter()
            .all(|b| !b.is_sensitive())
    );
    close_sql_tab(&ui, &id);
    assert!(
        ui.sql_tabs.borrow().contains_key(&id),
        "running SQL tab cannot close"
    );
    assert!(shortcuts::dispatch(&ui, gtk::gdk::Key::Return, ctrl));
    assert_eq!(
        ui.sql_tabs.borrow()[&id].request_id,
        request,
        "a second Run cannot replace an active query"
    );
    assert!(shortcuts::dispatch(
        &ui,
        gtk::gdk::Key::period,
        ctrl | shift
    ));
    until(|| ui.sql_tabs.borrow()[&id].run_button.is_sensitive());
    assert!(ui.sql_tabs.borrow()[&id].error_message.is_visible());
    assert!(!ui.sql_tabs.borrow()[&id].stop_button.is_sensitive());
    let message = ui.sql_tabs.borrow()[&id]
        .error_message
        .text()
        .to_lowercase();
    assert!(
        message.contains("отмен") || message.contains("останов"),
        "{message}"
    );
    assert!(!ui.sql_tabs.borrow()[&id].transaction_state.pending);
    assert!(!ui.sql_tabs.borrow()[&id].transaction_state.failed);
    let after_cancel = runtime
        .block_on(owned_session.execute("SELECT value FROM dbdeck_sql_edit_test WHERE id=2"))
        .unwrap();
    assert_eq!(after_cancel.data.rows[0][0], CellValue::Text("two".into()));
    assert!(shortcuts::dispatch(&ui, gtk::gdk::Key::_1, ctrl));
    let focused = gtk::prelude::RootExt::focus(&ui.window).unwrap();
    assert!(
        focused
            == ui.sql_tabs.borrow()[&id]
                .editor
                .clone()
                .upcast::<gtk::Widget>()
            || focused.is_ancestor(&ui.sql_tabs.borrow()[&id].editor)
    );
    assert!(shortcuts::dispatch(&ui, gtk::gdk::Key::p, ctrl | shift));
    let palette = gtk::Window::list_toplevels()
        .into_iter()
        .filter_map(|w| w.downcast::<adw::Window>().ok())
        .find(|w| w.title().as_deref() == Some("Палитра команд"))
        .unwrap();
    until(|| palette.is_mapped());
    let search = named_widget(palette.upcast_ref(), "command-search")
        .unwrap()
        .downcast::<gtk::SearchEntry>()
        .unwrap();
    let commands = named_widget(palette.upcast_ref(), "command-list")
        .unwrap()
        .downcast::<gtk::ListBox>()
        .unwrap();
    search.set_text("Настройки");
    until(|| {
        commands.selected_row().is_some_and(|r| {
            r.child()
                .unwrap()
                .first_child()
                .unwrap()
                .downcast::<gtk::Label>()
                .unwrap()
                .text()
                == "Настройки"
        })
    });
    search.emit_activate();
    until(|| ui.preferences.is_mapped());
    ui.preferences.set_visible(false);
    assert!(shortcuts::dispatch(
        &ui,
        gtk::gdk::Key::question,
        ctrl | shift
    ));
    let help = gtk::Window::list_toplevels()
        .into_iter()
        .filter_map(|w| w.downcast::<adw::Window>().ok())
        .find(|w| w.title().as_deref() == Some("Сочетания клавиш"))
        .unwrap();
    until(|| help.is_mapped());
    assert!(named_widget(help.upcast_ref(), "shortcuts-search").is_some());
    help.close();
    buffer.set_text("SELECT * FROM dbdeck_sql_edit_test ORDER BY id");
    assert!(shortcuts::dispatch(&ui, gtk::gdk::Key::Return, ctrl));
    until(|| {
        ui.sql_tabs.borrow()[&id].run_button.is_sensitive()
            && ui.sql_tabs.borrow()[&id].result.is_some()
    });
    assert!(shortcuts::dispatch(&ui, gtk::gdk::Key::d, ctrl | shift));
    let inspector = gtk::Window::list_toplevels()
        .into_iter()
        .filter_map(|w| w.downcast::<adw::Window>().ok())
        .find(|w| w.widget_name() == "object-structure")
        .unwrap();
    until(|| inspector.is_mapped());
    let copy_ddl = named_widget(inspector.upcast_ref(), "structure-copy-ddl")
        .unwrap()
        .downcast::<gtk::Button>()
        .unwrap();
    until(|| copy_ddl.is_sensitive());
    let ddl = named_widget(inspector.upcast_ref(), "structure-ddl")
        .unwrap()
        .downcast::<sourceview5::View>()
        .unwrap();
    let source = ddl.buffer();
    let definition = source.text(&source.start_iter(), &source.end_iter(), true);
    assert!(definition.contains("CREATE TABLE") && definition.contains("GENERATED ALWAYS AS"));
    let open_ddl = named_widget(inspector.upcast_ref(), "structure-open-sql")
        .unwrap()
        .downcast::<gtk::Button>()
        .unwrap();
    assert!(
        !open_ddl.is_sensitive(),
        "TEMP DDL cannot be executed in a different SQL session"
    );
    inspector.close();
    let profile = ui.current_profile.borrow().clone().unwrap();
    structure_ui::show(
        &ui,
        &profile,
        &DbObject {
            schema: "public".into(),
            name: "sample_records".into(),
            kind: database::DbObjectKind::Table,
        },
    );
    let inspector = gtk::Window::list_toplevels()
        .into_iter()
        .filter_map(|w| w.downcast::<adw::Window>().ok())
        .find(|w| w.widget_name() == "object-structure" && w.is_visible())
        .unwrap();
    let open_ddl = named_widget(inspector.upcast_ref(), "structure-open-sql")
        .unwrap()
        .downcast::<gtk::Button>()
        .unwrap();
    until(|| open_ddl.is_sensitive());
    open_ddl.emit_clicked();
    until(|| !inspector.is_visible());
    let preview_id = ui
        .sql_tabs
        .borrow()
        .keys()
        .find(|other| *other != &id)
        .unwrap()
        .clone();
    assert!(query_text(&ui.sql_tabs.borrow()[&preview_id]).contains("CREATE TABLE"));
    assert!(
        ui.sql_tabs.borrow()[&preview_id].request_id.is_empty(),
        "DDL preview never executes automatically"
    );
    close_sql_tab(&ui, &preview_id);
    assert!(
        ui.store
            .borrow()
            .as_ref()
            .unwrap()
            .sql_workspace()
            .history
            .iter()
            .any(|entry| entry.sql.contains("transaction-edit"))
    );
    assert!(shortcuts::dispatch(&ui, gtk::gdk::Key::h, ctrl | alt));
    let history = gtk::Window::list_toplevels()
        .into_iter()
        .filter_map(|w| w.downcast::<adw::Window>().ok())
        .find(|w| w.widget_name() == "sql-history" && w.is_visible())
        .unwrap();
    until(|| history.is_mapped());
    let search_history = named_widget(history.upcast_ref(), "sql-history-search")
        .unwrap()
        .downcast::<gtk::SearchEntry>()
        .unwrap();
    let preview = named_widget(history.upcast_ref(), "sql-history-preview")
        .unwrap()
        .downcast::<sourceview5::View>()
        .unwrap();
    search_history.set_text("transaction-edit");
    until(|| {
        let text = preview.buffer();
        text.text(&text.start_iter(), &text.end_iter(), true)
            .contains("transaction-edit")
    });
    search_history.emit_activate();
    until(|| !history.is_visible());
    let history_id = ui
        .sql_tabs
        .borrow()
        .keys()
        .find(|other| *other != &id)
        .unwrap()
        .clone();
    assert!(query_text(&ui.sql_tabs.borrow()[&history_id]).contains("transaction-edit"));
    assert!(
        ui.sql_tabs.borrow()[&history_id].request_id.is_empty(),
        "history opens without executing a mutating query"
    );
    close_sql_tab(&ui, &history_id);
    // Exercise native SQL menu and completion surfaces as well as the WebKit menu.
    let menu = ui.sql_tabs.borrow()[&id].actions_menu.clone();
    menu.popup();
    until(|| menu.popover().unwrap().is_mapped() && menu.popover().unwrap().width() > 0);
    assert!(menu.popover().unwrap().width() > 0);
    menu.popdown();
    until(|| !menu.popover().unwrap().is_mapped());
    let editor = ui.sql_tabs.borrow()[&id].editor.clone();
    editor.grab_focus();
    buffer.set_text("SEL");
    buffer.place_cursor(&buffer.end_iter());
    editor.completion().emit_show();
    let waited = Rc::new(Cell::new(false));
    let callback = waited.clone();
    gtk::glib::timeout_add_local_once(Duration::from_millis(300), move || callback.set(true));
    until(|| waited.get());
    fn completion_popup(editor: &sourceview5::View) -> Option<gtk::Widget> {
        let mut child = editor.first_child();
        while let Some(widget) = child {
            child = widget.next_sibling();
            if widget.type_().name() == "GtkSourceCompletionList" {
                return Some(widget);
            }
        }
        None
    }
    let popup = completion_popup(&editor).expect("completion surface was created");
    // A test window may lack compositor focus. Show its real completion surface
    // explicitly so geometry and the empty-content transition are still exercised.
    popup.set_visible(true);
    until(|| popup.is_mapped() && popup.width() > 0 && popup.height() > 0);
    assert!(popup.width() > 0 && popup.height() > 0);
    // Simulate the brief empty-content interval before GtkSourceView's hide tick.
    let popover = popup.clone().downcast::<gtk::Popover>().unwrap();
    let content = popover.child().unwrap();
    content.set_visible(false);
    popover.present();
    content.set_visible(true);
    editor.completion().hide();
    for text in [
        "",
        " ",
        "zzzz_no_such_completion",
        "SELECT ",
        "sample_records.",
    ] {
        buffer.set_text(text);
        buffer.place_cursor(&buffer.end_iter());
        editor.completion().emit_show();
        let waited = Rc::new(Cell::new(false));
        let callback = waited.clone();
        gtk::glib::timeout_add_local_once(Duration::from_millis(100), move || callback.set(true));
        until(|| waited.get());
        editor.completion().hide();
    }
    buffer.set_text("SELECT 'draft-restored-marker' AS draft");
    sql_history_ui::save_drafts(&ui, &profile.id).unwrap();
    let path = ui
        .store
        .borrow()
        .as_ref()
        .unwrap()
        .storage_path()
        .to_owned();
    assert!(
        !std::fs::read_to_string(path)
            .unwrap()
            .contains("draft-restored-marker")
    );
    close_profile_sessions(&ui, &profile.id);
    assert!(!ui.sessions.borrow().contains_key(&session));
    assert!(ui.sql_tabs.borrow().is_empty());
    assert!(
        ui.store
            .borrow()
            .as_ref()
            .unwrap()
            .sql_workspace()
            .drafts
            .iter()
            .any(|draft| draft.sql.contains("draft-restored-marker"))
    );
    // An edited endpoint must not inherit the old endpoint's SQL drafts.
    let mut changed_profile = profile.clone();
    changed_profile.database = "another_database".into();
    assert_ne!(
        profile_session_key(&profile),
        profile_session_key(&changed_profile)
    );
    sql_history_ui::restore_drafts(&ui, &changed_profile);
    assert!(ui.sql_tabs.borrow().is_empty());
    let request = "restore-test".to_owned();
    ui.connect_requests
        .borrow_mut()
        .insert(session.clone(), request.clone());
    handle_event(
        &ui,
        Event::Connected(
            request,
            profile.clone(),
            Ok(DbPool::PostgreSql(pool.clone())),
        ),
    );
    assert_eq!(ui.sql_tabs.borrow().len(), 1);
    let restored_id = ui.sql_tabs.borrow().keys().next().unwrap().clone();
    assert!(query_text(&ui.sql_tabs.borrow()[&restored_id]).contains("draft-restored-marker"));
    assert!(ui.sql_tabs.borrow()[&restored_id].request_id.is_empty());
    assert!(ui.sql_tabs.borrow()[&restored_id].result.is_none());
    // Locked storage and failed writes leave the tab and its SQL available.
    let unlocked = ui.store.borrow_mut().take().unwrap();
    *ui.store.borrow_mut() = Some(SettingsStore::load().unwrap());
    assert!(!ui.store.borrow().as_ref().unwrap().vault_is_unlocked());
    close_sql_tab(&ui, &restored_id);
    assert!(ui.sql_tabs.borrow().contains_key(&restored_id));
    *ui.store.borrow_mut() = Some(unlocked);
    let path = ui
        .store
        .borrow()
        .as_ref()
        .unwrap()
        .storage_path()
        .to_owned();
    let displaced = path.with_extension("test-displaced");
    std::fs::rename(&path, &displaced).unwrap();
    std::fs::create_dir(&path).unwrap();
    close_sql_tab(&ui, &restored_id);
    assert!(ui.sql_tabs.borrow().contains_key(&restored_id));
    assert!(query_text(&ui.sql_tabs.borrow()[&restored_id]).contains("draft-restored-marker"));
    std::fs::remove_dir(&path).unwrap();
    std::fs::rename(&displaced, &path).unwrap();
    // The cap includes disconnected profiles without constructing 32 WebViews.
    let inactive: Vec<_> = (0..31)
        .map(|n| sql_workspace::SqlDraft {
            id: format!("inactive-{n}"),
            profile_id: "another-profile".into(),
            connection_key: String::new(),
            schema: None,
            name: format!("legacy-draft-{n}"),
            sql: "SELECT 1".into(),
        })
        .collect();
    ui.store
        .borrow_mut()
        .as_mut()
        .unwrap()
        .replace_sql_drafts("another-profile", inactive)
        .unwrap();
    create_sql_tab(&ui, &session);
    assert_eq!(ui.sql_tabs.borrow().len(), 1);
    assert!(query_text(&ui.sql_tabs.borrow()[&restored_id]).contains("draft-restored-marker"));
    // Legacy drafts remain inspectable and removable even at the global cap.
    sql_history_ui::show(&ui);
    let history = gtk::Window::list_toplevels()
        .into_iter()
        .filter_map(|w| w.downcast::<adw::Window>().ok())
        .find(|w| w.widget_name() == "sql-history" && w.is_visible())
        .unwrap();
    let search = named_widget(history.upcast_ref(), "sql-history-search")
        .unwrap()
        .downcast::<gtk::SearchEntry>()
        .unwrap();
    search.set_text("legacy-draft-0");
    let remove = named_widget(history.upcast_ref(), "sql-draft-remove")
        .unwrap()
        .downcast::<gtk::Button>()
        .unwrap();
    until(|| remove.is_sensitive());
    remove.emit_clicked();
    assert_eq!(
        ui.store
            .borrow()
            .as_ref()
            .unwrap()
            .sql_workspace()
            .drafts
            .len(),
        31
    );
    for window in gtk::Window::list_toplevels() {
        if window.widget_name() == "sql-history" {
            if let Ok(window) = window.downcast::<gtk::Window>() {
                window.close();
            }
        }
    }
    ui.store
        .borrow_mut()
        .as_mut()
        .unwrap()
        .replace_sql_drafts("another-profile", Vec::new())
        .unwrap();
    close_sql_tab(&ui, &restored_id);
    assert!(!ui.table_edit_states.borrow().contains_key(&key));
    ui.window.close();
    runtime.block_on(pool.close());
}

#[test]
#[ignore = "requires GTK display; isolated XDG, no database writes"]
fn sql_editor_assistance_and_connected_icon() {
    gtk::init().unwrap();
    adw::init().unwrap();
    let runtime = Arc::new(
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap(),
    );
    let app = adw::Application::builder()
        .application_id("io.github.dbdeck.sql-assistance-test")
        .build();
    app.register(None::<&gtk::gio::Cancellable>).unwrap();
    let ui = build_window(&app, runtime.clone());
    let profile = ConnectionProfile {
        id: "assistance-test".into(),
        name: "assistance test".into(),
        kind: DatabaseKind::PostgreSql,
        host: "127.0.0.1".into(),
        port: 9,
        database: "dbdeck_test".into(),
        username: "dbdeck_test".into(),
        environment: EnvironmentTag::Local,
        color: "#4f8fe8".into(),
        read_only: true,
        tls_mode: TlsMode::Disabled,
        password_saved: false,
        gopass_entry: None,
        tls_files: Default::default(),
        native_tools_dir: None,
        transport: Default::default(),
    };
    ui.store
        .borrow_mut()
        .as_mut()
        .unwrap()
        .profiles
        .push(profile.clone());
    ui.profile_status
        .borrow_mut()
        .insert(profile.id.clone(), ProfileStatus::Connected);
    let lazy_pool = || {
        let _guard = runtime.enter();
        sqlx::postgres::PgPoolOptions::new()
            .acquire_timeout(Duration::from_millis(200))
            .connect_lazy_with(
                sqlx::postgres::PgConnectOptions::new()
                    .host("127.0.0.1")
                    .port(9),
            )
    };
    let pool = lazy_pool();
    let session = profile_session_key(&profile);
    ui.sessions.borrow_mut().insert(
        session.clone(),
        (profile.clone(), DbPool::PostgreSql(pool.clone())),
    );
    *ui.current_profile.borrow_mut() = Some(profile.clone());
    refresh_profiles(&ui);
    fn connected_image(widget: &gtk::Widget) -> Option<gtk::Image> {
        if widget.has_css_class("connected-action") {
            return widget
                .clone()
                .downcast::<gtk::Button>()
                .ok()?
                .child()?
                .downcast::<gtk::Image>()
                .ok();
        }
        let mut child = widget.first_child();
        while let Some(current) = child {
            child = current.next_sibling();
            if let Some(image) = connected_image(&current) {
                return Some(image);
            }
        }
        None
    }
    let image = connected_image(ui.profiles.upcast_ref()).unwrap();
    assert_eq!(image.pixel_size(), 18);
    for mode in [
        ThemeMode::CatppuccinMocha,
        ThemeMode::TokyoNight,
        ThemeMode::Dark,
    ] {
        let css = install_css();
        theme::apply_mode(mode, &css);
        ui.active_theme.set(mode);
        let expected = gtk::gdk::RGBA::parse(&theme::palette(mode).success).unwrap();
        until(|| image.style_context().color() == expected);
    }
    create_sql_tab(&ui, &session);
    let id = ui.sql_tabs.borrow().keys().next().unwrap().clone();
    // Simulate catalog arrival without any server access or scan delay.
    handle_event(
        &ui,
        Event::SqlCompletionsLoaded(
            id.clone(),
            DbPool::PostgreSql(pool.clone()),
            Ok(database::SqlCompletionCatalog {
                search_path: vec!["public".into()],
                namespaces: [
                    (
                        "public".into(),
                        [
                            ("sample_records".into(), vec!["name".into()]),
                            ("Display Items".into(), vec!["item_name".into()]),
                        ]
                        .into(),
                    ),
                    (
                        "audit".into(),
                        [("sample_records".into(), vec!["event".into()])].into(),
                    ),
                ]
                .into(),
            }),
        ),
    );
    // A reply from a replaced pool must leave the current catalog intact.
    handle_event(
        &ui,
        Event::SqlCompletionsLoaded(
            id.clone(),
            DbPool::PostgreSql(lazy_pool()),
            Ok(Default::default()),
        ),
    );
    let provider = ui.sql_tabs.borrow()[&id].completion_provider.clone();
    let buffer = ui.sql_tabs.borrow()[&id].buffer.clone();
    buffer.set_text("SELEC * FROM sample_records");
    let tag = buffer
        .tag_table()
        .lookup("dbdeck-sql-syntax-error")
        .unwrap();
    until(|| buffer.start_iter().has_tag(&tag));
    assert_eq!(tag.underline(), gtk::pango::Underline::Error);
    buffer.set_text("SELECT * FROM sample_records");
    assert!(
        !buffer.start_iter().has_tag(&tag),
        "obsolete diagnostic is cleared immediately"
    );
    // Rapid edits exercise cancellation/revision guards.
    buffer.set_text("SELECT (");
    buffer.set_text("SELECT 1");
    let waited = Rc::new(Cell::new(false));
    let done = waited.clone();
    gtk::glib::timeout_add_local_once(Duration::from_millis(450), move || done.set(true));
    until(|| waited.get());
    assert!(!buffer.start_iter().has_tag(&tag));
    ui.landing.set_visible(false);
    let editor = ui.sql_tabs.borrow()[&id].editor.clone();
    until(|| editor.is_mapped());
    editor.grab_focus();
    assert!(
        editor.has_focus(),
        "SQL editor must have focus for automatic completion"
    );
    let popover = ui.sql_tabs.borrow()[&id].actions_menu.popover().unwrap();
    let format_button = named_widget(popover.upcast_ref(), "sql-format")
        .unwrap()
        .downcast::<gtk::Button>()
        .unwrap();
    let unformatted = "SELECT  'Имя',2 FROM records WHERE id=1";
    let formatted = "SELECT 'Имя',\n    2\nFROM records\nWHERE id=1";
    let editor_text = || buffer.text(&buffer.start_iter(), &buffer.end_iter(), true);
    let offset_of =
        |text: &str, word: &str| text[..text.find(word).unwrap()].chars().count() as i32;
    buffer.set_text(unformatted);
    buffer.select_range(
        &buffer.end_iter(),
        &buffer.iter_at_offset(offset_of(unformatted, "WHERE")),
    );
    format_button.emit_clicked();
    assert_eq!(editor_text(), formatted);
    let (start, end) = buffer.selection_bounds().unwrap();
    assert_eq!(buffer.text(&start, &end, true), "WHERE id=1");
    assert_eq!(buffer.cursor_position(), buffer.char_count());
    buffer.undo();
    assert_eq!(editor_text(), unformatted);

    assert!(shortcuts::dispatch(
        &ui,
        gtk::gdk::Key::F,
        gtk::gdk::ModifierType::CONTROL_MASK | gtk::gdk::ModifierType::SHIFT_MASK,
    ));
    assert_eq!(editor_text(), formatted);
    buffer.undo();
    assert_eq!(editor_text(), unformatted);

    let projection_end = buffer.iter_at_offset(offset_of(unformatted, ",") + 1);
    buffer.select_range(&buffer.start_iter(), &projection_end);
    format_button.emit_clicked();
    let (start, end) = buffer.selection_bounds().unwrap();
    assert_eq!(buffer.text(&start, &end, true), "SELECT 'Имя',");
    assert_eq!(buffer.cursor_position(), 0);
    buffer.undo();
    assert_eq!(editor_text(), unformatted);

    shortcuts::palette(&ui);
    let palette = gtk::Window::list_toplevels()
        .into_iter()
        .filter_map(|widget| widget.downcast::<adw::Window>().ok())
        .find(|window| window.title().as_deref() == Some("Палитра команд"))
        .unwrap();
    until(|| palette.is_mapped());
    let search = named_widget(palette.upcast_ref(), "command-search")
        .unwrap()
        .downcast::<gtk::SearchEntry>()
        .unwrap();
    let commands = named_widget(palette.upcast_ref(), "command-list")
        .unwrap()
        .downcast::<gtk::ListBox>()
        .unwrap();
    search.set_text("Форматировать SQL");
    until(|| {
        commands.selected_row().is_some_and(|row| {
            row.child()
                .and_then(|child| child.first_child())
                .and_then(|label| label.downcast::<gtk::Label>().ok())
                .is_some_and(|label| label.text() == "Форматировать SQL")
        })
    });
    search.emit_activate();
    assert_eq!(editor_text(), formatted);
    until(|| !palette.is_mapped() && ui.window.is_active());
    editor.grab_focus();

    buffer.set_text("SELECT  id FROM");
    assert!(sql_editor::format_buffer(&editor, sql_editor::Dialect::PostgreSql).is_err());
    assert_eq!(editor_text(), "SELECT  id FROM");

    buffer.set_text("");
    buffer.insert_at_cursor("s");
    buffer.insert_at_cursor("e");
    buffer.insert_at_cursor("l");
    fn editor_completion_context(
        editor: &sourceview5::View,
    ) -> Option<sourceview5::CompletionContext> {
        let mut child = editor.first_child();
        while let Some(widget) = child {
            child = widget.next_sibling();
            if widget.type_().name() == "GtkSourceCompletionList" {
                return widget.property::<Option<sourceview5::CompletionContext>>("context");
            }
        }
        None
    }
    until(|| editor_completion_context(&editor).is_some());
    until(|| {
        let mut child = editor.first_child();
        while let Some(widget) = child {
            child = widget.next_sibling();
            if widget.type_().name() == "GtkSourceCompletionList" {
                return widget.is_mapped() && widget.width() > 0 && widget.height() > 0;
            }
        }
        false
    });
    let context = editor_completion_context(&editor).unwrap();
    let model = gtk::glib::MainContext::default()
        .block_on(provider.populate_future(&context))
        .unwrap();
    assert!(model.n_items() > 0);
    let proposal = model
        .item(0)
        .unwrap()
        .downcast::<sourceview5::CompletionProposal>()
        .unwrap();
    for key in [gtk::gdk::Key::Tab, gtk::gdk::Key::Return] {
        assert!(provider.key_activates(
            &context,
            &proposal,
            gtk::glib::translate::IntoGlib::into_glib(key),
            gtk::gdk::ModifierType::empty()
        ));
        for modifiers in [
            gtk::gdk::ModifierType::CONTROL_MASK,
            gtk::gdk::ModifierType::ALT_MASK,
            gtk::gdk::ModifierType::SHIFT_MASK,
        ] {
            assert!(!provider.key_activates(
                &context,
                &proposal,
                gtk::glib::translate::IntoGlib::into_glib(key),
                modifiers
            ));
        }
    }
    sourceview5::prelude::CompletionProviderExt::activate(&provider, &context, &proposal);
    assert_eq!(
        buffer.text(&buffer.start_iter(), &buffer.end_iter(), true),
        "SELECT"
    );
    buffer.undo();
    assert_eq!(
        buffer.text(&buffer.start_iter(), &buffer.end_iter(), true),
        "sel"
    );
    // Replace only the current identifier, including the suffix to the right
    // of the caret. A same-named table in another schema supplies no fields.
    editor.completion().hide();
    for (text, before_caret, expected) in [
        (
            "SELECT r.na$old FROM public.sample_records AS r",
            "SELECT r.na",
            "SELECT r.\"name\" FROM public.sample_records AS r",
        ),
        (
            "SELECT * FROM public.sample_records_old",
            "SELECT * FROM public.sample_records",
            "SELECT * FROM public.sample_records",
        ),
        (
            "SELECT * FROM public.\"Display I",
            "SELECT * FROM public.\"Display I",
            "SELECT * FROM public.\"Display Items\"",
        ),
        (
            "SELECT * FROM public.\"Display Invalid\" AS d",
            "SELECT * FROM public.\"Display I",
            "SELECT * FROM public.\"Display Items\" AS d",
        ),
    ] {
        buffer.set_text(text);
        buffer.place_cursor(&buffer.iter_at_offset(before_caret.len() as i32));
        editor.completion().emit_show();
        until(|| editor_completion_context(&editor).is_some());
        let context = editor_completion_context(&editor).unwrap();
        let model = gtk::glib::MainContext::default()
            .block_on(provider.populate_future(&context))
            .unwrap();
        assert_eq!(model.n_items(), 1, "{text}");
        let proposal = model
            .item(0)
            .unwrap()
            .downcast::<sourceview5::CompletionProposal>()
            .unwrap();
        sourceview5::prelude::CompletionProviderExt::activate(&provider, &context, &proposal);
        assert_eq!(
            buffer.text(&buffer.start_iter(), &buffer.end_iter(), true),
            expected
        );
        buffer.undo();
        assert_eq!(
            buffer.text(&buffer.start_iter(), &buffer.end_iter(), true),
            text
        );
        editor.completion().hide();
    }
    // Context-sensitive suppression inside comments and string literals.
    for text in ["-- sel", "/* sel */", "SELECT 'sel'", "SELECT sel"] {
        buffer.set_text(text);
        buffer.place_cursor(&buffer.iter_at_offset((text.find("sel").unwrap() + 3) as i32));
        editor.completion().emit_show();
        let context = editor_completion_context(&editor).unwrap();
        let model = gtk::glib::MainContext::default()
            .block_on(provider.populate_future(&context))
            .unwrap();
        assert_eq!(model.n_items() > 0, text == "SELECT sel", "{text}");
    }
    for mode in [ThemeMode::TokyoNight, ThemeMode::Dark] {
        ui.active_theme.set(mode);
        refresh_sql_editor_themes(&ui);
        assert_eq!(
            tag.underline_rgba().unwrap(),
            gtk::gdk::RGBA::parse(&theme::palette(mode).error).unwrap()
        );
    }
    close_sql_tab(&ui, &id);
    ui.profile_status.borrow_mut().remove(&profile.id);
    ui.sessions.borrow_mut().clear();
    refresh_profiles(&ui);
    assert!(connected_image(ui.profiles.upcast_ref()).is_none());
    ui.window.close();
    runtime.block_on(pool.close());
}

#[test]
#[ignore = "requires isolated GTK display, synthetic workspace only"]
fn command_palette_tracks_context_and_keyboard_selection() {
    gtk::init().unwrap();
    adw::init().unwrap();
    let runtime = Arc::new(tokio::runtime::Runtime::new().unwrap());
    let app = adw::Application::builder()
        .application_id("io.github.dbdeck.palette-test")
        .build();
    app.register(None::<&gtk::gio::Cancellable>).unwrap();
    let ui = build_window(&app, runtime.clone());
    until(|| ui.window.is_mapped());

    let open_palette = || {
        shortcuts::palette(&ui);
        let window = gtk::Window::list_toplevels()
            .into_iter()
            .filter_map(|widget| widget.downcast::<adw::Window>().ok())
            .find(|window| {
                window.title().as_deref() == Some("Палитра команд") && window.is_visible()
            })
            .unwrap();
        until(|| window.is_mapped());
        let search = named_widget(window.upcast_ref(), "command-search")
            .unwrap()
            .downcast::<gtk::SearchEntry>()
            .unwrap();
        let list = named_widget(window.upcast_ref(), "command-list")
            .unwrap()
            .downcast::<gtk::ListBox>()
            .unwrap();
        (window, search, list)
    };
    let selected = |list: &gtk::ListBox| {
        list.selected_row().map(|row| {
            row.child()
                .unwrap()
                .first_child()
                .unwrap()
                .downcast::<gtk::Label>()
                .unwrap()
                .text()
        })
    };
    let (palette, search, list) = open_palette();
    let ctrl = gtk::gdk::ModifierType::CONTROL_MASK;
    let shift = gtk::gdk::ModifierType::SHIFT_MASK;
    search.set_text("форматировать");
    assert_eq!(selected(&list), None);
    search.emit_activate();
    assert!(palette.is_visible());
    search.set_text("SQL новая");
    assert_eq!(selected(&list), None);
    search.set_text("Настройки");
    assert_eq!(selected(&list).as_deref(), Some("Настройки"));
    search.emit_activate();
    until(|| ui.preferences.is_mapped());
    ui.preferences.set_visible(false);

    let profile: ConnectionProfile = serde_json::from_value(serde_json::json!({
        "id": "palette", "name": "Palette test", "kind": "postgre_sql", "host": "localhost",
        "port": 5432, "database": "palette", "username": "local",
        "environment": "local", "color": "#ffffff", "read_only": true, "tls_mode": "disabled"
    }))
    .unwrap();
    let session = profile_session_key(&profile);
    let new_pool = || {
        let _guard = runtime.enter();
        DbPool::PostgreSql(
            sqlx::postgres::PgPoolOptions::new()
                .connect_lazy_with(sqlx::postgres::PgConnectOptions::new().socket("/nonexistent")),
        )
    };
    ui.sessions
        .borrow_mut()
        .insert(session.clone(), (profile.clone(), new_pool()));
    *ui.current_profile.borrow_mut() = Some(profile.clone());
    create_sql_tab(&ui, &session);
    let id = ui.sql_tabs.borrow().keys().next().unwrap().clone();
    let (buffer, run_button, stop_button, grid) = {
        let tabs = ui.sql_tabs.borrow();
        let tab = &tabs[&id];
        (
            tab.buffer.clone(),
            tab.run_button.clone(),
            tab.stop_button.clone(),
            tab.tabulator.clone(),
        )
    };
    ui.landing.set_visible(false);
    buffer.set_text("  \n");
    let (palette, search, list) = open_palette();
    search.set_text("выполнить sql");
    assert_eq!(selected(&list), None);
    palette.close();
    buffer.set_text("SELECT  1");

    let (palette, search, list) = open_palette();
    for unavailable in [
        "применить фильтр",
        "импорт",
        "с правом записи",
        "остановить sql",
        "фокус результат",
    ] {
        search.set_text(unavailable);
        assert_eq!(selected(&list), None, "{unavailable}");
    }
    search.set_text("SQL формат");
    assert_eq!(selected(&list).as_deref(), Some("Форматировать SQL"));
    let shortcut = gtk::accelerator_get_label(gtk::gdk::Key::f, ctrl | shift);
    search.set_text(&shortcut);
    assert_eq!(selected(&list).as_deref(), Some("Форматировать SQL"));
    search.set_text("sql");
    let first = list.selected_row().unwrap().index();
    let focus = gtk::prelude::RootExt::focus(&palette).unwrap();
    let controllers = search.observe_controllers();
    let keys = (0..controllers.n_items())
        .filter_map(|n| {
            controllers
                .item(n)?
                .downcast::<gtk::EventControllerKey>()
                .ok()
        })
        .find(|keys| keys.propagation_phase() == gtk::PropagationPhase::Capture)
        .unwrap();
    let press = |key: gtk::gdk::Key| {
        keys.emit_by_name::<bool>(
            "key-pressed",
            &[&key, &0u32, &gtk::gdk::ModifierType::LOCK_MASK],
        )
    };
    assert!(press(gtk::gdk::Key::Down));
    assert!(list.selected_row().unwrap().index() > first);
    assert_eq!(
        gtk::prelude::RootExt::focus(&palette).as_ref(),
        Some(&focus)
    );
    assert!(press(gtk::gdk::Key::Up));
    assert_eq!(list.selected_row().unwrap().index(), first);
    search.set_text("SQL формат");
    search.emit_activate();
    assert_eq!(
        buffer.text(&buffer.start_iter(), &buffer.end_iter(), true),
        "SELECT 1"
    );

    ui.sql_tabs.borrow()[&id]
        .result_stack
        .set_visible_child_name("data");
    until(|| grid.webview.is_mapped() && ui.window.is_active());
    assert!(grid.webview.grab_focus());
    // Observe the existing WebKit bridge entry point with a synthetic edit.
    until(|| js(&grid, "typeof window.dbdeckFinishEdit === 'function'") == true);
    assert_eq!(
        js(
            &grid,
            "window.dbdeckFinishEdit = action => window.paletteEdit = action; true"
        ),
        true
    );
    let edit = ui.table_edit_states.borrow()[&format!("sql::{id}")].clone();
    edit.grid_editing.set(true);
    update_table_edit_controls(&edit, None);
    let (_, search, list) = open_palette();
    for unavailable in ["выполнить sql", "сбросить sql-сессию", "ручной режим"]
    {
        search.set_text(unavailable);
        assert_eq!(selected(&list), None, "{unavailable}");
    }
    search.set_text("сохранить");
    assert_eq!(
        selected(&list).as_deref(),
        Some("Сохранить изменения результата SQL")
    );
    search.emit_activate();
    until(|| js(&grid, "window.paletteEdit ?? null") == "save");
    until(|| ui.window.is_active());
    assert!(grid.webview.grab_focus());
    let (_, search, list) = open_palette();
    search.set_text("отменить результат");
    assert_eq!(
        selected(&list).as_deref(),
        Some("Отменить правки результата SQL")
    );
    search.emit_activate();
    until(|| js(&grid, "window.paletteEdit ?? null") == "discard");
    edit.grid_editing.set(false);
    update_table_edit_controls(&edit, None);

    stop_button.set_sensitive(true);
    run_button.set_sensitive(false);
    let (palette, search, list) = open_palette();
    search.set_text("остановить");
    assert_eq!(selected(&list).as_deref(), Some("Остановить SQL-запрос"));
    stop_button.set_sensitive(false);
    run_button.set_sensitive(true);
    search.emit_activate();
    assert!(!palette.is_visible());
    assert!(ui.status.text().contains("Команда больше недоступна"));
    buffer.set_text("SELECT  1");
    for replace_pool in [true, false] {
        let (palette, search, list) = open_palette();
        search.set_text("формат sql");
        assert_eq!(selected(&list).as_deref(), Some("Форматировать SQL"));
        if replace_pool {
            ui.sessions
                .borrow_mut()
                .insert(session.clone(), (profile.clone(), new_pool()));
        } else {
            replace_tab_session(&ui, &id);
        }
        search.emit_activate();
        assert!(!palette.is_visible());
        assert_eq!(
            buffer.text(&buffer.start_iter(), &buffer.end_iter(), true),
            "SELECT  1"
        );
    }
    close_sql_tab(&ui, &id);
    ui.window.close();
}

#[test]
#[ignore = "requires isolated GUI display; dialogs/settings only, no database writes"]
fn popup_dismissal_and_file_locations() {
    gtk::init().unwrap();
    adw::init().unwrap();
    let runtime = Arc::new(tokio::runtime::Runtime::new().unwrap());
    let app = adw::Application::builder()
        .application_id("io.github.dbdeck.dismiss-test")
        .build();
    app.register(None::<&gtk::gio::Cancellable>).unwrap();
    let ui = build_window(&app, runtime);
    until(|| ui.window.is_mapped());
    // Real Capture controllers are installed above SearchEntry's Escape handling.
    fn escape(window: &impl IsA<gtk::Window>) {
        let window = window.as_ref();
        let controllers = window.observe_controllers();
        for n in 0..controllers.n_items() {
            if let Some(key) = controllers
                .item(n)
                .and_then(|c| c.downcast::<gtk::EventControllerKey>().ok())
            {
                if key.propagation_phase() == gtk::PropagationPhase::Capture {
                    let _: bool = key.emit_by_name(
                        "key-pressed",
                        &[
                            &gtk::gdk::Key::Escape,
                            &0u32,
                            &gtk::gdk::ModifierType::empty(),
                        ],
                    );
                    if !window.is_visible() {
                        return;
                    }
                }
            }
        }
        panic!("Escape did not close popup");
    }
    fn outside(window: &impl IsA<gtk::Window>) {
        let controllers = window.as_ref().observe_controllers();
        for n in 0..controllers.n_items() {
            if let Some(click) = controllers
                .item(n)
                .and_then(|c| c.downcast::<gtk::GestureClick>().ok())
            {
                if click.propagation_phase() == gtk::PropagationPhase::Capture
                    && click.button() == 0
                {
                    click.emit_by_name::<()>("pressed", &[&1i32, &2f64, &2f64]);
                    return;
                }
            }
        }
        panic!("No outside-click controller");
    }
    for keyboard in [true, false] {
        shortcuts::palette(&ui);
        let palette = gtk::Window::list_toplevels()
            .into_iter()
            .filter_map(|w| w.downcast::<gtk::Window>().ok())
            .find(|w| w.title().as_deref() == Some("Палитра команд") && w.is_visible())
            .unwrap();
        until(|| palette.is_mapped());
        let search = named_widget(palette.upcast_ref(), "command-search")
            .unwrap()
            .downcast::<gtk::SearchEntry>()
            .unwrap();
        search.set_text("SQL");
        search.grab_focus();
        if keyboard {
            escape(&palette);
        } else {
            outside(&ui.window);
        }
        assert!(!palette.is_visible());
    }
    ui.preferences.present();
    until(|| ui.preferences.is_mapped());
    assert!(named_widget(ui.preferences.upcast_ref(), "settings-sql-directory").is_some());
    assert!(named_widget(ui.preferences.upcast_ref(), "settings-csv-directory").is_some());
    let message = gtk::MessageDialog::builder()
        .transient_for(&ui.window)
        .text("confirmation")
        .buttons(gtk::ButtonsType::YesNo)
        .build();
    dialogs::install(&message);
    let accepted = Rc::new(Cell::new(false));
    let result = accepted.clone();
    message.connect_response(move |dialog, response| {
        result.set(response == gtk::ResponseType::Yes);
        dialog.close();
    });
    message.present();
    until(|| message.is_mapped());
    outside(&ui.preferences);
    assert!(!message.is_visible());
    assert!(ui.preferences.is_visible());
    assert!(!accepted.get());
    escape(&ui.preferences);
    ui.store
        .borrow_mut()
        .as_mut()
        .unwrap()
        .unlock_or_create_vault("synthetic test key")
        .unwrap();
    let folder = std::env::temp_dir().join(format!("dbdeck-folders-{}", Uuid::new_v4()));
    fs::create_dir(&folder).unwrap();
    fn folder_chooser(title: &str) -> gtk::FileChooserDialog {
        gtk::Window::list_toplevels()
            .into_iter()
            .filter_map(|w| w.downcast::<gtk::FileChooserDialog>().ok())
            .find(|w| w.title().as_deref() == Some(title) && w.is_visible())
            .unwrap()
    }
    choose_sql_queries_directory(&ui);
    let sql_folder = folder_chooser("Папка SQL-запросов");
    sql_folder
        .set_current_folder(Some(&gtk::gio::File::for_path(&folder)))
        .unwrap();
    until(|| sql_folder.current_folder().and_then(|f| f.path()).as_ref() == Some(&folder));
    sql_folder.response(gtk::ResponseType::Accept);
    assert_eq!(
        ui.store.borrow().as_ref().unwrap().sql_queries_directory(),
        Some(folder.clone())
    );
    assert_eq!(
        ui.sql_directory_label.text(),
        folder.to_string_lossy().as_ref()
    );
    choose_csv_directory(&ui);
    let csv_folder = folder_chooser("Папка CSV");
    csv_folder
        .set_current_folder(Some(&gtk::gio::File::for_path(&folder)))
        .unwrap();
    until(|| csv_folder.current_folder().and_then(|f| f.path()).as_ref() == Some(&folder));
    csv_folder.response(gtk::ResponseType::Accept);
    assert_eq!(
        ui.store.borrow().as_ref().unwrap().csv_export_directory(),
        Some(folder.clone())
    );
    assert_eq!(
        ui.csv_directory_label.text(),
        folder.to_string_lossy().as_ref()
    );
    let check = dialogs::file_chooser(
        Some("CSV remembered"),
        Some(&ui.window),
        gtk::FileChooserAction::Save,
        Some("Save"),
        Some("Cancel"),
    );
    configure_csv_chooser(&ui, &check);
    until(|| check.current_folder().and_then(|f| f.path()).as_ref() == Some(&folder));
    check.destroy();
    fs::remove_dir(folder).unwrap();
    let chooser = dialogs::file_chooser(
        Some("Test SQL save"),
        Some(&ui.window),
        gtk::FileChooserAction::Save,
        Some("Save"),
        Some("Cancel"),
    );
    chooser.show();
    until(|| chooser.is_mapped());
    escape(&chooser);
    assert!(!chooser.is_visible());
    ui.window.close();
}

#[test]
#[ignore = "requires isolated GTK display and Compose PostgreSQL; read-only browser navigation"]
fn postgres_server_browser_navigation_and_icons() {
    gtk::init().unwrap();
    adw::init().unwrap();
    let runtime = Arc::new(tokio::runtime::Runtime::new().unwrap());
    let profile = ConnectionProfile {
        id: "pg-browser-ui".into(),
        name: "server".into(),
        kind: DatabaseKind::PostgreSql,
        host: "127.0.0.1".into(),
        port: 55432,
        database: String::new(),
        username: "dbdeck_test".into(),
        environment: EnvironmentTag::Local,
        color: "#4f8fe8".into(),
        read_only: true,
        tls_mode: TlsMode::Disabled,
        password_saved: false,
        gopass_entry: None,
        tls_files: Default::default(),
        native_tools_dir: None,
        transport: Default::default(),
    };
    let pool = runtime
        .block_on(DbPool::connect(
            &profile,
            &std::env::var("DBDECK_PG_PASSWORD").unwrap(),
        ))
        .unwrap();
    let app = adw::Application::builder()
        .application_id("io.github.dbdeck.pg-browser-test")
        .build();
    app.register(None::<&gtk::gio::Cancellable>).unwrap();
    let ui = build_window(&app, runtime.clone());
    {
        let mut store = ui.store.borrow_mut();
        let store = store.as_mut().unwrap();
        store
            .unlock_or_create_vault("synthetic browser key")
            .unwrap();
        store
            .save_profile(
                profile.clone(),
                &std::env::var("DBDECK_PG_PASSWORD").unwrap(),
            )
            .unwrap();
    }
    let root_key = profile_session_key(&profile);
    ui.sessions
        .borrow_mut()
        .insert(root_key.clone(), (profile.clone(), pool));
    *ui.current_profile.borrow_mut() = Some(profile.clone());
    *ui.pool.borrow_mut() = Some(ui.sessions.borrow()[&root_key].1.clone());
    add_session_tab(&ui, &profile);
    assert_eq!(
        ui.session_tabs.n_pages(),
        0,
        "server browser is not a database group"
    );
    assert!(!ui.session_tabs.is_visible());
    ui.landing.set_visible(false);
    show_table_catalog(&ui);
    /// Tree rows may be wrapped with a hover action in a `tree-row-line` box.
    fn row_button(widget: &gtk::Widget) -> Option<gtk::Button> {
        if let Some(button) = widget.downcast_ref::<gtk::Button>() {
            return Some(button.clone());
        }
        if widget.has_css_class("tree-row-line") {
            return widget.first_child().and_downcast::<gtk::Button>();
        }
        None
    }
    /// The children box that follows a tree row (after its wrapper, if any).
    fn row_children(button: &gtk::Button) -> Option<gtk::Widget> {
        match button.parent() {
            Some(line) if line.has_css_class("tree-row-line") => line.next_sibling(),
            _ => button.next_sibling(),
        }
    }
    fn database_button(ui: &Ui, database: &str) -> Option<gtk::Button> {
        let mut child = ui.objects.first_child();
        while let Some(widget) = child {
            child = widget.next_sibling();
            if let Some(button) = row_button(&widget) {
                if button.tooltip_text().as_deref()
                    == Some(format!("Открыть базу {database}").as_str())
                {
                    return Some(button);
                }
            }
        }
        None
    }
    fn contains_text(widget: &gtk::Widget, text: &str) -> bool {
        if widget
            .downcast_ref::<gtk::Label>()
            .is_some_and(|label| label.text() == text)
        {
            return true;
        }
        let mut child = widget.first_child();
        while let Some(widget) = child {
            child = widget.next_sibling();
            if contains_text(&widget, text) {
                return true;
            }
        }
        false
    }
    fn schema_button(parent: &gtk::Widget, name: &str) -> gtk::Button {
        let mut child = parent.first_child();
        while let Some(widget) = child {
            child = widget.next_sibling();
            if let Some(button) = row_button(&widget) {
                if contains_text(button.upcast_ref(), name) {
                    return button;
                }
            }
        }
        panic!("schema {name} must be present");
    }
    until(|| database_button(&ui, "dbdeck_test").is_some());
    let theme = gtk::IconTheme::for_display(&gtk::gdk::Display::default().unwrap());
    for name in [
        "postgresql",
        "mariadb",
        "mysql",
        "database-open",
        "database-closed",
    ] {
        assert!(theme.has_icon(&format!("dbdeck-{name}-symbolic")));
    }
    database_button(&ui, "dbdeck_test").unwrap().emit_clicked();
    until(|| {
        ui.table_list_items
            .borrow()
            .iter()
            .any(|(_, o)| o.schema == "crm" && o.name == "events")
    });
    assert_eq!(ui.sessions.borrow().len(), 2);
    assert_eq!(
        ui.session_tabs.n_pages(),
        0,
        "a database opens without a schema tab"
    );
    assert!(
        database_button(&ui, "dbdeck_test")
            .unwrap()
            .has_css_class("database-tree-row-active")
    );
    assert!(
        database_button(&ui, "postgres").is_some(),
        "siblings remain in the tree"
    );
    assert!(!contains_text(ui.objects.upcast_ref(), "← Все базы данных"));
    let crm = schema_button(
        &row_children(&database_button(&ui, "dbdeck_test").unwrap()).unwrap(),
        "crm",
    );
    crm.emit_clicked();
    assert!(row_children(&crm).unwrap().is_visible());
    database_button(&ui, "postgres").unwrap().emit_clicked();
    until(|| {
        ui.current_profile
            .borrow()
            .as_ref()
            .is_some_and(|p| p.database == "postgres")
            && database_button(&ui, "postgres")
                .is_some_and(|b| row_children(&b).is_some_and(|w| contains_text(&w, "public")))
    });
    assert_eq!(ui.sessions.borrow().len(), 3);
    assert_eq!(
        ui.session_tabs.n_pages(),
        1,
        "only the explicitly chosen crm schema has a tab"
    );
    let postgres = database_button(&ui, "postgres").unwrap();
    assert!(postgres.has_css_class("database-tree-row-active"));
    let test_db = database_button(&ui, "dbdeck_test").unwrap();
    assert!(!test_db.has_css_class("database-tree-row-active"));
    assert!(row_children(&test_db).unwrap().is_visible());
    assert!(
        row_children(&schema_button(&row_children(&test_db).unwrap(), "crm"))
            .unwrap()
            .is_visible(),
        "schema expansion is remembered"
    );
    let public = schema_button(&row_children(&postgres).unwrap(), "public");
    assert!(contains_text(
        &row_children(&public).unwrap(),
        "Нет таблиц и представлений"
    ));
    layout_snapshot(&ui.window, "target/postgres-inline-tree.png");
    // A table in another expanded DB must use that DB's pool, not the active postgres pool.
    let table = ui
        .table_list_items
        .borrow()
        .iter()
        .find(|(_, o)| o.schema == "crm" && o.name == "events")
        .unwrap()
        .0
        .clone();
    table.emit_clicked();
    until(|| {
        ui.current_profile.borrow().as_ref().unwrap().database == "dbdeck_test"
            && !ui.table_data.borrow().is_empty()
    });
    assert!(
        database_button(&ui, "dbdeck_test")
            .unwrap()
            .has_css_class("database-tree-row-active")
    );
    assert!(
        !database_button(&ui, "postgres")
            .unwrap()
            .has_css_class("database-tree-row-active")
    );
    let actual = runtime
        .block_on(
            ui.pool
                .borrow()
                .as_ref()
                .unwrap()
                .execute_sql("SELECT current_database() AS name"),
        )
        .unwrap();
    assert_eq!(
        actual.data.rows,
        [vec![CellValue::Text("dbdeck_test".into())]]
    );
    // The connection is shown in the status bar block, not in a header above the tabs.
    until(|| ui.connection_block.root.is_visible() && ui.connection_block.root.width() > 0);
    let grid = ui.tabulator_grids.borrow().values().next().unwrap().clone();
    until(|| {
        js(
            &grid,
            "Boolean(typeof table !== 'undefined' && table && table.getRows().length > 0 && !updatingData)",
        ) == true
    });
    layout_snapshot(&ui.window, "target/postgres-inline-active-table.png");
    // A retained button from a closed database must never use the other database's pool.
    let closed_key = profile_session_key(ui.current_profile.borrow().as_ref().unwrap());
    database_button(&ui, "postgres").unwrap().emit_clicked();
    until(|| ui.current_profile.borrow().as_ref().unwrap().database == "postgres");
    close_session(&ui, &closed_key);
    table.emit_clicked();
    assert_eq!(
        ui.current_profile.borrow().as_ref().unwrap().database,
        "postgres"
    );
    assert!(!ui.sessions.borrow().contains_key(&closed_key));
    assert!(!contains_text(
        &row_children(&database_button(&ui, "dbdeck_test").unwrap()).unwrap(),
        "crm"
    ));
    database_button(&ui, "dbdeck_test").unwrap().emit_clicked();
    until(|| {
        ui.current_profile.borrow().as_ref().unwrap().database == "dbdeck_test"
            && ui
                .table_list_items
                .borrow()
                .iter()
                .any(|(_, o)| o.schema == "crm" && o.name == "events")
    });
    // Closing the server tab leaves the inline database tree usable.
    close_session(&ui, &root_key);
    assert!(!ui.sessions.borrow().contains_key(&root_key));
    assert!(database_button(&ui, "postgres").is_some());
    select_connection_database(&ui, &profile);
    export_app_tests::select_target_database("dbdeck_test");
    until(|| ui.current_profile.borrow().as_ref().unwrap().database == "dbdeck_test");
    assert!(!ui.sessions.borrow().contains_key(&root_key));
    // Collapse still selects the database; expansion and active state are separate.
    database_button(&ui, "dbdeck_test").unwrap().emit_clicked();
    until(|| ui.current_profile.borrow().as_ref().unwrap().database == "dbdeck_test");

    let current_key = profile_session_key(ui.current_profile.borrow().as_ref().unwrap());
    let current_pool = ui.pool.borrow().clone().unwrap();
    let mut failed = profile.clone();
    failed.database = "missing".into();
    let failed_key = profile_session_key(&failed);
    ui.connect_requests
        .borrow_mut()
        .insert(failed_key.clone(), "failed".into());
    ui.pending_database_opens
        .borrow_mut()
        .insert(failed_key.clone());
    *ui.desired_database_session.borrow_mut() = Some(failed_key);
    handle_event(
        &ui,
        Event::Connected("failed".into(), failed, Err("test CONNECT denied".into())),
    );
    assert!(matches!(
        ui.profile_status.borrow().get(&profile.id),
        Some(ProfileStatus::Connected)
    ));
    for window in gtk::Window::list_toplevels() {
        if let Ok(window) = window.downcast::<gtk::Window>() {
            if window.transient_for().is_some() {
                window.close();
            }
        }
    }
    // A late success must not steal focus from the more recently selected DB.
    let mut late = profile.clone();
    late.database = "late_never_selected".into();
    let late_key = profile_session_key(&late);
    ui.connect_requests
        .borrow_mut()
        .insert(late_key.clone(), "late".into());
    ui.pending_database_opens
        .borrow_mut()
        .insert(late_key.clone());
    *ui.desired_database_session.borrow_mut() = Some(current_key.clone());
    handle_event(
        &ui,
        Event::Connected("late".into(), late, Ok(current_pool.clone())),
    );
    assert_eq!(
        profile_session_key(ui.current_profile.borrow().as_ref().unwrap()),
        current_key
    );
    assert!(!ui.sessions.borrow().contains_key(&late_key));
    assert!(ui.connect_requests.borrow().is_empty());
    for n in 0..16 {
        ui.pending_database_opens
            .borrow_mut()
            .insert(format!("synthetic-{n}"));
    }
    open_database_session(&ui, profile.clone(), current_pool, "another");
    assert!(
        ui.connect_requests.borrow().is_empty(),
        "limit must refuse another pool"
    );
    assert!(ui.status.text().contains("лимит 16"));
    ui.pending_database_opens.borrow_mut().clear();
    ui.window.close();
}
