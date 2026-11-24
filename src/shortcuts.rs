use super::*;

const COMMANDS: &[(&str, &str)] = &[
    ("<Control><Shift>p", "Палитра команд"),
    ("<Control><Shift>n", "Новое подключение"),
    ("<Control>t", "Новая SQL-вкладка"),
    ("<Control>w", "Закрыть текущую вкладку"),
    ("<Control>Tab", "Следующая вкладка"),
    ("<Control><Shift>Tab", "Предыдущая вкладка"),
    ("<Control><Alt>Page_Down", "Следующее подключение"),
    ("<Control><Alt>Page_Up", "Предыдущее подключение"),
    ("<Control>b", "Свернуть / раскрыть боковую панель"),
    ("<Alt>1", "Подключения"),
    ("<Alt>2", "Таблицы"),
    ("<Control>comma", "Настройки"),
    ("<Control><Shift>d", "Структура таблицы и DDL"),
    ("<Control><Alt>h", "История SQL"),
    ("<Control><Shift>b", "Полная резервная копия текущей БД"),
    ("<Control><Alt><Shift>b", "Структура текущей БД без данных"),
    ("<Control><Shift>i", "Импорт SQL / восстановление БД"),
    ("<Control><Alt>i", "Импорт CSV в таблицу"),
    ("<Alt>d", "Выбрать БД в карточке подключения"),
    ("<Control><Shift>x", "Отменить текущую файловую операцию"),
    ("<Control>1", "Фокус на SQL-редакторе / таблице"),
    ("<Control>2", "Фокус на результате SQL"),
    ("<Control>3", "Фокус на фильтрах таблицы"),
    ("<Control>l", "Фокус на фильтре таблицы"),
    ("<Control><Shift>l", "Поиск таблицы в боковой панели"),
    ("<Control><Alt>l", "Переключить SQL WHERE / условия"),
    ("<Control><Shift>plus", "Добавить условие фильтра"),
    ("<Control><Shift>BackSpace", "Сбросить фильтр"),
    ("<Control>Return", "Выполнить SQL / применить фильтр"),
    (
        "<Control><Shift>Return",
        "Выполнить SQL с правом записи (подтверждение PROD)",
    ),
    ("<Control>r", "Обновить таблицу / повторить SQL"),
    ("<Control><Shift>period", "Остановить SQL-запрос"),
    (
        "<Control><Alt><Shift>r",
        "Сбросить SQL-сессию с подтверждением",
    ),
    ("<Control>s", "Сохранить SQL / изменения таблицы"),
    (
        "<Control><Alt>BackSpace",
        "Отменить правки таблицы / результата SQL",
    ),
    ("<Control>o", "Открыть SQL-файл"),
    ("<Control><Shift>f", "Форматировать SQL"),
    ("<Control><Shift>g", "План запроса (EXPLAIN)"),
    ("<Control><Shift>r", "Переименовать SQL-запрос"),
    ("<Control><Shift>o", "Папка SQL-запросов"),
    ("<Control><Shift>e", "Экспорт показанных строк CSV"),
    ("<Control><Alt>e", "Экспорт всех строк таблицы CSV"),
    ("<Alt>m", "Меню приложения"),
    ("<Control>period", "Меню SQL-вкладки"),
    ("<Control><Alt>t", "Ручной режим SQL / Auto Commit"),
    ("<Control><Alt>Return", "Commit SQL-транзакции"),
    ("<Control><Alt>r", "Rollback SQL-транзакции"),
    ("<Control><Shift>question", "Сочетания клавиш"),
];

// Match command keys in both common keyboard layouts, without changing typed text.
fn normalized(key: gtk::gdk::Key) -> gtk::gdk::Key {
    if key == gtk::gdk::Key::KP_Enter {
        return gtk::gdk::Key::Return;
    }
    if key == gtk::gdk::Key::ISO_Left_Tab {
        return gtk::gdk::Key::Tab;
    }
    let Some(ch) = key.to_unicode() else {
        return key;
    };
    let lower = ch.to_lowercase().next().unwrap_or(ch);
    let latin = match lower {
        'т' => 'n',
        'е' => 't',
        'ц' => 'w',
        'д' => 'l',
        'ы' => 's',
        'щ' => 'o',
        'к' => 'r',
        'у' => 'e',
        'с' => 'c',
        'м' => 'v',
        'ф' => 'a',
        'б' => ',',
        'ю' => '.',
        'з' => 'p',
        'р' => 'h',
        'ь' => 'm',
        'а' => 'f',
        'п' => 'g',
        'в' => 'd',
        'ч' => 'x',
        'й' => 'q',
        'я' => 'z',
        'и' => 'b',
        'г' => 'u',
        'н' => 'y',
        'ш' => 'i',
        'о' => 'j',
        'л' => 'k',
        _ => lower,
    };
    if latin == ',' {
        return gtk::gdk::Key::comma;
    }
    if latin == '.' || latin == '>' {
        return gtk::gdk::Key::period;
    }
    gtk::gdk::Key::from_name(latin.to_string()).unwrap_or(key)
}

