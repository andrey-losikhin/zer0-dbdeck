use super::*;
use grid_app_tests::until;

fn choices(ui: &Ui) -> Vec<String> {
    let Some(model) = ui.namespace_selector.model() else {
        return Vec::new();
    };
    (0..model.n_items())
        .map(|i| {
            model
                .item(i)
                .unwrap()
                .downcast::<gtk::StringObject>()
                .unwrap()
                .string()
                .to_string()
        })
        .collect()
}

fn selected(ui: &Ui) -> Option<String> {
    ui.namespace_selector
        .selected_item()
        .and_downcast::<gtk::StringObject>()
        .map(|s| s.string().to_string())
}

fn choose(ui: &Ui, value: &str) {
    until(|| ui.namespace_selector.is_sensitive() && choices(ui).iter().any(|v| v == value));
    let index = choices(ui).iter().position(|v| v == value).unwrap() as u32;
    ui.namespace_selector.set_selected(index);
}

fn setup(kind: DatabaseKind, port: u16, password_key: &str) -> (Rc<Ui>, ConnectionProfile, DbPool) {
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
        "id":format!("namespace-{port}"), "name":"Namespace test", "kind":kind, "host":"127.0.0.1", "port":port,
        "database":"dbdeck_test", "username":"dbdeck_test", "environment":"local", "color":"#94e2d5",
        "read_only":true, "tls_mode":"disabled"
    })).unwrap();
    let password = std::env::var(password_key).unwrap();
    let pool = runtime
        .block_on(DbPool::connect(&profile, &password))
        .unwrap();
    let app = adw::Application::builder()
        .application_id(format!("io.github.dbdeck.namespace{port}"))
        .build();
    app.register(None::<&gtk::gio::Cancellable>).unwrap();
    let ui = build_window(&app, runtime);
    {
        let mut store = ui.store.borrow_mut();
        let store = store.as_mut().unwrap();
        store
            .unlock_or_create_vault("synthetic namespace test key")
            .unwrap();
        store.save_profile(profile.clone(), &password).unwrap();
    }
    let key = profile_session_key(&profile);
    ui.connect_requests
        .borrow_mut()
        .insert(key, "namespace".into());
    handle_event(
        &ui,
        Event::Connected("namespace".into(), profile.clone(), Ok(pool.clone())),
    );
    until(|| ui.namespace_selector.is_sensitive());
    assert!(ui.namespace_row.is_visible());
    assert!(ui.namespace_selector.enables_search());
    (ui, profile, pool)
}

#[test]
#[ignore = "requires headless GTK and local Compose PostgreSQL"]
fn postgres_schema_dropdown() {
    let (ui, profile, _pool) = setup(DatabaseKind::PostgreSql, 55432, "DBDECK_PG_PASSWORD");
    let session = profile_session_key(&profile);
    assert_eq!(ui.namespace_label.text(), "Схема");
    assert_eq!(selected(&ui).as_deref(), Some("Выберите схему"));
    assert!(ui.workspace_groups.borrow().is_empty());
    assert!(choices(&ui).iter().any(|v| v == "audit"));
    choose(&ui, "audit");
    let audit = workspace_ui::key(&profile, Some("audit"));
    assert_eq!(workspace_ui::active(&ui, &session), audit);
    assert_eq!(selected(&ui).as_deref(), Some("audit"));
    assert!(ui.catalog_tree.borrow().schema_expanded[&(session.clone(), "audit".into())]);
    create_sql_tab(&ui, &session);
    let audit_id = ui.active_table_by_session.borrow()[&audit]
        .strip_prefix("sql::")
        .unwrap()
        .to_owned();
    ui.sql_tabs.borrow()[&audit_id]
        .buffer
        .set_text("SELECT current_schema(), description FROM events ORDER BY id");
    run_sql_tab(&ui, &audit_id);
    until(|| ui.sql_tabs.borrow()[&audit_id].run_button.is_sensitive());
    let data = ui.sql_tabs.borrow()[&audit_id].result.clone().unwrap();
    assert_eq!(data.rows[0][0], CellValue::Text("audit".into()));
    assert!(matches!(&data.rows[0][1], CellValue::Text(s) if s.starts_with("AUDIT:")));
    choose(&ui, "public");
    create_sql_tab(&ui, &session);
    let public = workspace_ui::key(&profile, Some("public"));
    let public_id = ui.active_table_by_session.borrow()[&public]
        .strip_prefix("sql::")
        .unwrap()
        .to_owned();
    ui.sql_tabs.borrow()[&public_id]
        .buffer
        .set_text("SELECT 'public draft'");
    ui.session_tabs
        .set_current_page(session_page_index(&ui, &audit));
    assert_eq!(selected(&ui).as_deref(), Some("audit"));
    assert_eq!(ui.session_tabs.n_pages(), 2);
    assert_eq!(ui.sql_tabs.borrow()[&audit_id].group_key, audit);
    assert!(ui.sql_tabs.borrow().contains_key(&public_id));
    until(|| ui.namespace_selector.is_mapped());
    grid_app_tests::layout_snapshot(&ui.window, "target/postgres-schema-dropdown.png");
    close_session(&ui, &session);
    assert!(!ui.namespace_row.is_visible());
    ui.window.close();
    println!(
        "PostgreSQL dropdown: public/audit choices, group/SQL context, top-tab synchronization, drafts and disconnect visibility passed"
    );
}

