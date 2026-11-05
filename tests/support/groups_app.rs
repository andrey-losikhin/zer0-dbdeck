//! Every workspace group belongs to a concrete database, including server browsing.
use super::*;
use grid_app_tests::until;

#[test]
#[ignore = "requires headless GTK and local Compose PostgreSQL"]
fn postgres_concrete_database_groups() {
    groups(
        DatabaseKind::PostgreSql,
        55432,
        "DBDECK_PG_PASSWORD",
        "postgres",
    );
}

#[test]
#[ignore = "requires headless GTK and local Compose MySQL"]
fn mysql_concrete_database_groups() {
    groups(
        DatabaseKind::MySql,
        53307,
        "DBDECK_MYSQL_PASSWORD",
        "dbdeck_analytics",
    );
}

#[test]
#[ignore = "requires headless GTK and local Compose MariaDB"]
fn mariadb_concrete_database_groups() {
    groups(
        DatabaseKind::MariaDb,
        53306,
        "DBDECK_MARIADB_PASSWORD",
        "dbdeck_analytics",
    );
}

fn groups(kind: DatabaseKind, port: u16, password_key: &str, second: &str) {
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
        "id":format!("groups-{port}"), "name":"server", "kind":kind, "host":"127.0.0.1", "port":port,
        "database":"", "username":"dbdeck_test", "environment":"local", "color":"#ffffff",
        "read_only":true, "tls_mode":"disabled"
    })).unwrap();
    let password = std::env::var(password_key).unwrap();
    let pool = runtime
        .block_on(DbPool::connect(&profile, &password))
        .unwrap();
    let app = adw::Application::builder()
        .application_id(format!("io.github.dbdeck.groups{port}"))
        .build();
    app.register(None::<&gtk::gio::Cancellable>).unwrap();
    let ui = build_window(&app, runtime.clone());
    {
        let mut store = ui.store.borrow_mut();
        let store = store.as_mut().unwrap();
        store
            .unlock_or_create_vault("synthetic groups test key")
            .unwrap();
        store.save_profile(profile.clone(), &password).unwrap();
    }
    let root_key = profile_session_key(&profile);
    ui.connect_requests
        .borrow_mut()
        .insert(root_key.clone(), "root".into());
    handle_event(
        &ui,
        Event::Connected("root".into(), profile.clone(), Ok(pool.clone())),
    );
    until(|| ui.objects_available.get());
    assert_eq!(ui.session_tabs.n_pages(), 0);
    assert!(!ui.session_tabs.is_visible());
    assert!(!ui.session_table_tabs.borrow().contains_key(&root_key));

    let mut first_profile = profile.clone();
    first_profile.database = "dbdeck_test".into();
    let first_key = profile_session_key(&first_profile);
    let object = DbObject {
        schema: if matches!(kind, DatabaseKind::PostgreSql) {
            "public".into()
        } else {
            "dbdeck_test".into()
        },
        name: "sample_records".into(),
        kind: database::DbObjectKind::Table,
    };
    if matches!(kind, DatabaseKind::PostgreSql) {
        with_database_session(&ui, first_profile, pool.clone(), move |ui, _| {
            open_table(ui, object, true)
        });
    } else {
        // A table selected from the server-wide catalog must open its own DB group.
        open_table(&ui, object, true);
    }
    until(|| !ui.table_data.borrow().is_empty());
    assert_eq!(ui.session_tabs.n_pages(), 1);
    assert_eq!(
        ui.current_profile.borrow().as_ref().unwrap().database,
        "dbdeck_test"
    );
    assert!(
        ui.table_profiles
            .borrow()
            .values()
            .all(|p| p.database == "dbdeck_test")
    );
    assert!(!ui.session_table_tabs.borrow().contains_key(&root_key));
    create_sql_tab(&ui, &first_key);
    let first_sql = ui
        .sql_tabs
        .borrow()
        .values()
        .find(|t| t.session_key == first_key)
        .unwrap()
        .buffer
        .clone();
    first_sql.set_text("SELECT 'first database draft';");
    let first_page = ui.session_table_tabs.borrow()[&first_key].current_page();

    open_database_session(&ui, profile.clone(), pool.clone(), second);
    until(|| {
        ui.current_profile
            .borrow()
            .as_ref()
            .is_some_and(|p| p.database == second)
    });
    let second_key = profile_session_key(ui.current_profile.borrow().as_ref().unwrap());
    create_sql_tab(&ui, &second_key);
    let second_sql = ui
        .sql_tabs
        .borrow()
        .values()
        .find(|t| t.session_key == second_key)
        .unwrap()
        .buffer
        .clone();
    second_sql.set_text("SELECT 'second database draft';");
    assert_eq!(ui.session_tabs.n_pages(), 2);
    for index in 0..ui.session_tabs.n_pages() {
        let page = ui.session_tabs.nth_page(Some(index)).unwrap();
        let key = page.tooltip_text().unwrap();
        assert!(!ui.sessions.borrow()[key.as_str()].0.database.is_empty());
    }
    assert_eq!(ui.session_table_tabs.borrow()[&first_key].n_pages(), 2);
    assert_eq!(ui.session_table_tabs.borrow()[&second_key].n_pages(), 1);
    ui.session_tabs
        .set_current_page(session_page_index(&ui, &first_key));
    assert_eq!(
        ui.current_profile.borrow().as_ref().unwrap().database,
        "dbdeck_test"
    );
    assert_eq!(
        ui.session_table_tabs.borrow()[&first_key].current_page(),
        first_page
    );
    assert_eq!(
        first_sql.text(&first_sql.start_iter(), &first_sql.end_iter(), true),
        "SELECT 'first database draft';"
    );
    grid_app_tests::layout_snapshot(&ui.window, &format!("target/database-groups-{port}.png"));

    close_session(&ui, &first_key);
    assert_eq!(ui.session_tabs.n_pages(), 1);
    assert_eq!(
        ui.current_profile.borrow().as_ref().unwrap().database,
        second
    );
    assert_eq!(
        second_sql.text(&second_sql.start_iter(), &second_sql.end_iter(), true),
        "SELECT 'second database draft';"
    );
    close_session(&ui, &second_key);
    assert_eq!(ui.session_tabs.n_pages(), 0);
    assert!(!ui.session_tabs.is_visible());
    assert_eq!(
        profile_session_key(ui.current_profile.borrow().as_ref().unwrap()),
        root_key
    );
    assert!(ui.sessions.borrow().contains_key(&root_key));
    assert!(!ui.session_table_tabs.borrow().contains_key(&root_key));
    let sql_count = ui.sql_tabs.borrow().len();
    let targets = runtime
        .block_on(DbPool::profile_database_targets(&profile, &password))
        .expect("load SQL database choices");
    assert!(targets.iter().any(|(name, _)| name == "dbdeck_test"));
    create_sql_tab(&ui, &root_key);
    assert_eq!(
        ui.sql_tabs.borrow().len(),
        sql_count,
        "SQL requires an explicit database"
    );
    export_app_tests::select_target_database("dbdeck_test");
    until(|| {
        ui.current_profile
            .borrow()
            .as_ref()
            .is_some_and(|p| p.database == "dbdeck_test")
            && ui
                .sql_tabs
                .borrow()
                .values()
                .any(|tab| tab.session_key == first_key)
    });
    assert_eq!(ui.session_tabs.n_pages(), 1);
    close_session(&ui, &root_key);
    assert_eq!(ui.session_tabs.n_pages(), 1);
    assert_eq!(
        ui.current_profile.borrow().as_ref().unwrap().database,
        "dbdeck_test"
    );
    println!(
        "{kind:?}: no server group/notebook; tables/SQL grouped by database; switching preserves drafts; closing active/last group and reopening work"
    );
    ui.window.close();
    runtime.block_on(async {
        match pool {
            DbPool::PostgreSql(p) => p.close().await,
            DbPool::MySql(p) => p.close().await,
        }
    });
}