fn matches(accelerator: &str, key: gtk::gdk::Key, state: gtk::gdk::ModifierType) -> bool {
    let Some((expected, modifiers)) = gtk::accelerator_parse(accelerator) else {
        return false;
    };
    let mask = gtk::gdk::ModifierType::CONTROL_MASK
        | gtk::gdk::ModifierType::SHIFT_MASK
        | gtk::gdk::ModifierType::ALT_MASK
        | gtk::gdk::ModifierType::SUPER_MASK;
    normalized(key) == normalized(expected) && state & mask == modifiers & mask
}

pub(super) fn button(
    owner: &impl IsA<gtk::Widget>,
    button: &gtk::Button,
    accelerator: &'static str,
) {
    let label = button
        .tooltip_text()
        .or_else(|| button.label())
        .unwrap_or_default();
    button.set_tooltip_text(Some(&format!(
        "{label} ({})",
        gtk::accelerator_get_label(
            gtk::accelerator_parse(accelerator).unwrap().0,
            gtk::accelerator_parse(accelerator).unwrap().1,
        )
    )));
    let keys = gtk::EventControllerKey::new();
    keys.set_propagation_phase(gtk::PropagationPhase::Capture);
    let weak = button.downgrade();
    keys.connect_key_pressed(move |_, key, _, state| {
        if matches(accelerator, key, state)
            && let Some(button) = weak.upgrade()
            && button.is_sensitive()
        {
            button.emit_clicked();
            return gtk::glib::Propagation::Stop;
        }
        gtk::glib::Propagation::Proceed
    });
    owner.add_controller(keys);
}

pub(super) fn default_button(window: &adw::Window, button: &gtk::Button) {
    window.set_default_widget(Some(button));
    fn entries(widget: &gtk::Widget) {
        if let Some(entry) = widget.downcast_ref::<gtk::Entry>() {
            entry.set_activates_default(true);
        } else if let Some(entry) = widget.downcast_ref::<gtk::PasswordEntry>() {
            entry.set_activates_default(true);
        }
        let mut child = widget.first_child();
        while let Some(widget) = child {
            entries(&widget);
            child = widget.next_sibling();
        }
    }
    entries(window.upcast_ref());
}

fn notebook(ui: &Ui) -> Option<gtk::Notebook> {
    ui.table_tabs.visible_child()?.downcast().ok()
}

fn active_key(ui: &Ui) -> Option<String> {
    let notebook = notebook(ui)?;
    let page = notebook.nth_page(notebook.current_page())?;
    table_key_from_page(&notebook, &page)
        .or_else(|| page.tooltip_text().map(|s| s.to_string()))
        .map(|key| key.strip_prefix("sql::").unwrap_or(&key).to_owned())
}

fn focus_filter(ui: &Ui, key: &str) {
    if let Some(panel) = ui.table_filter_panels.borrow().get(key) {
        panel.toggle.set_active(true);
        if panel.sql_mode.is_active() {
            panel.raw.grab_focus();
        } else if let Some(row) = panel.controls.borrow().first() {
            row.value.grab_focus();
        }
    }
}