#[test]
#[ignore = "requires headless GTK and local Compose MariaDB"]
fn mariadb_database_dropdown() {
    database_dropdown(DatabaseKind::MariaDb, 53306, "DBDECK_MARIADB_PASSWORD");
}

#[test]
#[ignore = "requires headless GTK and local Compose MySQL"]
fn mysql_database_dropdown() {
    database_dropdown(DatabaseKind::MySql, 53307, "DBDECK_MYSQL_PASSWORD");
}

fn database_dropdown(kind: DatabaseKind, port: u16, password_key: &str) {
    let (ui, profile, _pool) = setup(kind, port, password_key);
    assert_eq!(ui.namespace_label.text(), "База данных");
    assert_eq!(selected(&ui).as_deref(), Some("dbdeck_test"));
    assert!(choices(&ui).iter().any(|v| v == "dbdeck_analytics"));
    let first = profile_session_key(&profile);
    create_sql_tab(&ui, &first);
    let first_id = ui.active_table_by_session.borrow()[&first]
        .strip_prefix("sql::")
        .unwrap()
        .to_owned();
    ui.sql_tabs.borrow()[&first_id]
        .buffer
        .set_text("SELECT 'first database draft'");
    choose(&ui, "dbdeck_analytics");
    until(|| {
        ui.current_profile
            .borrow()
            .as_ref()
            .is_some_and(|p| p.database == "dbdeck_analytics")
            && ui.namespace_selector.is_sensitive()
    });
    assert_eq!(selected(&ui).as_deref(), Some("dbdeck_analytics"));
    assert!(
        choices(&ui).iter().any(|v| v == "dbdeck_test"),
        "other DBs disappeared after switching"
    );
    let second = profile_session_key(ui.current_profile.borrow().as_ref().unwrap());
    create_sql_tab(&ui, &second);
    let second_id = ui.active_table_by_session.borrow()[&second]
        .strip_prefix("sql::")
        .unwrap()
        .to_owned();
    ui.sql_tabs.borrow()[&second_id]
        .buffer
        .set_text("SELECT DATABASE() AS selected_database");
    run_sql_tab(&ui, &second_id);
    until(|| ui.sql_tabs.borrow()[&second_id].run_button.is_sensitive());
    assert_eq!(
        ui.sql_tabs.borrow()[&second_id]
            .result
            .as_ref()
            .unwrap()
            .rows[0][0],
        CellValue::Text("dbdeck_analytics".into())
    );
    choose(&ui, "dbdeck_test");
    until(|| {
        ui.current_profile
            .borrow()
            .as_ref()
            .is_some_and(|p| p.database == "dbdeck_test")
    });
    assert!(query_text(&ui.sql_tabs.borrow()[&first_id]).contains("first database draft"));
    assert_eq!(ui.session_tabs.n_pages(), 2);
    ui.session_tabs
        .set_current_page(session_page_index(&ui, &second));
    assert_eq!(selected(&ui).as_deref(), Some("dbdeck_analytics"));
    let before = choices(&ui);
    let revision = ui.namespace_state.borrow().revision;
    let mut root = profile.clone();
    root.database.clear();
    handle_event(
        &ui,
        Event::NamespaceChoices(
            profile_session_key(&root),
            revision.wrapping_sub(1),
            Ok(vec!["stale result".into()]),
        ),
    );
    handle_event(
        &ui,
        Event::NamespaceChoices(
            "another server".into(),
            revision,
            Ok(vec!["wrong server".into()]),
        ),
    );
    assert_eq!(choices(&ui), before);
    assert_eq!(selected(&ui).as_deref(), Some("dbdeck_analytics"));
    for i in 0..16 {
        ui.pending_database_opens
            .borrow_mut()
            .insert(format!("namespace-limit-{i}"));
    }
    choose(&ui, "information_schema");
    assert!(ui.status.text().contains("лимит 16"));
    assert_eq!(
        selected(&ui).as_deref(),
        Some("dbdeck_analytics"),
        "rejected selection must show the active DB"
    );
    assert_eq!(
        ui.current_profile.borrow().as_ref().unwrap().database,
        "dbdeck_analytics"
    );
    ui.pending_database_opens
        .borrow_mut()
        .retain(|key| !key.starts_with("namespace-limit-"));
    until(|| {
        ui.table_list_items
            .borrow()
            .iter()
            .any(|(_, object)| object.schema == "dbdeck_analytics")
    });
    until(|| ui.namespace_selector.is_mapped());
    grid_app_tests::layout_snapshot(&ui.window, &format!("target/database-dropdown-{port}.png"));
    ui.window.close();
    println!(
        "{kind:?} dropdown: both DBs remain available, real DB context, existing groups/drafts, top-tab sync and stale results passed"
    );
}
