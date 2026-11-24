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

/// Whether the PostgreSQL tree of the `dbdeck_test` database lists `schema`.
fn schema_listed(ui: &Ui, schema: &str) -> bool {
    find_schema_button(ui.objects.upcast_ref(), &format!("dbdeck_test: {schema}")).is_some()
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
    if matches!(kind, DatabaseKind::PostgreSql) {
        // PostgreSQL schemas are chosen in the catalog tree, not in the dropdown.
        until(|| ui.objects_available.get());
        assert!(!ui.namespace_row.is_visible());
    } else {
        until(|| ui.namespace_selector.is_sensitive());
        assert!(ui.namespace_row.is_visible());
        assert!(ui.namespace_selector.enables_search());
    }
    (ui, profile, pool)
}

#[test]
#[ignore = "requires headless GTK and local Compose PostgreSQL"]
fn postgres_schema_dropdown() {
    let (ui, profile, _pool) = setup(DatabaseKind::PostgreSql, 55432, "DBDECK_PG_PASSWORD");
    let session = profile_session_key(&profile);
    assert!(ui.workspace_groups.borrow().is_empty());
    // The tree replaces the old schema dropdown: both schemas must be offered there.
    until(|| schema_listed(&ui, "audit") && schema_listed(&ui, "public"));
    find_schema_button(ui.objects.upcast_ref(), "dbdeck_test: audit")
        .unwrap()
        .emit_clicked();
    let audit = workspace_ui::key(&profile, Some("audit"));
    assert_eq!(workspace_ui::active(&ui, &session), audit);
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
    open_schema_group(&ui, &profile, "public");
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
    assert_eq!(workspace_ui::active(&ui, &session), audit);
    assert_eq!(ui.session_tabs.n_pages(), 2);
    assert_eq!(ui.sql_tabs.borrow()[&audit_id].group_key, audit);
    assert!(ui.sql_tabs.borrow().contains_key(&public_id));
    grid_app_tests::layout_snapshot(&ui.window, "target/postgres-schema-dropdown.png");
    close_session(&ui, &session);
    assert!(!ui.namespace_row.is_visible());
    ui.window.close();
    println!(
        "PostgreSQL schemas from the tree: group/SQL context, top-tab synchronization, drafts and hidden dropdown passed"
    );
}

#[test]
#[ignore = "requires headless GTK and local Compose PostgreSQL; read-only"]
fn postgres_tree_is_dropped_after_last_session_closes() {
    let (ui, profile, _pool) = setup(DatabaseKind::PostgreSql, 55432, "DBDECK_PG_PASSWORD");
    until(|| schema_listed(&ui, "public"));
    close_session(&ui, &profile_session_key(&profile));
    until(|| !ui.tab_strip.add_button.is_sensitive());
    show_sidebar(&ui, "tables");
    assert!(ui.sessions.borrow().is_empty());
    assert!(
        ui.objects.first_child().is_none(),
        "the closed connection's tree must not be shown"
    );
    assert!(!schema_listed(&ui, "public"));
    assert!(ui.table_list_items.borrow().is_empty());
    assert!(ui.selected_table_button.borrow().is_none());
    assert!(!ui.objects_available.get());
    assert!(ui.tables_hint.is_visible(), "the hint replaces the tree");
    assert!(!ui.table_filter.is_visible());
    ui.window.close();
}

#[test]
#[ignore = "requires headless GTK and local Compose PostgreSQL; read-only"]
fn postgres_new_sql_button_is_enabled_before_any_table_is_open() {
    let (ui, _profile, _pool) = setup(DatabaseKind::PostgreSql, 55432, "DBDECK_PG_PASSWORD");
    until(|| ui.tab_strip.add_button.is_sensitive());
    assert_eq!(ui.session_tabs.n_pages(), 0, "no tab is open yet");
    assert!(ui.table_data.borrow().is_empty());
    assert!(ui.sql_tabs.borrow().is_empty());
    ui.window.close();
}