pub(super) fn help(ui: &Ui) {
    let window = adw::Window::builder()
        .title("Сочетания клавиш")
        .transient_for(&ui.window)
        .modal(true)
        .default_width(590)
        .default_height(650)
        .build();
    add_escape_to_window(&window);
    let content = gtk::Box::new(gtk::Orientation::Vertical, 8);
    content.set_margin_top(16);
    content.set_margin_bottom(16);
    content.set_margin_start(16);
    content.set_margin_end(16);
    for (accelerator, text) in COMMANDS.iter().chain(
        [
            ("Tab", "Между полями и кнопками (Shift+Tab — назад)"),
            ("Return", "Подтвердить поле / активировать кнопку"),
            (
                "Escape",
                "Закрыть меню / диалог; выйти из фильтра; отменить ввод ячейки",
            ),
            ("<Control>c", "Копировать диапазон ячеек"),
            ("<Control>v", "Вставить в диапазон ячеек"),
            ("<Control>a", "Выделить загруженные ячейки / текст поля"),
            ("Return", "Редактировать выбранную ячейку"),
            ("Menu", "Меню выбранной ячейки (стрелки, Enter, Escape)"),
            ("<Control>space", "Автодополнение SQL"),
            ("<Alt>Return", "Подключить / отключить профиль в фокусе"),
            ("<Alt>e", "Изменить профиль в фокусе"),
            ("<Alt>Delete", "Удалить профиль в фокусе (с подтверждением)"),
        ]
        .iter(),
    ) {
        let row = gtk::Box::new(gtk::Orientation::Horizontal, 16);
        let label = gtk::Label::new(Some(text));
        label.set_xalign(0.0);
        label.set_hexpand(true);
        label.set_wrap(true);
        row.append(&label);
        row.append(&gtk::ShortcutLabel::new(accelerator));
        content.append(&row);
    }
    let search = gtk::SearchEntry::builder()
        .placeholder_text("Найти действие или сочетание")
        .build();
    search.set_widget_name("shortcuts-search");
    let searchable = content.clone();
    search.connect_search_changed(move |search| {
        let query = search.text().to_lowercase();
        let mut child = searchable.first_child();
        while let Some(row) = child {
            child = row.next_sibling();
            if let Some(label) = row
                .first_child()
                .and_then(|w| w.downcast::<gtk::Label>().ok())
            {
                let accelerator = row
                    .last_child()
                    .and_then(|w| w.downcast::<gtk::ShortcutLabel>().ok())
                    .and_then(|w| w.accelerator())
                    .map(|s| s.to_string())
                    .unwrap_or_default();
                row.set_visible(
                    label.text().to_lowercase().contains(&query)
                        || accelerator.to_lowercase().contains(&query),
                );
            }
        }
    });
    let close = gtk::Button::with_label("Закрыть");
    let weak = window.downgrade();
    close.connect_clicked(move |_| {
        if let Some(window) = weak.upgrade() {
            window.close();
        }
    });
    content.append(&close);
    let scroll = gtk::ScrolledWindow::builder()
        .child(&content)
        .vexpand(true)
        .build();
    let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
    let header = adw::HeaderBar::new();
    header.set_title_widget(Some(&gtk::Label::new(Some("Сочетания клавиш"))));
    root.append(&header);
    root.append(&search);
    root.append(&scroll);
    window.set_content(Some(&root));
    window.present();
    search.grab_focus();
}

