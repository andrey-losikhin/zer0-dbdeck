//! Check real clipboard contents, dynamic messages and error-dialog dismissal.
use super::*;
use grid_app_tests::until;

#[test]
#[ignore = "requires headless GTK and local PostgreSQL at 127.0.0.1:55432"]
fn application_postgres_idle_recovery_messages() {
    use sql_session::postgres_idle_eof::{Proxy, mark_idle, session};

    gtk::init().unwrap();
    adw::init().unwrap();
    let runtime = Arc::new(tokio::runtime::Runtime::new().unwrap());
    let profile: ConnectionProfile = serde_json::from_value(serde_json::json!({
        "id":"idle-ui", "name":"Idle SQL", "kind":"postgre_sql", "host":"127.0.0.1",
        "port":55432, "database":"dbdeck_test", "username":"dbdeck_test",
        "environment":"local", "color":"#ffffff", "read_only":false, "tls_mode":"disabled"
    }))
    .unwrap();
    let password = std::env::var("DBDECK_PG_PASSWORD").unwrap();
    let pool = runtime
        .block_on(DbPool::connect(&profile, &password))
        .unwrap();
    let app = adw::Application::builder()
        .application_id("io.github.dbdeck.idle-test")
        .build();
    app.register(None::<&gtk::gio::Cancellable>).unwrap();
    let ui = build_window(&app, runtime.clone());
    let session_key = profile_session_key(&profile);
    ui.sessions
        .borrow_mut()
        .insert(session_key.clone(), (profile.clone(), pool));
    *ui.current_profile.borrow_mut() = Some(profile);
    create_sql_tab(&ui, &session_key);
    let id = ui.sql_tabs.borrow().keys().next().unwrap().clone();

    for open_transaction in [false, true] {
        let mut proxy = runtime.block_on(Proxy::start(false));
        let session = {
            let _runtime = runtime.enter();
            session(proxy.port)
        };
        ui.sql_tabs.borrow_mut().get_mut(&id).unwrap().session = session.clone();
        let run = |sql| {
            ui.sql_tabs.borrow()[&id].buffer.set_text(sql);
            run_sql_tab(&ui, &id);
            until(|| ui.sql_tabs.borrow()[&id].run_button.is_sensitive());
        };
        run(if open_transaction {
            "BEGIN; SELECT 1"
        } else {
            "SELECT 1"
        });
        assert_eq!(session.has_pending_transaction(), open_transaction);
        assert!(!ui.sql_tabs.borrow()[&id].error_message.is_visible());
        runtime.block_on(async {
            mark_idle(&session).await;
            proxy.cut().await;
        });
        run("SELECT 2");
        let tabs = ui.sql_tabs.borrow();
        let tab = &tabs[&id];
        if open_transaction {
            assert!(tab.error_message.is_visible());
            let message = tab.error_message.text();
            assert!(message.contains("Открытая транзакция потеряна"));
            assert!(message.contains("SQL не отправлен"));
            assert!(tab.result.is_none());
        } else {
            assert!(!tab.error_message.is_visible());
            assert!(tab.status.text().starts_with("Открыта новая SQL-сессия"));
            assert_eq!(tab.result.as_ref().unwrap().rows.len(), 1);
        }
    }
    ui.window.close();
}

fn find(widget: &gtk::Widget, class: &str) -> Option<gtk::Widget> {
    if widget.has_css_class(class) {
        return Some(widget.clone());
    }
    let mut child = widget.first_child();
    while let Some(widget) = child {
        child = widget.next_sibling();
        if let Some(found) = find(&widget, class) {
            return Some(found);
        }
    }
    None
}

fn copied(button: &gtk::Button, expected: &str) {
    button.emit_clicked();
    let result = Rc::new(RefCell::new(None));
    let output = result.clone();
    button
        .clipboard()
        .read_text_async(None::<&gtk::gio::Cancellable>, move |text| {
            *output.borrow_mut() = Some(text.unwrap().unwrap().to_string());
        });
    until(|| result.borrow().is_some());
    assert_eq!(result.borrow().as_deref(), Some(expected));
}

fn check_label(label: &gtk::Label) {
    assert!(label.is_selectable());
    until(|| label.is_mapped());
    label.select_region(0, -1);
    assert_eq!(
        label.selection_bounds(),
        Some((0, label.text().chars().count() as i32))
    );
    let row = label.parent().unwrap();
    assert!(row.has_css_class("copyable-message"));
    let button = row.last_child().unwrap().downcast::<gtk::Button>().unwrap();
    assert!(button.is_visible());
    copied(&button, &label.text());
}

