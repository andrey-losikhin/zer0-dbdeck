use super::*;
#[derive(Clone)]
struct Ready {
    object: DbObject,
    options: csv_input::Options,
    selectors: Vec<gtk::DropDown>,
    columns: Vec<String>,
    generation: u64,
}

pub(super) fn choose(ui: &Rc<Ui>, profile: &ConnectionProfile, object: Option<DbObject>) {
    if profile.read_only {
        show_error_dialog(
            ui,
            "Импорт запрещён",
            "Подключение доступно только для чтения.",
        );
        return;
    }
    if let Some(object) = object {
        let profile = profile.clone();
        let owner = ui.clone();
        ensure_vault_unlocked(ui, move || file(&owner, profile, Some(object)));
    } else {
        database_target_ui::choose(ui, profile, |ui, profile| file(ui, profile, None));
    }
}
fn file(ui: &Rc<Ui>, profile: ConnectionProfile, object: Option<DbObject>) {
    let chooser = dialogs::file_chooser(
        Some(&format!("Импорт CSV · {}", profile.database)),
        Some(&ui.window),
        gtk::FileChooserAction::Open,
        Some("Открыть"),
        Some("Отмена"),
    );
    let filter = gtk::FileFilter::new();
    filter.set_name(Some("CSV / TSV"));
    for pattern in ["*.csv", "*.CSV", "*.tsv"] {
        filter.add_pattern(pattern);
    }
    chooser.add_filter(&filter);
    let owner = Rc::downgrade(ui);
    chooser.connect_response(move |dialog, response| {
        let path = dialog.file().and_then(|f| f.path());
        dialog.destroy();
        if response == gtk::ResponseType::Accept {
            if let (Some(ui), Some(path)) = (owner.upgrade(), path) {
                show(&ui, profile.clone(), object.clone(), path);
            }
        }
    });
    chooser.show();
}
fn show(ui: &Rc<Ui>, profile: ConnectionProfile, object: Option<DbObject>, path: PathBuf) {
    let secret = match database_target_ui::password(ui, &profile) {
        Ok(s) => s,
        Err(e) => {
            show_error_dialog(ui, "Импорт CSV", &e);
            return;
        }
    };
    let window = adw::Window::builder()
        .title("Импорт CSV")
        .transient_for(&ui.window)
        .default_width(840)
        .default_height(640)
        .build();
    window.set_widget_name("csv-import-wizard");
    add_escape_to_window(&window);
    let root = gtk::Box::new(gtk::Orientation::Vertical, 6);
    root.append(&adw::HeaderBar::new());
    root.set_margin_start(10);
    root.set_margin_end(10);
    root.set_margin_bottom(8);
    let title = gtk::Label::new(Some(&format!(
        "{} · {}:{} · {}\n{}",
        profile.name,
        profile.host,
        profile.port,
        profile.database,
        path.display()
    )));
    title.set_wrap(true);
    root.append(&title);
    let settings = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    let tables = gtk::DropDown::from_strings(&["Загрузка таблиц…"]);
    tables.set_hexpand(true);
    tables.set_sensitive(false);
    tables.set_tooltip_text(Some("Целевая таблица"));
    tables.set_widget_name("csv-target-table");
    settings.append(&tables);
    let delimiter = gtk::DropDown::from_strings(&[
        "Запятая ,",
        "Точка с запятой ;",
        "Табуляция",
        "Вертикальная черта |",
    ]);
    delimiter.set_widget_name("csv-delimiter");
    settings.append(&delimiter);
    if path.extension().is_some_and(|e| e == "tsv") {
        delimiter.set_selected(2);
    }
    let header = gtk::CheckButton::with_label("Заголовки");
    header.set_active(true);
    settings.append(&header);
    root.append(&settings);
    let actions = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    actions.append(&gtk::Label::new(Some("NULL без кавычек:")));
    let null = gtk::Entry::new();
    null.set_text("\\N");
    null.set_width_chars(8);
    null.set_widget_name("csv-null");
    actions.append(&null);
    let preview = gtk::Button::with_label("Предпросмотр");
    preview.set_sensitive(false);
    preview.set_widget_name("csv-preview");
    actions.append(&preview);
    let preview_enter = preview.downgrade();
    null.connect_activate(move |_| {
        if let Some(button) = preview_enter.upgrade().filter(|b| b.is_sensitive()) {
            button.emit_clicked();
        }
    });
    let execute = gtk::Button::with_label("Импортировать");
    execute.add_css_class("suggested-action");
    execute.set_sensitive(false);
    execute.set_widget_name("csv-execute");
    actions.append(&execute);
    root.append(&actions);
    let status = gtk::Label::new(Some("Подключение к выбранной БД…"));
    status.set_widget_name("csv-import-status");
    status.set_wrap(true);
    status.set_xalign(0.0);
    root.append(&status);
    let mapping_title = gtk::Label::new(Some("Столбец CSV → Столбец таблицы"));
    mapping_title.set_xalign(0.0);
    mapping_title.add_css_class("heading");
    root.append(&mapping_title);
    let mapping = gtk::Box::new(gtk::Orientation::Vertical, 4);
    let mapping_scroll = gtk::ScrolledWindow::builder()
        .min_content_height(130)
        .max_content_height(240)
        .child(&mapping)
        .build();
    root.append(&mapping_scroll);
    let text = gtk::TextView::new();
    text.set_editable(false);
    text.set_monospace(true);
    text.set_wrap_mode(gtk::WrapMode::None);
    text.add_css_class("sql-editor");
    let text_scroll = gtk::ScrolledWindow::builder()
        .vexpand(true)
        .child(&text)
        .build();
    root.append(&text_scroll);
    let hint = gtk::Label::new(Some(
        "UTF-8 · запись до 1 МиБ · пустое поле — пустая строка · пропущенные столбцы используют DEFAULT. Ошибка или отмена откатывает строки целевой таблицы. Явные id не обновляют счётчики PostgreSQL.",
    ));
    hint.set_wrap(true);
    hint.add_css_class("dim-label");
    root.append(&hint);
    window.set_content(Some(&root));
    shortcuts::button(&window, &preview, "<Control>r");
    shortcuts::button(&window, &execute, "<Control>Return");
    let ready = Rc::new(RefCell::new(None::<Ready>));
    let generation = Rc::new(Cell::new(0u64));
    let invalidate: Rc<dyn Fn()> = Rc::new({
        let ready = ready.clone();
        let generation = generation.clone();
        let execute = execute.downgrade();
        move || {
            generation.set(generation.get() + 1);
            ready.borrow_mut().take();
            if let Some(execute) = execute.upgrade() {
                execute.set_sensitive(false);
            }
        }
    });
    for control in [&tables, &delimiter] {
        let invalidate = invalidate.clone();
        control.connect_selected_notify(move |_| invalidate());
    }
    let invalid = invalidate.clone();
    header.connect_toggled(move |_| invalid());
    let invalid = invalidate.clone();
    null.connect_changed(move |_| invalid());
    let pool = Rc::new(RefCell::new(None::<DbPool>));
    let objects = Rc::new(RefCell::new(Vec::<DbObject>::new()));
    let (sender, receiver) = mpsc::channel();
    let task_profile = profile.clone();
    ui.runtime.spawn(async move {
        let result = async {
            let secret = resolve_profile_password(&task_profile, secret).await?;
            let pool = DbPool::connect(&task_profile, &secret).await?;
            let (objects, truncated) = if let Some(object) = object {
                (vec![object], false)
            } else {
                let catalog = pool.list_tables(&task_profile).await?;
                (
                    catalog
                        .objects
                        .into_iter()
                        .filter(|o| o.kind == database::DbObjectKind::Table)
                        .collect(),
                    catalog.truncated,
                )
            };
            Ok::<_, String>((pool, objects, truncated))
        }
        .await;
        let _ = sender.send(result);
    });
    let weak = window.downgrade();
    let pool_loaded = pool.clone();
    let objects_loaded = objects.clone();
    let tables_loaded = tables.clone();
    let preview_loaded = preview.clone();
    let status_loaded = status.clone();
    gtk::glib::timeout_add_local(Duration::from_millis(40), move || {
        let Some(window) = weak.upgrade() else {
            return gtk::glib::ControlFlow::Break;
        };
        if !window.is_visible() {
            return gtk::glib::ControlFlow::Break;
        }
        match receiver.try_recv() {
            Ok(Ok((pool, items, truncated))) => {
                let names = items
                    .iter()
                    .map(|o| format!("{}.{}", o.schema, o.name))
                    .collect::<Vec<_>>();
                let refs = names.iter().map(String::as_str).collect::<Vec<_>>();
                tables_loaded.set_model(Some(&gtk::StringList::new(&refs)));
                tables_loaded.set_selected(0);
                tables_loaded.set_sensitive(true);
                preview_loaded.set_sensitive(!items.is_empty());
                status_loaded.set_text(if truncated{"Каталог ограничен 1000 объектами. Для остальных откройте импорт из меню конкретной таблицы."}else if items.is_empty(){"Нет доступных таблиц"}else{"Выберите таблицу и нажмите «Предпросмотр» (Ctrl+R)"});
                *pool_loaded.borrow_mut() = Some(pool);
                *objects_loaded.borrow_mut() = items;
                gtk::glib::ControlFlow::Break
            }
            Ok(Err(e)) => {
                status_loaded.set_text(&e);
                gtk::glib::ControlFlow::Break
            }
            Err(mpsc::TryRecvError::Disconnected) => {
                status_loaded.set_text("Не удалось загрузить таблицы");
                gtk::glib::ControlFlow::Break
            }
            Err(_) => gtk::glib::ControlFlow::Continue,
        }
    });
    let owner = Rc::downgrade(ui);
    let weak = window.downgrade();
    let ready_preview = ready.clone();
    let gen_preview = generation.clone();
    let pool_preview = pool.clone();
    let objects_preview = objects.clone();
    let preview_status = status.clone();
    let preview_execute = execute.clone();
    let path_preview = path.clone();
    preview.connect_clicked(move |button| {
        let (Some(ui), Some(pool), Some(object)) = (
            owner.upgrade(),
            pool_preview.borrow().clone(),
            objects_preview
                .borrow()
                .get(tables.selected() as usize)
                .cloned(),
        ) else {
            return;
        };
        invalidate();
        let version = gen_preview.get();
        let options = csv_input::Options {
            delimiter: [b',', b';', b'\t', b'|'][delimiter.selected() as usize],
            header: header.is_active(),
            null: null.text().to_string(),
            expected_headers: None,
        };
        button.set_sensitive(false);
        preview_status.set_text("Читаю предпросмотр и столбцы…");
        let (sender, receiver) = mpsc::channel();
        let runtime = ui.runtime.clone();
        let path = path_preview.clone();
        let selected = object.clone();
        let opts = options.clone();
        ui.runtime.spawn_blocking(move || {
            let result = runtime.block_on(async {
                let columns = pool.csv_import_columns(&selected).await?;
                let data = csv_input::preview(&path, &opts)?;
                Ok::<_, String>((columns, data))
            });
            let _ = sender.send(result);
        });
        let weak = weak.clone();
        let ready = ready_preview.clone();
        let generation = gen_preview.clone();
        let status = preview_status.clone();
        let execute = preview_execute.clone();
        let button = button.downgrade();
        let mapping = mapping.clone();
        let text = text.clone();
        gtk::glib::timeout_add_local(Duration::from_millis(40), move || {
            let Some(window) = weak.upgrade() else {
                return gtk::glib::ControlFlow::Break;
            };
            if !window.is_visible() {
                return gtk::glib::ControlFlow::Break;
            }
            match receiver.try_recv() {
                Ok(result) => {
                    if let Some(button) = button.upgrade() {
                        button.set_sensitive(true);
                    }
                    if generation.get() != version {
                        return gtk::glib::ControlFlow::Break;
                    }
                    match result {
                        Ok((columns, data)) => {
                            clear_box(&mapping);
                            let mut names = vec!["— Пропустить —".to_string()];
                            names.extend(columns.clone());
                            let refs = names.iter().map(String::as_str).collect::<Vec<_>>();
                            let mut selectors = Vec::new();
                            for (index, name) in data.headers.iter().enumerate() {
                                let row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
                                let label = gtk::Label::new(Some(name));
                                label.set_xalign(0.0);
                                label.set_hexpand(true);
                                label.set_ellipsize(gtk::pango::EllipsizeMode::End);
                                row.append(&label);
                                let select = gtk::DropDown::from_strings(&refs);
                                select.set_hexpand(true);
                                let chosen = if options.header {
                                    columns.iter().position(|c| c == name)
                                } else {
                                    (index < columns.len()).then_some(index)
                                };
                                select.set_selected(chosen.map_or(0, |i| i as u32 + 1));
                                row.append(&select);
                                mapping.append(&row);
                                selectors.push(select);
                            }
                            let mut rendered = String::new();
                            for row in &data.rows {
                                let line = row
                                    .iter()
                                    .map(|f| {
                                        csv_input::value(f, &options.null)
                                            .map(|v| {
                                                v.chars()
                                                    .take(160)
                                                    .collect::<String>()
                                                    .replace('\n', "↵")
                                            })
                                            .unwrap_or("NULL".into())
                                    })
                                    .collect::<Vec<_>>()
                                    .join(" | ");
                                if rendered.len() + line.len() > 256 * 1024 {
                                    break;
                                }
                                rendered.push_str(&line);
                                rendered.push('\n');
                            }
                            text.buffer().set_text(&rendered);
                            status.set_text(
                                "Предпросмотр готов. Проверьте сопоставление; Ctrl+Enter — импорт.",
                            );
                            execute.set_sensitive(!columns.is_empty());
                            let mut import_options = options.clone();
                            if options.header {
                                import_options.expected_headers = Some(data.headers.clone());
                            }
                            *ready.borrow_mut() = Some(Ready {
                                object: object.clone(),
                                options: import_options,
                                selectors,
                                columns,
                                generation: version,
                            });
                        }
                        Err(e) => status.set_text(&e),
                    }
                    gtk::glib::ControlFlow::Break
                }
                Err(mpsc::TryRecvError::Disconnected) => {
                    status.set_text("Предпросмотр не завершён");
                    if let Some(button) = button.upgrade() {
                        button.set_sensitive(true);
                    }
                    gtk::glib::ControlFlow::Break
                }
                Err(_) => gtk::glib::ControlFlow::Continue,
            }
        });
    });
    let owner = Rc::downgrade(ui);
    let weak = window.downgrade();
    execute.connect_clicked(move |_| {
        let (Some(ui), Some(window), Some(pool), Some(ready)) = (owner.upgrade(), weak.upgrade(), pool.borrow().clone(), ready.borrow().clone()) else { return; };
        let mapping = ready.selectors.iter().map(|s| s.selected().checked_sub(1).and_then(|i| ready.columns.get(i as usize).cloned())).collect::<Vec<_>>();
        let targets = mapping.iter().flatten().collect::<std::collections::HashSet<_>>();
        if targets.is_empty() || targets.len() != mapping.iter().flatten().count() {
            show_error_dialog(&ui,"Проверьте столбцы","Выберите хотя бы один столбец; один столбец таблицы нельзя выбрать дважды."); return;
        }
        let details = format!("{}.{}.{} · {}:{} · {}\nФайл: {}\nСопоставление столбцов: {:?}\nСтроки добавляются одной транзакцией. Существующие строки не заменяются.",profile.database,ready.object.schema,ready.object.name,profile.host,profile.port,profile.username,path.display(),mapping);
        let title = format!("Добавить CSV в {}.{}.{}?",profile.database,ready.object.schema,ready.object.name);
        let launch: Rc<dyn Fn()> = Rc::new({
            let owner=Rc::downgrade(&ui); let window=window.downgrade(); let profile=profile.clone(); let path=path.clone(); let generation=generation.clone();
            move || {
                let (Some(ui),Some(window))=(owner.upgrade(),window.upgrade().filter(|w|w.is_visible())) else { return; };
                if generation.get()!=ready.generation { show_error_dialog(&ui,"Параметры изменились","Обновите предпросмотр перед импортом."); return; }
                let Some(cancel)=start_transfer(&ui) else {return;}; window.close();
                let sender=ui.sender.clone(); let runtime=ui.runtime.clone(); let profile=profile.clone(); let path=path.clone(); let object=ready.object.clone(); let options=ready.options.clone(); let mapping=mapping.clone(); let pool=pool.clone(); ui.status.set_text("Импорт CSV…");
                ui.runtime.spawn_blocking(move || {
                    let progress=sender.clone();
                    let result=runtime.block_on(pool.import_csv(&profile,&object,&path,&options,&mapping,&cancel,move |rows| {let _=progress.send(Event::CsvImportProgress(rows));}));
                    let _=sender.send(Event::CsvImported(profile,object,result));
                });
            }
        });
        if matches!(profile.environment,EnvironmentTag::Prod) {
            production_ui::confirm(&ui,&profile,&title,&details,move || launch());
        } else {
            let dialog=gtk::MessageDialog::builder().transient_for(&window).message_type(gtk::MessageType::Question).buttons(gtk::ButtonsType::None).text(&title).secondary_text(&details).build();
            dialog.add_button("Отмена",gtk::ResponseType::Cancel); dialog.add_button("Импортировать",gtk::ResponseType::Accept); dialog.set_default_response(gtk::ResponseType::Cancel); dialogs::install(&dialog);
            dialog.connect_response(move |dialog,response| {dialog.close(); if response==gtk::ResponseType::Accept {launch();}}); dialog.present();
        }
    });
    window.present();
}