// The shortcut help also lists bindings owned by individual dialogs.
fn palette_commands(
    ui: &Ui,
    active: Option<&str>,
    result_focus: bool,
) -> Vec<(&'static str, &'static str)> {
    let profile = ui.current_profile.borrow();
    let connected = profile.is_some();
    let writable = profile.as_ref().is_some_and(|profile| !profile.read_only);
    let tabs = ui.sql_tabs.borrow();
    let sql = active.and_then(|key| tabs.get(key));
    let table = active.filter(|key| sql.is_none() && ui.table_objects.borrow().contains_key(*key));
    let panels = ui.table_filter_panels.borrow();
    let filter = table.and_then(|key| panels.get(key));
    let edit_key = active.map(|key| {
        if sql.is_some() {
            format!("sql::{key}")
        } else {
            key.to_owned()
        }
    });
    let edits = ui.table_edit_states.borrow();
    let edit = edit_key.as_ref().and_then(|key| edits.get(key));
    let can_save = edit.is_some_and(|edit| edit.save_button.is_sensitive());
    let can_discard = edit.is_some_and(|edit| edit.discard_button.is_sensitive());
    let edits_pending = edit.is_some_and(|edit| {
        edit.saving.get() || edit.grid_editing.get() || !edit.pending.borrow().is_empty()
    });
    let sql_idle = !edits_pending && sql.is_some_and(|tab| tab.run_button.is_sensitive());
    let can_run_sql = sql_idle
        && sql.is_some_and(|tab| {
            let (start, end) = tab.buffer.bounds();
            !tab.buffer.text(&start, &end, true).trim().is_empty()
        });
    let can_reload_table = table.is_some()
        && !edits_pending
        && !edit.is_some_and(|edit| edit.bulk_column.borrow().is_some());
    let table_loaded = table.is_some_and(|key| ui.table_data.borrow().contains_key(key));
    let grid = table.is_some_and(|key| ui.tabulator_grids.borrow().contains_key(key));
    let transfer_idle = ui.transfer_cancel.borrow().is_none();
    let structure = profile.as_ref().is_some_and(|profile| {
        let session = profile_session_key(profile);
        ui.active_table_by_session
            .borrow()
            .get(&workspace_ui::active(ui, &session))
            .is_some_and(|key| ui.table_objects.borrow().contains_key(key))
    });

    COMMANDS
        .iter()
        .filter_map(|&(accelerator, title)| {
            let title = match accelerator {
                "<Control><Shift>p" | "<Alt>d" => return None,
                "<Control>t" | "<Alt>2" | "<Control><Shift>l" if !connected => return None,
                "<Control>w" if active.is_none() => return None,
                "<Control>Tab" | "<Control><Shift>Tab"
                    if notebook(ui).is_none_or(|tabs| tabs.n_pages() < 2) =>
                {
                    return None;
                }
                "<Control><Alt>Page_Down" | "<Control><Alt>Page_Up"
                    if ui.session_tabs.n_pages() < 2 =>
                {
                    return None;
                }
                "<Control><Shift>d" if !structure => return None,
                "<Control><Shift>b" | "<Control><Alt><Shift>b" if !connected || !transfer_idle => {
                    return None;
                }
                "<Control><Shift>i" | "<Control><Alt>i" if !writable || !transfer_idle => {
                    return None;
                }
                "<Control><Shift>x" if !ui.background_tasks.can_cancel(TRANSFER) => {
                    return None;
                }
                "<Control>1" if sql.is_some() => "Фокус на SQL-редакторе",
                "<Control>1" if grid => "Фокус на таблице",
                "<Control>1" => return None,
                "<Control>2" if sql.and_then(SqlTab::result_focus_widget).is_none() => return None,
                "<Control>3" | "<Control>l" | "<Control><Alt>l" if filter.is_none() => return None,
                "<Control><Shift>plus" if !filter.is_some_and(|panel| panel.add.is_sensitive()) => {
                    return None;
                }
                "<Control><Shift>BackSpace"
                    if !filter.is_some_and(|panel| panel.clear.is_sensitive()) =>
                {
                    return None;
                }
                "<Control>Return" if can_run_sql => "Выполнить SQL",
                "<Control>Return"
                    if can_reload_table
                        && filter.is_some_and(|panel| panel.apply.is_sensitive()) =>
                {
                    "Применить фильтр"
                }
                "<Control>Return" => return None,
                "<Control><Shift>Return" if !can_run_sql || !writable => return None,
                "<Control><Shift>g" if !can_run_sql => return None,
                // Ctrl+R repeats Ctrl+Enter in SQL tabs.
                "<Control>r" if can_reload_table => "Обновить таблицу",
                "<Control>r" => return None,
                "<Control><Shift>period"
                    if !sql.is_some_and(|tab| tab.stop_button.is_sensitive()) =>
                {
                    return None;
                }
                "<Control><Alt><Shift>r" | "<Control><Alt>t" if !sql_idle => return None,
                "<Control><Alt>Return" | "<Control><Alt>r"
                    if !sql_idle || !sql.is_some_and(|tab| tab.transaction_state.pending) =>
                {
                    return None;
                }
                "<Control>s" if result_focus && can_save => "Сохранить изменения результата SQL",
                "<Control>s" if sql.is_some() && !result_focus => "Сохранить SQL",
                "<Control>s" if table.is_some() && can_save => "Сохранить изменения таблицы",
                "<Control>s" => return None,
                "<Control><Alt>BackSpace" if result_focus && can_discard => {
                    "Отменить правки результата SQL"
                }
                "<Control><Alt>BackSpace" if table.is_some() && can_discard => {
                    "Отменить правки таблицы"
                }
                "<Control><Alt>BackSpace" => return None,
                "<Control>o" | "<Control><Shift>f" | "<Control><Shift>r" | "<Control><Shift>o"
                | "<Control>period"
                    if sql.is_none() =>
                {
                    return None;
                }
                "<Control><Shift>e" if !transfer_idle => return None,
                "<Control><Shift>e" if sql.is_some_and(|tab| tab.export_button.is_sensitive()) => {
                    "Экспорт результата SQL в CSV"
                }
                "<Control><Shift>e" if !table_loaded => return None,
                "<Control><Alt>e" if !table_loaded || !transfer_idle || filter.is_none() => {
                    return None;
                }
                _ => title,
            };
            Some((accelerator, title))
        })
        .collect()
}

fn sql_result_focused(ui: &Ui, id: &str) -> bool {
    ui.sql_tabs
        .borrow()
        .get(id)
        .and_then(SqlTab::result_focus_widget)
        .is_some_and(|result| {
            gtk::prelude::RootExt::focus(&ui.window)
                .is_some_and(|focus| focus == *result || focus.is_ancestor(result))
        })
}