#[test]
#[ignore = "requires headless GTK and local Compose PostgreSQL; read-only"]
fn postgres_system_schemas_toggle_shows_and_hides_system_schemas() {
    let (ui, _profile, _pool) = setup(DatabaseKind::PostgreSql, 55432, "DBDECK_PG_PASSWORD");
    until(|| schema_listed(&ui, "public"));
    assert!(!ui.system_schemas_toggle.is_active(), "hidden by default");
    assert!(!schema_listed(&ui, "pg_catalog"));
    assert!(!schema_listed(&ui, "information_schema"));

    ui.system_schemas_toggle.set_active(true);
    assert!(ui.store.borrow().as_ref().unwrap().show_system_schemas());
    until(|| schema_listed(&ui, "pg_catalog") && schema_listed(&ui, "information_schema"));
    assert!(schema_listed(&ui, "public"));

    ui.system_schemas_toggle.set_active(false);
    assert!(!ui.store.borrow().as_ref().unwrap().show_system_schemas());
    until(|| !schema_listed(&ui, "pg_catalog") && !schema_listed(&ui, "information_schema"));
    assert!(schema_listed(&ui, "public"));

    // The checkbox mirrors the stored value, e.g. after the vault is unlocked.
    ui.store
        .borrow_mut()
        .as_mut()
        .unwrap()
        .set_show_system_schemas(true)
        .unwrap();
    storage_ui::sync_system_schemas(&ui);
    assert!(ui.system_schemas_toggle.is_active());
    ui.store
        .borrow_mut()
        .as_mut()
        .unwrap()
        .set_show_system_schemas(false)
        .unwrap();
    storage_ui::sync_system_schemas(&ui);
    assert!(!ui.system_schemas_toggle.is_active());
    ui.window.close();
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
    // Both real databases are open; system ones are hidden, so offer a fresh name.
    let mut probe = before.clone();
    probe.push("dbdeck_limit_probe".into());
    handle_event(
        &ui,
        Event::NamespaceChoices(profile_session_key(&root), revision, Ok(probe)),
    );
    choose(&ui, "dbdeck_limit_probe");
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
    // The tree keeps listing the saved profile scope; the dropdown shows the active DB.
    until(|| {
        ui.table_list_items
            .borrow()
            .iter()
            .any(|(_, object)| object.schema == "dbdeck_test")
    });
    until(|| ui.namespace_selector.is_mapped());
    grid_app_tests::layout_snapshot(&ui.window, &format!("target/database-dropdown-{port}.png"));
    ui.window.close();
    println!(
        "{kind:?} dropdown: both DBs remain available, real DB context, existing groups/drafts, top-tab sync and stale results passed"
    );
}

