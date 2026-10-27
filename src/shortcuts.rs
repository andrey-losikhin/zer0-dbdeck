use super::*;

const COMMANDS: &[(&str, &str)] = &[
    ("<Control><Shift>p", "Палитра команд"),
    ("<Control><Shift>n", "Новое подключение"),
    ("<Control>t", "Новая SQL-вкладка"),
    ("<Control>w", "Закрыть текущую вкладку"),
    ("<Control>Tab", "Следующая вкладка"),
    ("<Control><Shift>Tab", "Предыдущая вкладка"),
    (
        "<Control><Alt>Page_Down",
        "Следующее подключение (Page Up — предыдущее)",
    ),
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
    ("<Control><Shift>x", "Отменить экспорт / бэкап / импорт"),
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
    let commands: Vec<_> = COMMANDS
        .iter()
        .copied()
        .filter(|(key, _)| *key != "<Control><Shift>p")
        .collect();
    for (accelerator, description) in &commands {
        let row = gtk::Box::new(gtk::Orientation::Horizontal, 12);
        row.set_margin_top(8);
        row.set_margin_bottom(8);
        row.set_margin_start(8);
        row.set_margin_end(8);
        let label = gtk::Label::new(Some(description));
        label.set_xalign(0.0);
        label.set_hexpand(true);
        row.append(&label);
        row.append(&gtk::ShortcutLabel::new(accelerator));
        list.append(&row);
    }
    list.select_row(list.row_at_index(0).as_ref());
    let weak_ui = Rc::downgrade(ui);
    let weak_window = window.downgrade();
    let commands = Rc::new(commands);
    let invoke: Rc<dyn Fn(i32)> = Rc::new(move |index| {
        let Some((accelerator, _)) = commands.get(index as usize) else {
            return;
        };
        if let Some(window) = weak_window.upgrade() {
            window.close();
        }
        if let Some(ui) = weak_ui.upgrade() {
            if let Some((key, modifiers)) = gtk::accelerator_parse(*accelerator) {
                dispatch(&ui, key, modifiers);
            }
        }
    });
    let selected = invoke.clone();
    list.connect_row_activated(move |_, row| selected(row.index()));
    let selected_list = list.clone();
    search.connect_activate(move |_| {
        if let Some(row) = selected_list.selected_row() {
            if row.is_visible() {
                invoke(row.index());
            }
        }
    });
    let filtered = list.clone();
    search.connect_search_changed(move |search| {
        let query = search.text().to_lowercase();
        let mut first = None;
        let mut index = 0;
        while let Some(row) = filtered.row_at_index(index) {
            let title = row
                .child()
                .and_then(|w| w.first_child())
                .and_then(|w| w.downcast::<gtk::Label>().ok())
                .map(|l| l.text().to_lowercase())
                .unwrap_or_default();
            row.set_visible(title.contains(&query));
            if row.is_visible() && first.is_none() {
                first = Some(row);
            }
            index += 1;
        }
        filtered.select_row(first.as_ref());
    });
    let keys = gtk::EventControllerKey::new();
    let focused_list = list.clone();
    keys.connect_key_pressed(move |_, key, _, modifiers| {
        if key == gtk::gdk::Key::Down && modifiers.is_empty() {
            if let Some(row) = focused_list.selected_row() {
                row.grab_focus();
            }
            return gtk::glib::Propagation::Stop;
        }
        gtk::glib::Propagation::Proceed
    });
    search.add_controller(keys);
    content.append(&search);
    let scroll = gtk::ScrolledWindow::builder()
        .vexpand(true)
        .child(&list)
        .build();
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
    if matches("<Control><Shift>Return", key, state) {
        if let Some(id) = active_key(ui).filter(|id| ui.sql_tabs.borrow().contains_key(id)) {
            run_sql_tab_policy(ui, &id, None, false, true);
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
        if ui.transfer_cancel_button.is_visible() {
            ui.transfer_cancel_button.emit_clicked();
        }
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
            } else {
                tab.tabulator.webview.grab_focus();
            }
        } else if let Some(grid) = ui.tabulator_grids.borrow().get(&active) {
            grid.webview.grab_focus();
        }
        return true;
    }
    if sql {
        if is("<Control><Shift>period") {
            stop_sql_tab(ui, &active);
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
    let result_focus = sql
        && ui.sql_tabs.borrow().get(&active).is_some_and(|tab| {
            gtk::prelude::RootExt::focus(&ui.window).is_some_and(|focus| {
                focus == tab.tabulator.webview.clone().upcast::<gtk::Widget>()
                    || focus.is_ancestor(&tab.tabulator.webview)
            })
        });
    if is("<Control>s") {
        if result_focus {
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
    if is("<Control><Alt>BackSpace") && (!sql || result_focus) {
        if let Some(grid) = ui.tabulator_grids.borrow().get(&active).cloned() {
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