pub(super) fn palette(ui: &Rc<Ui>) {
    let window = adw::Window::builder()
        .title("Палитра команд")
        .transient_for(&ui.window)
        .modal(true)
        .default_width(600)
        .default_height(480)
        .build();
    add_escape_to_window(&window);
    let content = gtk::Box::new(gtk::Orientation::Vertical, 8);
    content.set_margin_top(12);
    content.set_margin_bottom(12);
    content.set_margin_start(12);
    content.set_margin_end(12);
    let search = gtk::SearchEntry::builder()
        .placeholder_text("Введите название команды…")
        .build();
    search.set_widget_name("command-search");
    let list = gtk::ListBox::new();
    list.set_widget_name("command-list");
    list.set_selection_mode(gtk::SelectionMode::Single);
    list.add_css_class("boxed-list");
    let empty = gtk::Label::new(Some("Команды не найдены"));
    empty.set_margin_top(16);
    empty.add_css_class("dim-label");
    list.set_placeholder(Some(&empty));
    let target = active_key(ui);
    let session = ui
        .current_profile
        .borrow()
        .as_ref()
        .map(profile_session_key);
    let pool_for = |ui: &Ui, key: Option<&str>| {
        key.and_then(|key| ui.sessions.borrow().get(key).map(|(_, pool)| pool.clone()))
    };
    let sql_session_for = |ui: &Ui, key: Option<&str>| {
        key.and_then(|key| ui.sql_tabs.borrow().get(key).map(|tab| tab.session.clone()))
    };
    let pool = pool_for(ui, session.as_deref());
    let sql_session = sql_session_for(ui, target.as_deref());
    let result_focus = target
        .as_deref()
        .is_some_and(|id| sql_result_focused(ui, id));
    let commands = palette_commands(ui, target.as_deref(), result_focus);
    let mut searchable = Vec::with_capacity(commands.len());
    for &(accelerator, description) in &commands {
        let row = gtk::Box::new(gtk::Orientation::Horizontal, 12);
        row.set_margin_top(8);
        row.set_margin_bottom(8);
        row.set_margin_start(8);
        row.set_margin_end(8);
        let label = gtk::Label::new(Some(description));
        label.set_xalign(0.0);
        label.set_wrap(true);
        label.set_hexpand(true);
        row.append(&label);
        row.append(&gtk::ShortcutLabel::new(accelerator));
        list.append(&row);
        let (key, modifiers) = gtk::accelerator_parse(accelerator).unwrap();
        searchable.push(
            format!(
                "{description} {}",
                gtk::accelerator_get_label(key, modifiers)
            )
            .to_lowercase(),
        );
    }
    list.select_row(list.row_at_index(0).as_ref());
    let weak_ui = Rc::downgrade(ui);
    let weak_window = window.downgrade();
    let invoke = Rc::new(move |row: &gtk::ListBoxRow| {
        let Some(&command) = commands
            .get(row.index() as usize)
            .filter(|_| row.is_visible())
        else {
            return;
        };
        let (Some(ui), Some(window)) = (weak_ui.upgrade(), weak_window.upgrade()) else {
            return;
        };
        if !window.is_visible() {
            return;
        }
        let current_session = ui
            .current_profile
            .borrow()
            .as_ref()
            .map(profile_session_key);
        let current_target = active_key(&ui);
        let same_pool = match (&pool, pool_for(&ui, current_session.as_deref())) {
            (Some(previous), Some(current)) => previous.same_pool(&current),
            (None, None) => true,
            _ => false,
        };
        let current_sql = sql_session_for(&ui, current_target.as_deref());
        if current_session != session
            || current_target != target
            || !same_pool
            || sql_session.as_ref().map(sql_session::SqlSession::identity)
                != current_sql.as_ref().map(sql_session::SqlSession::identity)
            || !palette_commands(&ui, target.as_deref(), result_focus).contains(&command)
        {
            window.close();
            ui.status
                .set_text("Команда больше недоступна. Откройте палитру снова.");
            return;
        }
        window.close();
        if let Some((key, modifiers)) = gtk::accelerator_parse(command.0) {
            dispatch_with_focus(&ui, key, modifiers, Some(result_focus));
        }
    });
    let selected = invoke.clone();
    list.connect_row_activated(move |_, row| selected(row));
    let selected_list = list.clone();
    search.connect_activate(move |_| {
        if let Some(row) = selected_list.selected_row() {
            invoke(&row);
        }
    });
    let scroll = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vexpand(true)
        .child(&list)
        .build();
    let adjustment = scroll.vadjustment();
    let filtered = list.clone();
    let position = adjustment.clone();
    search.connect_changed(move |search| {
        let query = search.text().to_lowercase();
        let mut first = None;
        for (index, command) in searchable.iter().enumerate() {
            let row = filtered.row_at_index(index as i32).unwrap();
            let visible = query.split_whitespace().all(|word| command.contains(word));
            row.set_visible(visible);
            if visible && first.is_none() {
                first = Some(row);
            }
        }
        filtered.select_row(first.as_ref());
        position.set_value(position.lower());
    });
    let keys = gtk::EventControllerKey::new();
    keys.set_propagation_phase(gtk::PropagationPhase::Capture);
    let focused_list = list.clone();
    keys.connect_key_pressed(move |_, key, _, modifiers| {
        let step = match key {
            gtk::gdk::Key::Down => 1,
            gtk::gdk::Key::Up => -1,
            _ => return gtk::glib::Propagation::Proceed,
        };
        if !modifiers
            .difference(gtk::gdk::ModifierType::LOCK_MASK)
            .is_empty()
        {
            return gtk::glib::Propagation::Proceed;
        }
        let mut index = focused_list.selected_row().map_or(-1, |row| row.index()) + step;
        while let Some(row) = focused_list.row_at_index(index) {
            if row.is_visible() {
                focused_list.select_row(Some(&row));
                let bounds = row.allocation();
                adjustment.clamp_page(
                    f64::from(bounds.y()),
                    f64::from(bounds.y() + bounds.height()),
                );
                break;
            }
            index += step;
        }
        gtk::glib::Propagation::Stop
    });
    search.add_controller(keys);
    content.append(&search);
    content.append(&scroll);
    window.set_content(Some(&content));
    window.present();
    search.grab_focus();
}