#[test]
#[ignore = "requires headless GTK; synthetic catalogs, no database servers"]
fn object_search_reveals_paths_and_restores_expansion() {
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
        .application_id("io.github.dbdeck.object-search-test")
        .build();
    app.register(None::<&gtk::gio::Cancellable>).unwrap();
    let ui = build_window(&app, runtime);
    let profile: ConnectionProfile = serde_json::from_value(serde_json::json!({
        "id":"object-search", "name":"Synthetic", "kind":"postgre_sql",
        "host":"127.0.0.1", "port":5432, "database":"sales", "username":"test",
        "environment":"local", "color":"#94e2d5", "read_only":true
    }))
    .unwrap();
    *ui.current_profile.borrow_mut() = Some(profile.clone());
    let catalog = |schema: &str, names: &[&str]| TableCatalog {
        schemas: vec![schema.into()],
        objects: names
            .iter()
            .map(|name| DbObject {
                schema: schema.into(),
                name: (*name).into(),
                kind: database::DbObjectKind::Table,
            })
            .collect(),
        truncated: false,
    };
    catalog_tree::prepare(&ui, &profile);
    catalog_tree::loaded(&ui, &profile, catalog("public", &["orders", "customers"]));
    let mut other = profile.clone();
    other.database = "archive".into();
    catalog_tree::loaded(&ui, &other, catalog("Аудит", &["События", "orders_old"]));
    catalog_tree::databases_loaded(
        &ui,
        &profile,
        vec!["sales".into(), "archive".into(), "unloaded".into()],
    );
    ui.catalog_tree
        .borrow_mut()
        .schema_expanded
        .insert((profile_session_key(&other), "Аудит".into()), false);
    catalog_tree::render(&ui);

    let branch = |name: &str| {
        let branches = ui.object_search_branches.borrow();
        let b = branches.iter().find(|b| b.name == name).unwrap();
        (b.line.clone(), b.children.clone(), b.disclosure.clone())
    };
    let visible_tables = || {
        ui.table_list_items
            .borrow()
            .iter()
            .filter(|(b, _)| b.is_visible())
            .map(|(_, o)| o.name.clone())
            .collect::<Vec<_>>()
    };
    let search = |query: &str| {
        ui.table_filter.set_text(query);
        // Exercise the real search signal without waiting for SearchEntry's debounce.
        ui.table_filter.emit_by_name::<()>("search-changed", &[]);
    };
    assert!(!branch("archive").1.is_visible());
    assert!(!branch("Аудит").1.is_visible());
    search("  СОБЫТИЯ  ");
    assert_eq!(visible_tables(), ["События"]);
    assert!(!branch("sales").0.is_visible());
    assert!(branch("archive").1.is_visible());
    assert!(branch("Аудит").1.is_visible());
    assert_eq!(branch("archive").2.text(), "▾");
    assert!(ui.sessions.borrow().is_empty(), "search must not connect");
    assert!(
        ui.workspace_groups.borrow().is_empty(),
        "search must not open tabs"
    );
    search("аудит");
    assert_eq!(visible_tables(), ["События", "orders_old"]);
    search("ARCHIVE");
    assert_eq!(visible_tables(), ["События", "orders_old"]);
    search("orders");
    assert_eq!(visible_tables(), ["orders", "orders_old"]);
    assert!(branch("sales").1.is_visible());
    assert!(branch("archive").1.is_visible());
    // Metadata re-render keeps the active query and the user's expansion state.
    catalog_tree::render(&ui);
    assert_eq!(visible_tables(), ["orders", "orders_old"]);
    assert!(branch("archive").1.is_visible());
    search("missing");
    assert!(visible_tables().is_empty());
    assert!(!branch("sales").0.is_visible());
    assert!(!branch("archive").0.is_visible());
    let mut child = ui.objects.first_child();
    let mut empty_visible = false;
    while let Some(widget) = child {
        child = widget.next_sibling();
        if widget.widget_name() == "object-search-empty" {
            empty_visible = widget.is_visible();
        }
    }
    assert!(empty_visible);
    search("unloaded");
    assert!(branch("unloaded").0.is_visible());
    assert!(visible_tables().is_empty());
    search("   ");
    assert!(branch("sales").1.is_visible());
    assert!(!branch("archive").1.is_visible());
    assert!(!branch("Аудит").1.is_visible());
    assert_eq!(branch("archive").2.text(), "▸");
    assert_eq!(visible_tables(), ["orders", "customers"]);
    assert_eq!(ui.table_list_items.borrow().len(), 4);
    // Explicit expansion while searching survives clearing the query.
    search("archive");
    find_schema_button(ui.objects.upcast_ref(), "Открыть базу archive")
        .unwrap()
        .emit_clicked();
    search("");
    assert!(branch("archive").1.is_visible());

    // MySQL/MariaDB use schema rows directly as database rows.
    clear_box(&ui.objects);
    ui.object_search_branches.borrow_mut().clear();
    ui.table_list_items.borrow_mut().clear();
    let mut mysql = profile.clone();
    mysql.kind = DatabaseKind::MySql;
    append_schema_tree(
        &ui,
        &ui.objects,
        &mysql,
        TableCatalog {
            schemas: vec!["sales".into(), "audit_db".into()],
            objects: vec![DbObject {
                schema: "audit_db".into(),
                name: "events".into(),
                kind: database::DbObjectKind::Table,
            }],
            truncated: false,
        },
    );
    apply_table_list_filter(&ui);
    assert!(!branch("audit_db").1.is_visible());
    search("events");
    assert!(branch("audit_db").1.is_visible());
    assert!(!branch("sales").0.is_visible());
    search("audit_db");
    assert_eq!(visible_tables(), ["events"]);
    search("");
    assert!(!branch("audit_db").1.is_visible());
    search("events");
    close_session(&ui, &profile_session_key(&profile));
    assert!(ui.objects.first_child().is_none());
    ui.table_filter.emit_by_name::<()>("search-changed", &[]);
    assert!(
        ui.objects.first_child().is_none(),
        "late search must not recreate a closed tree"
    );
    ui.window.close();
}
