use super::*;

pub(super) fn show_active(ui: &Rc<Ui>) {
    let Some(profile) = ui.current_profile.borrow().clone() else {
        return;
    };
    let session = profile_session_key(&profile);
    let key = ui
        .active_table_by_session
        .borrow()
        .get(&workspace_ui::active(ui, &session))
        .cloned();
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
    let status_row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    let spinner = adw::Spinner::new();
    spinner.set_size_request(16, 16);
    spinner.set_margin_start(8);
    status_row.append(&spinner);
    status_row.append(&dialogs::copyable_label(&status));
    root.append(&status_row);
    let notebook = gtk::Notebook::new();
    notebook.set_widget_name("structure-tabs");
    notebook.set_vexpand(true);
    let mut grids = Vec::new();
    for title in ["Столбцы", "Ограничения и связи", "Индексы"] {
        let grid = TabulatorGrid::new(false, |_| {});
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
    let compare = gtk::Button::with_label("Сравнить структуру…");
    compare.set_widget_name("structure-compare");
    compare.set_visible(object.kind == database::DbObjectKind::Table);
    compare.set_sensitive(false);
    let close = gtk::Button::with_label("Закрыть");
    for button in [&compare, &copy, &open, &close] {
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
    {
        let (weak_ui, weak) = (weak_ui.clone(), weak.clone());
        let (profile, object) = (profile.clone(), object.clone());
        compare.connect_clicked(move |_| {
            if let (Some(ui), Some(parent)) = (weak_ui.upgrade(), weak.upgrade()) {
                show_comparison(&ui, &parent, &profile, &object);
            }
        });
    }
    let buffer = ddl_buffer.clone();
    let schema = object.schema.clone();
    open.connect_clicked(move |_| {
        if let Some(ui) = weak_ui.upgrade() {
            let group = workspace_ui::ensure(&ui, &session, Some(&schema));
            workspace_ui::select(&ui, &group);
            let before = ui.sql_tabs.borrow().len();
            create_sql_tab_in_group(&ui, &group);
            if ui.sql_tabs.borrow().len() == before {
                return;
            }
            let id = ui
                .active_table_by_session
                .borrow()
                .get(&group)
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
    let guard = ui.activity.start();
    ui.runtime.spawn(async move {
        let _guard = guard;
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
        let received = receiver.try_recv();
        if !matches!(received, Err(mpsc::TryRecvError::Empty)) {
            spinner.set_visible(false);
        }
        match received {
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
                compare.set_sensitive(structure.ddl_can_open);
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

fn connection_caption(profile: &ConnectionProfile) -> String {
    let database = if profile.database.is_empty() {
        "по умолчанию"
    } else {
        &profile.database
    };
    format!(
        "{} · {} · {}@{}:{} / {}",
        profile.name,
        profile.environment.label(),
        profile.username,
        profile.host,
        profile.port,
        database
    )
}

const NO_STRUCTURE_DIFFERENCES: &str = "Различий в доступных метаданных не найдено";

fn show_comparison(
    ui: &Rc<Ui>,
    parent: &adw::Window,
    source: &ConnectionProfile,
    object: &DbObject,
) {
    let source_key = profile_session_key(source);
    let mut candidates: Vec<_> = ui
        .sessions
        .borrow()
        .iter()
        .filter(|(_, (profile, _))| {
            std::mem::discriminant(&source.kind) == std::mem::discriminant(&profile.kind)
                && (!matches!(profile.kind, DatabaseKind::PostgreSql)
                    || !profile.database.trim().is_empty())
        })
        .map(|(key, (profile, _))| (connection_caption(profile), key.clone()))
        .collect();
    candidates.sort_unstable();
    let initial_connection = candidates
        .iter()
        .position(|(_, key)| *key == source_key)
        .or_else(|| (!candidates.is_empty()).then_some(0));

    let window = adw::Window::builder()
        .title("Сравнение структуры")
        .transient_for(parent)
        .modal(true)
        .default_width(820)
        .default_height(560)
        .build();
    window.set_widget_name("structure-comparison");
    add_escape_to_window(&window);
    let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
    let header = adw::HeaderBar::new();
    let copy = gtk::Button::with_label("Копировать Markdown");
    copy.set_widget_name("structure-comparison-copy-markdown");
    copy.set_sensitive(false);
    header.pack_start(&copy);
    root.append(&header);
    let content = gtk::Box::new(gtk::Orientation::Vertical, 8);
    content.set_margin_start(12);
    content.set_margin_end(12);
    content.set_margin_top(10);
    content.set_margin_bottom(12);
    let source_label = gtk::Label::new(Some(&format!(
        "Исходная таблица: {}.{} · {}",
        object.schema,
        object.name,
        connection_caption(source)
    )));
    source_label.set_wrap(true);
    source_label.set_xalign(0.0);
    content.append(&source_label);

    let controls = gtk::Grid::builder()
        .column_spacing(8)
        .row_spacing(6)
        .build();
    let captions: Vec<_> = candidates
        .iter()
        .map(|(caption, _)| caption.as_str())
        .collect();
    let connection = gtk::DropDown::from_strings(&captions);
    connection.set_widget_name("structure-comparison-connection");
    connection.set_selected(gtk::INVALID_LIST_POSITION);
    let table = gtk::DropDown::from_strings(&[]);
    table.set_widget_name("structure-comparison-table");
    table.set_sensitive(false);
    for (row, label, dropdown) in [(0, "_Подключение", &connection), (1, "_Таблица", &table)]
    {
        let label = gtk::Label::with_mnemonic(label);
        label.set_xalign(0.0);
        label.set_mnemonic_widget(Some(dropdown));
        dropdown.set_hexpand(true);
        dropdown.set_enable_search(true);
        controls.attach(&label, 0, row, 1, 1);
        controls.attach(dropdown, 1, row, 1, 1);
    }
    let compare = gtk::Button::with_label("Сравнить");
    compare.set_widget_name("structure-comparison-run");
    compare.set_sensitive(false);
    controls.attach(&compare, 2, 1, 1, 1);
    content.append(&controls);

    let status = gtk::Label::new(Some(if candidates.is_empty() {
        "Откройте подключение к базе того же движка"
    } else {
        "Выберите подключение и таблицу для сравнения"
    }));
    status.set_widget_name("structure-comparison-status");
    status.set_xalign(0.0);
    status.set_wrap(true);
    content.append(&dialogs::copyable_label(&status));

    let result = gtk::TextView::builder()
        .editable(false)
        .cursor_visible(false)
        .monospace(true)
        .wrap_mode(gtk::WrapMode::WordChar)
        .left_margin(8)
        .right_margin(8)
        .top_margin(8)
        .build();
    result.set_widget_name("structure-comparison-result");
    let buffer = result.buffer();
    content.append(
        &gtk::ScrolledWindow::builder()
            .child(&result)
            .vexpand(true)
            .build(),
    );
    root.append(&content);
    window.set_content(Some(&root));

    let report = Rc::new(RefCell::new(None::<String>));
    let copy_report = report.clone();
    copy.connect_clicked(move |button| {
        if let Some(markdown) = copy_report.borrow().as_deref() {
            button.clipboard().set_text(markdown);
        }
    });
    let clear_result = {
        let (buffer, report, copy) = (buffer.clone(), report.clone(), copy.downgrade());
        move || {
            report.replace(None);
            if let Some(copy) = copy.upgrade() {
                copy.set_sensitive(false);
            }
            buffer.set_text("");
        }
    };

    let candidates = Rc::new(candidates);
    let tables = Rc::new(RefCell::new(Vec::<DbObject>::new()));
    let generation = Rc::new(Cell::new(0u64));
    let weak_ui = Rc::downgrade(ui);
    let (weak_table, weak_status) = (table.downgrade(), status.downgrade());
    let (catalog_candidates, catalog_tables, catalog_generation) =
        (candidates.clone(), tables.clone(), generation.clone());
    let catalog_ui = weak_ui.clone();
    let include_system = show_system_schemas(ui);
    let clear_catalog_result = clear_result.clone();
    connection.connect_selected_notify(move |connection| {
        clear_catalog_result();
        let (Some(ui), Some(table), Some(status)) = (
            catalog_ui.upgrade(),
            weak_table.upgrade(),
            weak_status.upgrade(),
        ) else {
            return;
        };
        let revision = catalog_generation.get().wrapping_add(1);
        catalog_generation.set(revision);
        catalog_tables.borrow_mut().clear();
        table.set_model(None::<&gtk::StringList>);
        table.set_sensitive(false);
        let Some((_, key)) = catalog_candidates.get(connection.selected() as usize) else {
            return;
        };
        let Some((profile, pool)) = ui.sessions.borrow().get(key).cloned() else {
            status.set_text("Подключение для сравнения закрыто");
            return;
        };
        status.set_text("Загружаю список таблиц…");
        let guard = ui.activity.start();
        let worker = ui.runtime.spawn(async move {
            let _guard = guard;
            tokio::time::timeout(
                Duration::from_secs(15),
                pool.list_tables_with_system(&profile, include_system),
            )
            .await
            .map_err(|_| "истекло время ожидания".to_owned())?
            .map_err(|error| error.to_string())
        });
        let (table, status) = (weak_table.clone(), weak_status.clone());
        let (generation, tables) = (catalog_generation.clone(), catalog_tables.clone());
        gtk::glib::MainContext::default().spawn_local(async move {
            let catalog = worker.await;
            let (Some(table), Some(status)) = (table.upgrade(), status.upgrade()) else {
                return;
            };
            if generation.get() != revision {
                return;
            }
            match catalog {
                Ok(Ok(catalog)) => {
                    let models = gtk::StringList::new(&["Выберите таблицу…"]);
                    let mut found = Vec::new();
                    for object in catalog
                        .objects
                        .into_iter()
                        .filter(|object| object.kind == database::DbObjectKind::Table)
                    {
                        models.append(&format!("{}.{}", object.schema, object.name));
                        found.push(object);
                    }
                    let has_tables = !found.is_empty();
                    tables.replace(found);
                    table.set_model(Some(&models));
                    table.set_selected(0);
                    table.set_sensitive(has_tables);
                    status.set_text(match (has_tables, catalog.truncated) {
                        (true, false) => "Выберите таблицу для сравнения",
                        (true, true) => "Каталог показан частично. Выберите таблицу из списка",
                        (false, false) => "В подключении нет доступных таблиц",
                        (false, true) => "В показанной части каталога нет таблиц",
                    });
                }
                Ok(Err(error)) => status.set_text(&format!(
                    "Не удалось загрузить таблицы для сравнения: {error}"
                )),
                Err(_) => status.set_text("Загрузка таблиц для сравнения прервана"),
            }
        });
    });

    let weak_run = compare.downgrade();
    let selection_status = status.downgrade();
    let clear_selection_result = clear_result.clone();
    table.connect_selected_notify(move |table| {
        clear_selection_result();
        let selected = table.selected();
        if let Some(run) = weak_run.upgrade() {
            run.set_sensitive(!matches!(selected, 0 | gtk::INVALID_LIST_POSITION));
        }
        if selected != gtk::INVALID_LIST_POSITION
            && let Some(status) = selection_status.upgrade()
        {
            status.set_text(if selected == 0 {
                "Выберите таблицу для сравнения"
            } else {
                "Нажмите «Сравнить», чтобы прочитать метаданные"
            });
        }
    });

    let (weak_connection, weak_table, weak_status) = (
        connection.downgrade(),
        table.downgrade(),
        status.downgrade(),
    );
    let copy = copy.downgrade();
    let source_caption = comparison_table_caption(object, &connection_caption(source));
    let source = object.clone();
    compare.connect_clicked(move |run| {
        if !run.is_sensitive() {
            return;
        }
        clear_result();
        let (Some(ui), Some(connection), Some(table), Some(status)) = (
            weak_ui.upgrade(),
            weak_connection.upgrade(),
            weak_table.upgrade(),
            weak_status.upgrade(),
        ) else {
            return;
        };
        let Some((target_connection, key)) = candidates.get(connection.selected() as usize) else {
            return;
        };
        let Some(target) = table
            .selected()
            .checked_sub(1)
            .and_then(|index| tables.borrow().get(index as usize).cloned())
        else {
            return;
        };
        let sessions = ui.sessions.borrow();
        let Some((_, source_pool)) = sessions.get(&source_key).cloned() else {
            status.set_text("Исходное подключение закрыто");
            return;
        };
        let Some((_, target_pool)) = sessions.get(key).cloned() else {
            status.set_text("Подключение для сравнения закрыто");
            return;
        };
        let target_caption = comparison_table_caption(&target, target_connection);
        connection.set_sensitive(false);
        table.set_sensitive(false);
        run.set_sensitive(false);
        status.set_text("Сравниваю структуру…");
        let (source, guard) = (source.clone(), ui.activity.start());
        let worker = ui.runtime.spawn(async move {
            let _guard = guard;
            source_pool
                .compare_table_structure(&source, &target_pool, &target)
                .await
        });
        let (connection, table, status, run) = (
            weak_connection.clone(),
            weak_table.clone(),
            weak_status.clone(),
            run.downgrade(),
        );
        let (buffer, report, copy) = (buffer.clone(), report.clone(), copy.clone());
        let source_caption = source_caption.clone();
        gtk::glib::MainContext::default().spawn_local(async move {
            let result = worker.await;
            let (Some(connection), Some(table), Some(status), Some(run)) = (
                connection.upgrade(),
                table.upgrade(),
                status.upgrade(),
                run.upgrade(),
            ) else {
                return;
            };
            connection.set_sensitive(true);
            table.set_sensitive(true);
            run.set_sensitive(true);
            match result {
                Ok(Ok(found)) => {
                    let summary = if found.is_empty() {
                        NO_STRUCTURE_DIFFERENCES.to_owned()
                    } else {
                        format!("Различий: {}", found.len())
                    };
                    let differences = format_differences(&found);
                    status.set_text(&summary);
                    buffer.set_text(&differences);
                    report.replace(Some(comparison_markdown(
                        &source_caption,
                        &target_caption,
                        &summary,
                        &differences,
                    )));
                    if let Some(copy) = copy.upgrade() {
                        copy.set_sensitive(true);
                    }
                }
                Ok(Err(error)) => status.set_text(&error),
                Err(_) => status.set_text("Сравнение прервано"),
            }
        });
    });
    window.present();
    if let Some(index) = initial_connection {
        connection.set_selected(index as u32);
    }
}

fn comparison_table_caption(object: &DbObject, connection: &str) -> String {
    format!("{:?}.{:?} · {connection:?}", object.schema, object.name)
}

fn comparison_markdown(source: &str, target: &str, summary: &str, differences: &str) -> String {
    use std::io::Write as _;

    let mut report = "# Сравнение структуры таблиц\n\n".as_bytes().to_vec();
    for (heading, caption) in [
        ("Исходная таблица", source),
        ("Таблица для сравнения", target),
    ] {
        writeln!(report, "## {heading}\n").expect("writing to Vec cannot fail");
        markdown_export::fenced(&mut report, "text", caption).expect("writing to Vec cannot fail");
    }
    writeln!(report, "## Результат\n\n{summary}\n").expect("writing to Vec cannot fail");
    if !differences.is_empty() {
        markdown_export::fenced(&mut report, "text", differences.trim_end_matches('\n'))
            .expect("writing to Vec cannot fail");
    }
    String::from_utf8(report).expect("Markdown report contains only UTF-8 text")
}

fn format_differences(found: &[crate::database_structure::StructureDifference]) -> String {
    use std::fmt::Write as _;

    let mut output = String::new();
    for difference in found {
        let change = match (&difference.before, &difference.after) {
            (None, _) => "Добавлено",
            (_, None) => "Удалено",
            _ => "Изменено",
        };
        let _ = writeln!(
            output,
            "{change} · {} {:?}",
            difference.category, difference.name
        );
        for (side, definition) in [
            ("Исходная", &difference.before),
            ("Для сравнения", &difference.after),
        ] {
            if let Some(definition) = definition {
                let _ = writeln!(output, "{side}\n{definition}");
            }
        }
        output.push('\n');
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::database_structure::StructureDifference;

    #[test]
    fn comparison_markdown_keeps_catalog_text_inside_fences() {
        let object = DbObject {
            schema: "a.b".into(),
            name: "table\n````<b>".into(),
            kind: database::DbObjectKind::Table,
        };
        let caption = comparison_table_caption(&object, "[local](https://example.test)");
        let differences = format_differences(&[StructureDifference {
            category: "Столбец",
            name: "odd | name".into(),
            before: Some("text\n```\n# заголовок".into()),
            after: Some("varchar(30)\n<em>текст</em>".into()),
        }]);
        let report = comparison_markdown(&caption, "target", "Различий: 1", &differences);
        assert!(report.contains(concat!(
            "## Исходная таблица\n\n",
            "`````text\n\"a.b\".\"table\\n````<b>\" · \"[local](https://example.test)\"\n`````\n"
        )));
        assert!(report.contains(concat!(
            "## Результат\n\nРазличий: 1\n\n",
            "````text\nИзменено · Столбец \"odd | name\"\n",
            "Исходная\ntext\n```\n# заголовок\n",
            "Для сравнения\nvarchar(30)\n<em>текст</em>\n````\n"
        )));
    }
}