pub(super) fn install(ui: &Rc<Ui>) {
    let keys = gtk::EventControllerKey::new();
    keys.set_propagation_phase(gtk::PropagationPhase::Capture);
    let weak = Rc::downgrade(ui);
    keys.connect_key_pressed(move |_, key, _, state| {
        let Some(ui) = weak.upgrade() else {
            return gtk::glib::Propagation::Proceed;
        };
        if dispatch(&ui, key, state) {
            gtk::glib::Propagation::Stop
        } else {
            gtk::glib::Propagation::Proceed
        }
    });
    ui.window.add_controller(keys);
    // Bubble: a dropdown/menu or inline editor gets Escape first.
    let keys = gtk::EventControllerKey::new();
    let weak = Rc::downgrade(ui);
    keys.connect_key_pressed(move |_, key, _, _| {
        if key == gtk::gdk::Key::Escape
            && let Some(ui) = weak.upgrade()
            && let Some(key) = active_key(&ui)
        {
            if let Some(grid) = ui.tabulator_grids.borrow().get(&key) {
                let focus = gtk::prelude::RootExt::focus(&ui.window);
                if focus.as_ref().is_some_and(|f| {
                    !f.is_ancestor(&grid.webview) && f != grid.webview.upcast_ref::<gtk::Widget>()
                }) {
                    grid.webview.grab_focus();
                    return gtk::glib::Propagation::Stop;
                }
            }
        }
        gtk::glib::Propagation::Proceed
    });
    ui.window.add_controller(keys);
}

pub(super) fn dispatch(ui: &Rc<Ui>, key: gtk::gdk::Key, state: gtk::gdk::ModifierType) -> bool {
    dispatch_with_focus(ui, key, state, None)
}