#[test]
#[ignore = "requires headless GTK and local Compose PostgreSQL"]
fn copyable_errors_and_status_messages() {
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
        "id":"messages-ui", "name":"messages UI", "kind":"postgre_sql", "host":"127.0.0.1", "port":55432,
        "database":"dbdeck_test", "username":"dbdeck_test", "environment":"local",
        "color":"#ffffff", "read_only":true, "tls_mode":"disabled"
    })).unwrap();
    let password = std::env::var("DBDECK_PG_PASSWORD").unwrap();
    let pool = runtime
        .block_on(DbPool::connect(&profile, &password))
        .unwrap();
    let app = adw::Application::builder()
        .application_id("io.github.dbdeck.messages-test")
        .build();
    app.register(None::<&gtk::gio::Cancellable>).unwrap();
    let ui = build_window(&app, runtime.clone());
    {
        let mut store = ui.store.borrow_mut();
        let store = store.as_mut().unwrap();
        store
            .unlock_or_create_vault("synthetic messages test key")
            .unwrap();
        store.save_profile(profile.clone(), &password).unwrap();
    }
    let session_key = profile_session_key(&profile);
    ui.sessions
        .borrow_mut()
        .insert(session_key.clone(), (profile.clone(), pool.clone()));
    *ui.current_profile.borrow_mut() = Some(profile.clone());
    create_sql_tab(&ui, &session_key);
    let id = ui.sql_tabs.borrow().keys().next().unwrap().clone();
    let buffer = ui.sql_tabs.borrow()[&id].buffer.clone();
    buffer.set_text("SELECT FROM;");
    let syntax = find(ui.window.upcast_ref(), "sql-syntax-diagnostic")
        .unwrap()
        .downcast::<gtk::Label>()
        .unwrap();
    until(|| syntax.is_visible());
    assert!(syntax.wraps() && syntax.ellipsize() == gtk::pango::EllipsizeMode::None);
    check_label(&syntax);
    run_sql_tab(&ui, &id);
    until(|| ui.sql_tabs.borrow()[&id].error_message.is_visible());
    let error = ui.sql_tabs.borrow()[&id].error_message.clone();
    assert!(error.text().contains("syntax error"));
    check_label(&error);
    grid_app_tests::layout_snapshot(&ui.window, "target/copyable-sql-errors.png");
    buffer.set_text("SELECT 1;");
    until(|| !syntax.is_visible());
    assert!(
        !syntax.parent().unwrap().is_visible(),
        "hidden diagnostics left an empty copy row"
    );
    run_sql_tab(&ui, &id);
    until(|| ui.sql_tabs.borrow()[&id].run_button.is_sensitive());
    assert!(!error.parent().unwrap().is_visible());

    // The button reads the latest complete text, even when the footer is ellipsized.
    let long = format!(
        "Ошибка: {}\nВторая строка: <tag> & |",
        "длинное сообщение ".repeat(100)
    );
    ui.status.set_text(&long);
    check_label(&ui.status);
    ui.status.set_text("Следующая ошибка");
    check_label(&ui.status);
    let state = create_table_edit_state();
    state
        .status
        .set_text("Ошибка проверки/сохранения: запись изменена другим пользователем");
    let table_window = gtk::Window::builder()
        .transient_for(&ui.window)
        .child(&state.toolbar)
        .build();
    state.toolbar.set_visible(true);
    table_window.present();
    check_label(&state.status);
    table_window.close();

    ui.profile_status.borrow_mut().insert(
        profile.id.clone(),
        ProfileStatus::Error("Отказ в подключении".into()),
    );
    refresh_profiles(&ui);
    let connection = find(ui.profiles.upcast_ref(), "connection-status")
        .unwrap()
        .downcast::<gtk::Label>()
        .unwrap();
    check_label(&connection);
    show_connection_form(&ui, Some(profile));
    set_feedback(
        &ui,
        "Ошибка: неверный пароль\nПодробности сервера",
        "status-error",
    );
    let feedback = ui.test_feedback.borrow().as_ref().unwrap().clone();
    check_label(&feedback);
    feedback
        .root()
        .unwrap()
        .downcast::<gtk::Window>()
        .unwrap()
        .close();

    let text = "Ошибка сервера\nUnicode: Привет 🌌\nSELECT 'a|b';";
    show_error_dialog(&ui, "Ошибка проверки копирования", text);
    let dialog = gtk::Window::list_toplevels()
        .into_iter()
        .filter_map(|w| w.downcast::<gtk::MessageDialog>().ok())
        .find(|d| d.text().as_deref() == Some("Ошибка проверки копирования"))
        .unwrap();
    until(|| dialog.is_mapped());
    dialogs::install(&dialog); // Reinstall must not duplicate copy buttons.
    let copy = find(&dialog.message_area(), "copy-message-button")
        .unwrap()
        .downcast::<gtk::Button>()
        .unwrap();
    copied(&copy, &format!("Ошибка проверки копирования\n\n{text}"));
    assert!(dialog.is_visible(), "copying closed the dialog");
    fn labels_selectable(widget: &gtk::Widget) {
        if let Some(label) = widget.downcast_ref::<gtk::Label>() {
            assert!(label.is_selectable());
        }
        let mut child = widget.first_child();
        while let Some(widget) = child {
            child = widget.next_sibling();
            if !widget.has_css_class("copy-message-button") {
                labels_selectable(&widget);
            }
        }
    }
    labels_selectable(&dialog.message_area());
    dialog.response(gtk::ResponseType::Close);

    let confirmation = gtk::MessageDialog::builder()
        .transient_for(&ui.window)
        .text("Подтверждение")
        .secondary_text("Не выполнять действие при копировании")
        .buttons(gtk::ButtonsType::YesNo)
        .build();
    let responded = Rc::new(Cell::new(false));
    let result = responded.clone();
    confirmation.connect_response(move |_, _| result.set(true));
    dialogs::install(&confirmation);
    confirmation.present();
    until(|| confirmation.is_mapped());
    let copy = find(&confirmation.message_area(), "copy-message-button")
        .unwrap()
        .downcast::<gtk::Button>()
        .unwrap();
    copied(
        &copy,
        "Подтверждение\n\nНе выполнять действие при копировании",
    );
    assert!(!responded.get());
    confirmation.close();
    println!(
        "Selectable syntax/SQL/profile/form/table/footer errors; full current Unicode text copied; dialog copying does not close/accept; hidden rows disappear: passed"
    );
    ui.window.close();
    runtime.block_on(async {
        match pool {
            DbPool::PostgreSql(p) => p.close().await,
            DbPool::MySql(p) => p.close().await,
        }
    });
}
