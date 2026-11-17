//! A database connection opens no schema tab until the user picks a schema.
use super::*;
use grid_app_tests::until;

fn find_schema_button(widget: &gtk::Widget, tooltip: &str) -> Option<gtk::Button> {
    if widget.has_css_class("schema-tree-row") && widget.tooltip_text().as_deref() == Some(tooltip)
    {
        return widget.clone().downcast::<gtk::Button>().ok();
    }
    let mut child = widget.first_child();
    while let Some(current) = child {
        if let Some(found) = find_schema_button(&current, tooltip) {
            return Some(found);
        }
        child = current.next_sibling();
    }
    None
}

fn has_css_class(widget: &gtk::Widget, class: &str) -> bool {
    if widget.has_css_class(class) {
        return true;
    }
    let mut child = widget.first_child();
    while let Some(current) = child {
        if has_css_class(&current, class) {
            return true;
        }
        child = current.next_sibling();
    }
    false
}

#[test]
#[ignore = "requires headless GTK and local Compose PostgreSQL; read-only"]
fn postgres_schema_opens_on_explicit_choice() {
    gtk::init().unwrap();
    adw::init().unwrap();
    let runtime = Arc::new(
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap(),
    );
    let profile: ConnectionProfile = serde_json::from_value(serde_json::json!({
        "id":"schema-open-ui", "name":"server", "kind":"postgre_sql", "host":"127.0.0.1", "port":55432,
        "database":"dbdeck_test", "username":"dbdeck_test", "environment":"local", "color":"#94e2d5",
        "read_only":true, "tls_mode":"disabled"
    }))
    .unwrap();
    let password = std::env::var("DBDECK_PG_PASSWORD").unwrap();
    let pool = runtime
        .block_on(DbPool::connect(&profile, &password))
        .unwrap();
    let app = adw::Application::builder()
        .application_id("io.github.dbdeck.schema-open-test")
        .build();
    app.register(None::<&gtk::gio::Cancellable>).unwrap();
    let ui = build_window(&app, runtime.clone());
    {
        let mut store = ui.store.borrow_mut();
        let store = store.as_mut().unwrap();
        store
            .unlock_or_create_vault("synthetic schema open test key")
            .unwrap();
        store.save_profile(profile.clone(), &password).unwrap();
    }
    let session = profile_session_key(&profile);
    ui.connect_requests
        .borrow_mut()
        .insert(session.clone(), "connect".into());
    handle_event(
        &ui,
        Event::Connected("connect".into(), profile.clone(), Ok(pool.clone())),
    );
    let tooltip = "dbdeck_test: audit";
    until(|| find_schema_button(ui.objects.upcast_ref(), tooltip).is_some());
    assert_eq!(
        ui.session_tabs.n_pages(),
        0,
        "no schema tab before a choice"
    );
    assert!(ui.workspace_groups.borrow().is_empty());
    assert!(ui.empty_session.is_visible());
    assert!(!ui.empty_session_sql.is_visible());
    let shown = ui
        .namespace_selector
        .selected_item()
        .and_downcast::<gtk::StringObject>()
        .unwrap()
        .string();
    assert_eq!(shown, "Выберите схему");

    find_schema_button(ui.objects.upcast_ref(), tooltip)
        .unwrap()
        .emit_clicked();
    let audit = workspace_ui::key(&profile, Some("audit"));
    assert_eq!(
        ui.session_tabs.n_pages(),
        1,
        "one click opens the schema tab"
    );
    assert!(workspace_ui::is_active(&ui, &audit));
    let tab = ui.session_tabs.nth_page(Some(0)).unwrap();
    let label = ui.session_tabs.tab_label(&tab).unwrap();
    assert!(has_css_class(&label, "tab-schema-name"));
    assert!(ui.empty_session.is_visible());
    assert!(ui.empty_session_sql.is_visible());
    let shown = ui
        .namespace_selector
        .selected_item()
        .and_downcast::<gtk::StringObject>()
        .unwrap()
        .string();
    assert_eq!(shown, "audit");

    until(|| ui.empty_session_sql.is_mapped());
    grid_app_tests::layout_snapshot(&ui.window, "target/schema-open-empty.png");
    ui.empty_session_sql.emit_clicked();
    assert_eq!(ui.session_table_tabs.borrow()[&audit].n_pages(), 1);
    assert!(ui.sql_tabs.borrow().values().any(|t| t.group_key == audit));
    assert!(ui.table_tabs.is_visible());
    grid_app_tests::layout_snapshot(&ui.window, "target/schema-open.png");

    // Re-rendered tree: clicking the already active schema only toggles it.
    find_schema_button(ui.objects.upcast_ref(), tooltip)
        .unwrap()
        .emit_clicked();
    assert_eq!(ui.session_tabs.n_pages(), 1);
    println!("PostgreSQL: no auto schema tab; one click opens schema; empty group offers SQL");
    ui.window.close();
    runtime.block_on(async {
        if let DbPool::PostgreSql(p) = pool {
            p.close().await
        }
    });
}