fn dispatch_with_focus(
    ui: &Rc<Ui>,
    key: gtk::gdk::Key,
    state: gtk::gdk::ModifierType,
    result_focus: Option<bool>,
) -> bool {
    if matches("<Control><Shift>Return", key, state) {
        if let Some(id) = active_key(ui).filter(|id| ui.sql_tabs.borrow().contains_key(id)) {
            run_sql_tab_policy(ui, &id, None, false, SqlRunMode::Write);
        }
        return true;
    }
    if matches("<Control><Alt>i", key, state) {
        if let Some(profile) = ui.current_profile.borrow().clone() {
            csv_import_ui::choose(ui, &profile, None);
        }
        return true;
    }
    if matches("<Control><Shift>i", key, state) {
        if let Some(profile) = ui.current_profile.borrow().clone() {
            backup_ui::choose_import(ui, &profile);
        }
        return true;
    }
    if matches("<Control><Shift>x", key, state) {
        cancel_transfer(ui);
        return true;
    }
    if matches("<Control><Shift>b", key, state) || matches("<Control><Alt><Shift>b", key, state) {
        if let Some(profile) = ui.current_profile.borrow().clone() {
            backup_ui::choose(
                ui,
                &profile,
                if state.contains(gtk::gdk::ModifierType::ALT_MASK) {
                    native_backup::DumpMode::Schema
                } else {
                    native_backup::DumpMode::Full
                },
            );
        }
        return true;
    }

    let is = |accelerator| matches(accelerator, key, state);
    if is("<Control>b") {
        ui.sidebar_toggle.set_active(!ui.sidebar_toggle.is_active());
        return true;
    }
    if is("<Alt>m") {
        ui.application_menu.popup();
        return true;
    }
    if is("<Control><Shift>question") || is("<Control><Shift>slash") {
        help(ui);
        return true;
    }
    if is("<Control><Shift>p") {
        palette(ui);
        return true;
    }
    if is("<Control><Alt>h") {
        sql_history_ui::show(ui);
        return true;
    }
    if is("<Control><Shift>d") {
        structure_ui::show_active(ui);
        return true;
    }
    if is("<Control>comma") {
        ui.preferences.present();
        return true;
    }
    if is("<Control><Shift>n") {
        start_new_connection(ui);
        return true;
    }
    if is("<Alt>1") {
        show_sidebar(ui, "connections");
        ui.sidebar_stack.child_focus(gtk::DirectionType::TabForward);
        return true;
    }
    if is("<Alt>2") {
        show_sidebar(ui, "tables");
        ui.table_filter.grab_focus();
        return true;
    }
    if is("<Control><Shift>l") {
        show_sidebar(ui, "tables");
        ui.table_filter.grab_focus();
        return true;
    }
    if is("<Control>t") {
        let profile = ui.current_profile.borrow().clone();
        if let Some(profile) = profile {
            create_sql_tab(ui, &profile_session_key(&profile));
        }
        return true;
    }
    if is("<Control><Alt>Page_Down") || is("<Control><Alt>Page_Up") {
        let tabs = &ui.session_tabs;
        if let Some(current) = tabs.current_page() {
            let count = tabs.n_pages();
            tabs.set_current_page(Some(
                (current
                    + if is("<Control><Alt>Page_Up") {
                        count - 1
                    } else {
                        1
                    })
                    % count,
            ));
        }
        return true;
    }
    if is("<Control>Tab")
        || is("<Control><Shift>Tab")
        || is("<Control>Page_Down")
        || is("<Control>Page_Up")
    {
        if let Some(tabs) = notebook(ui)
            && let Some(current) = tabs.current_page()
        {
            let count = tabs.n_pages();
            let backwards = is("<Control><Shift>Tab") || is("<Control>Page_Up");
            tabs.set_current_page(Some(
                (current + if backwards { count - 1 } else { 1 }) % count,
            ));
        }
        return true;
    }
    let Some(active) = active_key(ui) else {
        return false;
    };
    let sql = ui.sql_tabs.borrow().contains_key(&active);
    if is("<Control>1") || is("<Control>2") || is("<Control>3") {
        if is("<Control>3") {
            focus_filter(ui, &active);
        } else if let Some(tab) = ui.sql_tabs.borrow().get(&active) {
            if is("<Control>1") {
                tab.editor.grab_focus();
            } else if let Some(result) = tab.result_focus_widget() {
                result.grab_focus();
            }
        } else if let Some(grid) = ui.tabulator_grids.borrow().get(&active) {
            grid.webview.grab_focus();
        }
        return true;
    }
    if sql {
        if is("<Control><Shift>g") {
            explain_sql_tab(ui, &active);
            return true;
        }
        if is("<Control><Shift>period") {
            stop_sql_tab(ui, &active, None);
            return true;
        }
        if is("<Control><Alt><Shift>r") {
            reset_sql_session(ui, &active);
            return true;
        }
        if is("<Control>period") {
            if let Some(tab) = ui.sql_tabs.borrow().get(&active) {
                tab.actions_menu.popup();
            }
            return true;
        }
        let action = if is("<Control><Alt>t") {
            Some(None)
        } else if is("<Control><Alt>Return") {
            Some(Some(sql_session::TransactionAction::Commit))
        } else if is("<Control><Alt>r") {
            Some(Some(sql_session::TransactionAction::Rollback))
        } else {
            None
        };
        if let Some(action) = action {
            change_sql_transaction(ui, &active, action);
            return true;
        }
    }
    if is("<Control>w") {
        if !sql
            && ui
                .table_edit_states
                .borrow()
                .get(&active)
                .is_some_and(|state| state.grid_editing.get())
        {
            ui.status
                .set_text("Завершите ввод ячейки: Enter — принять, Escape — отменить");
            return true;
        }
        if sql {
            close_sql_tab(ui, &active)
        } else {
            close_table_tab(ui, &active)
        }
        return true;
    }
    let result_focus = || sql && result_focus.unwrap_or_else(|| sql_result_focused(ui, &active));
    if is("<Control>s") {
        if result_focus() {
            if let Some(tab) = ui.sql_tabs.borrow().get(&active) {
                tab.tabulator.finish_edit(false);
            }
        } else if sql {
            save_sql_tab(ui, &active);
        } else if let Some(grid) = ui.tabulator_grids.borrow().get(&active).cloned() {
            grid.finish_edit(false);
        }
        return true;
    }
    if is("<Control><Alt>BackSpace") && (!sql || result_focus()) {
        if let Some(tab) = ui.sql_tabs.borrow().get(&active) {
            tab.tabulator.finish_edit(true);
        } else if let Some(grid) = ui.tabulator_grids.borrow().get(&active).cloned() {
            grid.finish_edit(true);
        }
        return true;
    }
    if is("<Control>Return") || is("<Control>r") {
        if sql {
            run_sql_tab(ui, &active);
        } else if is("<Control>r") {
            reload_table_view(ui, &active);
        } else {
            let panel = ui.table_filter_panels.borrow().get(&active).cloned();
            if let Some(panel) = panel {
                if panel.apply.is_sensitive() {
                    panel.apply.emit_clicked();
                }
            }
        }
        return true;
    }
    if is("<Control><Shift>e") {
        if sql {
            choose_sql_csv_export(ui, &active);
        } else if let Some(object) = ui.table_objects.borrow().get(&active).cloned() {
            choose_table_csv_export(ui, &active, &object.name);
        }
        return true;
    }
    if sql {
        if is("<Control><Shift>f") {
            let button = ui.sql_tabs.borrow()[&active].format_button.clone();
            button.emit_clicked();
            return true;
        }
        if is("<Control>o") {
            choose_sql_open(ui, &active);
            return true;
        }
        if is("<Control><Shift>r") {
            rename_sql_tab(ui, &active);
            return true;
        }
        if is("<Control><Shift>o") {
            choose_sql_queries_directory(ui);
            return true;
        }
    } else if let Some(panel) = ui.table_filter_panels.borrow().get(&active).cloned() {
        if is("<Control>l") {
            focus_filter(ui, &active);
            return true;
        }
        if is("<Control><Alt>l") {
            panel.sql_mode.set_active(!panel.sql_mode.is_active());
            focus_filter(ui, &active);
            return true;
        }
        if is("<Control><Shift>plus") || is("<Control>equal") {
            if panel.add.is_sensitive() {
                panel.add.emit_clicked();
            }
            return true;
        }
        if is("<Control><Shift>BackSpace") {
            if panel.clear.is_sensitive() {
                panel.clear.emit_clicked();
            }
            return true;
        }
        if is("<Control><Alt>e") {
            let profile = ui.table_profiles.borrow().get(&active).cloned();
            let object = ui.table_objects.borrow().get(&active).cloned();
            if let (Some(profile), Some(object)) = (profile, object) {
                choose_full_table_csv_export(ui, &profile, &object, &object.name);
            }
            return true;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn commands_are_unique_and_do_not_require_function_keys() {
        let mut keys = std::collections::HashSet::new();
        for (accelerator, _) in COMMANDS {
            assert!(!accelerator.split('>').next_back().unwrap().starts_with('F'));
            assert!(keys.insert(*accelerator), "duplicate {accelerator}");
        }
        assert_eq!(normalized(gtk::gdk::Key::greater), gtk::gdk::Key::period);
    }
    #[test]
    fn command_letters_follow_english_and_russian_layouts() {
        for (russian, latin) in [
            ("Cyrillic_es", "c"),
            ("Cyrillic_ES", "c"),
            ("Cyrillic_yeru", "s"),
            ("Cyrillic_de", "l"),
            ("Cyrillic_be", "comma"),
        ] {
            assert_eq!(
                normalized(gtk::gdk::Key::from_name(russian).unwrap()),
                gtk::gdk::Key::from_name(latin).unwrap()
            );
        }
        assert_eq!(normalized(gtk::gdk::Key::ISO_Left_Tab), gtk::gdk::Key::Tab);
    }
}
