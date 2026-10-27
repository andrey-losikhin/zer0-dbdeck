use super::*;

pub(super) fn show_active(ui: &Rc<Ui>) {
    let Some(profile) = ui.current_profile.borrow().clone() else {
        return;
    };
    let session = profile_session_key(&profile);
    let key = ui.active_table_by_session.borrow().get(&session).cloned();
    let object = key.and_then(|key| ui.table_objects.borrow().get(&key).cloned());
    if let Some(object) = object {
        show(ui, &profile, &object);
    } else {
        ui.status.set_text(
            "Откройте таблицу или результат SELECT одной таблицы для просмотра структуры",
        );
    }
}

pub(super) fn show(ui: &Rc<Ui>, profile: &ConnectionProfile, object: &DbObject) {
    let session = profile_session_key(profile);
    let Some((_, pool)) = ui.sessions.borrow().get(&session).cloned() else {
        return;
    };
    let window = adw::Window::builder()
        .title(format!("Структура · {}.{}", object.schema, object.name))
        .transient_for(&ui.window)
        .modal(true)
        .default_width(920)
        .default_height(600)
        .build();
    window.set_widget_name("object-structure");
    add_escape_to_window(&window);
    let root = gtk::Box::new(gtk::Orientation::Vertical, 6);
    let header = adw::HeaderBar::new();
    header.set_title_widget(Some(&gtk::Label::new(Some(&format!(
        "{}.{}",
        object.schema, object.name
    )))));
    root.append(&header);
    let status = gtk::Label::new(Some("Читаю структуру…"));
    status.set_xalign(0.0);
    status.set_wrap(true);
    status.set_margin_start(8);
    status.set_margin_end(8);
    root.append(&status);
    let notebook = gtk::Notebook::new();
    notebook.set_widget_name("structure-tabs");
    notebook.set_vexpand(true);
    let mut grids = Vec::new();
    for title in ["Столбцы", "Ограничения и связи", "Индексы"] {
        let grid = TabulatorGrid::new(|_| {});
        grid.set_theme(&theme::grid_css(ui.active_theme.get()));
        notebook.append_page(&grid.webview, Some(&gtk::Label::new(Some(title))));
        grids.push(grid);
    }
    let ddl_buffer = sourceview5::Buffer::new(None);
    ddl_buffer.set_language(sourceview5::LanguageManager::new().language("sql").as_ref());
    apply_sql_style_scheme(
        &ddl_buffer,
        &theme::sql_style_schemes(),
        ui.active_theme.get(),
        adw::StyleManager::default().is_dark(),
    );
    let ddl = sourceview5::View::with_buffer(&ddl_buffer);
    ddl.set_widget_name("structure-ddl");
    ddl.set_editable(false);
    ddl.set_monospace(true);
    ddl.set_show_line_numbers(true);
    ddl.set_left_margin(8);
    let ddl_scroll = gtk::ScrolledWindow::builder()
        .child(&ddl)
        .hexpand(true)
        .vexpand(true)
        .build();
    notebook.append_page(&ddl_scroll, Some(&gtk::Label::new(Some("DDL"))));
    root.append(&notebook);
    let actions = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    actions.set_halign(gtk::Align::End);
    actions.set_margin_end(8);
    actions.set_margin_bottom(8);
    let copy = gtk::Button::with_label("Копировать DDL");
    copy.set_widget_name("structure-copy-ddl");
    copy.set_tooltip_text(Some("Копировать DDL"));
    copy.set_sensitive(false);
    let open = gtk::Button::with_label("Открыть DDL в SQL");
    open.set_widget_name("structure-open-sql");
    open.set_tooltip_text(Some(
        "Открыть DDL для редактирования; автоматически не выполняется",
    ));
    open.set_sensitive(false);
    let close = gtk::Button::with_label("Закрыть");
    for button in [&copy, &open, &close] {
        actions.append(button);
    }
    root.append(&actions);
    window.set_content(Some(&root));
    shortcuts::button(&window, &copy, "<Control><Shift>c");
    shortcuts::button(&window, &open, "<Control>Return");
    shortcuts::button(&window, &close, "<Control>w");
    let keys = gtk::EventControllerKey::new();
    let tabs = notebook.clone();
    keys.connect_key_pressed(move |_, key, _, modifiers| {
        if modifiers == gtk::gdk::ModifierType::CONTROL_MASK {
            let index = match key {
                gtk::gdk::Key::_1 => Some(0),
                gtk::gdk::Key::_2 => Some(1),
                gtk::gdk::Key::_3 => Some(2),
                gtk::gdk::Key::_4 => Some(3),
                _ => None,
            };
            if let Some(index) = index {
                tabs.set_current_page(Some(index));
                tabs.child_focus(gtk::DirectionType::TabForward);
                return gtk::glib::Propagation::Stop;
            }
        }
        gtk::glib::Propagation::Proceed
    });
    window.add_controller(keys);
    let weak = window.downgrade();
    close.connect_clicked(move |_| {
        if let Some(window) = weak.upgrade() {
            window.close();
        }
    });
    let buffer = ddl_buffer.clone();
    copy.connect_clicked(move |button| {
        button
            .clipboard()
            .set_text(&buffer.text(&buffer.start_iter(), &buffer.end_iter(), true))
    });
    let weak_ui = Rc::downgrade(ui);
    let weak = window.downgrade();
    let buffer = ddl_buffer.clone();
    open.connect_clicked(move |_| {
        if let Some(ui) = weak_ui.upgrade() {
            let before = ui.sql_tabs.borrow().len();
            create_sql_tab(&ui, &session);
            if ui.sql_tabs.borrow().len() == before {
                return;
            }
            let id = ui
                .active_table_by_session
                .borrow()
                .get(&session)
                .and_then(|key| key.strip_prefix("sql::").map(str::to_owned));
            if let Some(id) = id {
                if let Some(tab) = ui.sql_tabs.borrow().get(&id) {
                    tab.buffer.set_text(&buffer.text(
                        &buffer.start_iter(),
                        &buffer.end_iter(),
                        true,
                    ));
                    tab.editor.grab_focus();
                }
            }
        }
        if let Some(window) = weak.upgrade() {
            window.close();
        }
    });
    let (sender, receiver) = mpsc::channel();
    let object = object.clone();
    ui.runtime.spawn(async move {
        let _ = sender.send(pool.inspect_object(&object).await);
    });
    let weak = window.downgrade();
    gtk::glib::timeout_add_local(Duration::from_millis(30), move || {
        let Some(window) = weak.upgrade() else {
            return gtk::glib::ControlFlow::Break;
        };
        if !window.is_visible() {
            return gtk::glib::ControlFlow::Break;
        }
        match receiver.try_recv() {
            Ok(Ok(structure)) => {
                for (grid, data) in grids.iter().zip([
                    &structure.columns,
                    &structure.constraints,
                    &structure.indexes,
                ]) {
                    grid.set_data(
                        &data.columns,
                        &grid_rows(data),
                        false,
                        false,
                        false,
                        &[],
                        &[],
                    );
                }
                ddl_buffer.set_text(&structure.ddl);
                status.set_text(&structure.ddl_note);
                copy.set_sensitive(true);
                open.set_sensitive(structure.ddl_can_open);
                if !structure.ddl_can_open {
                    open.set_tooltip_text(Some(&structure.ddl_note));
                }
                gtk::glib::ControlFlow::Break
            }
            Ok(Err(error)) => {
                status.set_text(&format!("Не удалось прочитать структуру: {error}"));
                gtk::glib::ControlFlow::Break
            }
            Err(mpsc::TryRecvError::Empty) => gtk::glib::ControlFlow::Continue,
            Err(mpsc::TryRecvError::Disconnected) => {
                status.set_text("Чтение структуры прервано");
                gtk::glib::ControlFlow::Break
            }
        }
    });
    window.present();
}
