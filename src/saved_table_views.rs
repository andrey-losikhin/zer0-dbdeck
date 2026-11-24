use super::{
    Rc, SettingsStore, Ui, database, dialogs, gtk, load_table_view, profile_session_key,
    show_error_dialog,
};
use crate::database::DbPool;
use crate::settings::{SavedTableView, SavedViewLayout, TableViewScope};
use gtk::prelude::*;

pub fn menu(ui: &Rc<Ui>, table_key: &str) -> gtk::MenuButton {
    let button = gtk::MenuButton::builder()
        .label("Виды")
        .tooltip_text("Сохранённые представления таблицы")
        .build();
    button.set_widget_name("saved-table-views");
    let actions = gtk::Box::new(gtk::Orientation::Vertical, 4);
    let scroll = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .propagate_natural_width(true)
        .propagate_natural_height(true)
        .max_content_height(280)
        .child(&actions)
        .build();
    let popover = gtk::Popover::builder().child(&scroll).build();
    button.set_popover(Some(&popover));

    let weak_ui = Rc::downgrade(ui);
    let key = table_key.to_owned();
    popover.connect_show(move |popover| {
        while let Some(child) = actions.first_child() {
            actions.remove(&child);
        }
        let Some(ui) = weak_ui.upgrade() else { return };
        let Some(scope) = scope(&ui, &key) else {
            return;
        };
        let save = gtk::Button::with_label("Сохранить текущий вид…");
        save.set_widget_name("save-table-view");
        save.add_css_class("flat");
        save.set_sensitive(is_loaded(&ui, &key));
        let weak_ui = Rc::downgrade(&ui);
        let weak_popover = popover.downgrade();
        let save_key = key.clone();
        save.connect_clicked(move |_| {
            if let Some(popover) = weak_popover.upgrade() {
                popover.popdown();
            }
            if let Some(ui) = weak_ui.upgrade() {
                show_save_dialog(&ui, &save_key);
            }
        });
        actions.append(&save);
        let mut views = ui.store.borrow().as_ref().map_or_else(Vec::new, |store| {
            store.saved_table_views(&scope).cloned().collect()
        });
        views.sort_by(|left, right| left.name.cmp(&right.name));
        if views.is_empty() {
            return;
        }
        actions.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
        for view in views {
            actions.append(&view_row(&ui, popover, &key, view));
        }
    });
    button
}

fn view_row(ui: &Rc<Ui>, popover: &gtk::Popover, key: &str, view: SavedTableView) -> gtk::Box {
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 4);
    let apply = gtk::Button::new();
    let caption = gtk::Label::new(Some(&view.name));
    caption.set_ellipsize(gtk::pango::EllipsizeMode::End);
    caption.set_max_width_chars(32);
    caption.set_xalign(0.0);
    apply.set_child(Some(&caption));
    apply.set_widget_name("apply-table-view");
    apply.set_tooltip_text(Some(&view.name));
    apply.set_sensitive(is_loaded(ui, key));
    apply.set_hexpand(true);
    apply.add_css_class("flat");
    let remove = gtk::Button::from_icon_name("user-trash-symbolic");
    remove.set_widget_name("delete-table-view");
    remove.add_css_class("flat");
    let remove_label = format!("Удалить вид «{}»", view.name);
    remove.set_tooltip_text(Some(&remove_label));

    let weak_ui = Rc::downgrade(ui);
    let weak_popover = popover.downgrade();
    let apply_key = key.to_owned();
    let apply_view = view.clone();
    apply.connect_clicked(move |_| {
        if let Some(popover) = weak_popover.upgrade() {
            popover.popdown();
        }
        if let Some(ui) = weak_ui.upgrade()
            && let Err(error) = apply_view_to_table(&ui, &apply_key, &apply_view)
        {
            show_error_dialog(&ui, "Не удалось применить вид", &error);
        }
    });
    let weak_ui = Rc::downgrade(ui);
    let weak_popover = popover.downgrade();
    remove.connect_clicked(move |_| {
        if let Some(popover) = weak_popover.upgrade() {
            popover.popdown();
        }
        if let Some(ui) = weak_ui.upgrade() {
            confirm_delete(&ui, &view);
        }
    });
    row.append(&apply);
    row.append(&remove);
    row
}

fn scope(ui: &Ui, key: &str) -> Option<TableViewScope> {
    let profiles = ui.table_profiles.borrow();
    let objects = ui.table_objects.borrow();
    Some(TableViewScope::new(profiles.get(key)?, objects.get(key)?))
}

fn is_loaded(ui: &Ui, key: &str) -> bool {
    ui.table_data.borrow().contains_key(key)
        && !ui.table_more_loading.borrow().contains(key)
        && ui
            .table_edit_states
            .borrow()
            .get(key)
            .is_some_and(|edit| edit.load_started.get().is_none())
}

fn has_unsaved_edits(ui: &Ui, key: &str) -> bool {
    ui.table_edit_states.borrow().get(key).is_some_and(|edit| {
        edit.saving.get()
            || edit.grid_editing.get()
            || edit.editing_cell.get().is_some()
            || edit.editing_column.get().is_some()
            || !edit.pending.borrow().is_empty()
            || edit.bulk_column.borrow().is_some()
    })
}

fn show_save_dialog(ui: &Rc<Ui>, key: &str) {
    if !is_loaded(ui, key) {
        return;
    }
    let (Some(scope), Some(view), Some(grid)) = (
        scope(ui, key),
        ui.table_edit_states
            .borrow()
            .get(key)
            .and_then(|edit| edit.loaded_view.borrow().clone()),
        ui.tabulator_grids.borrow().get(key).cloned(),
    ) else {
        return;
    };
    let layout = SavedViewLayout {
        view,
        hidden_columns: grid.hidden_column_names(),
    };
    let dialog = gtk::Dialog::builder()
        .title("Сохранить вид таблицы")
        .transient_for(&ui.window)
        .modal(true)
        .default_width(360)
        .build();
    dialog.set_widget_name("save-table-view-dialog");
    dialog.add_button("Отмена", gtk::ResponseType::Cancel);
    let save = dialog.add_button("Сохранить", gtk::ResponseType::Accept);
    save.add_css_class("suggested-action");
    save.set_sensitive(false);
    dialog.set_default_response(gtk::ResponseType::Accept);
    let content = dialog.content_area();
    content.set_spacing(8);
    content.set_margin_top(12);
    content.set_margin_bottom(12);
    content.set_margin_start(12);
    content.set_margin_end(12);
    let name = gtk::Entry::builder()
        .placeholder_text("Имя вида")
        .activates_default(true)
        .build();
    name.set_widget_name("table-view-name");
    let note = gtk::Label::new(Some(
        "Вид с таким же именем для этой таблицы будет заменён.",
    ));
    note.set_xalign(0.0);
    note.set_wrap(true);
    note.add_css_class("dim-label");
    content.append(&name);
    content.append(&note);
    name.connect_changed(move |name| save.set_sensitive(!name.text().trim().is_empty()));
    let weak_ui = Rc::downgrade(ui);
    let name_for_response = name.clone();
    dialog.connect_response(move |dialog, response| {
        dialog.close();
        if response == gtk::ResponseType::Accept
            && let Some(ui) = weak_ui.upgrade()
        {
            let view = SavedTableView {
                scope: scope.clone(),
                name: name_for_response.text().to_string(),
                layout: layout.clone(),
            };
            update_store(
                &ui,
                "Вид сохранён",
                "Не удалось сохранить вид",
                |store| store.save_table_view(view),
            );
        }
    });
    dialogs::install(&dialog);
    dialog.present();
    name.grab_focus();
}

fn confirm_delete(ui: &Rc<Ui>, view: &SavedTableView) {
    let dialog = gtk::MessageDialog::builder()
        .transient_for(&ui.window)
        .modal(true)
        .message_type(gtk::MessageType::Warning)
        .buttons(gtk::ButtonsType::YesNo)
        .text(format!("Удалить вид «{}»?", view.name))
        .build();
    let weak_ui = Rc::downgrade(ui);
    let view = view.clone();
    dialog.connect_response(move |dialog, response| {
        dialog.close();
        if response == gtk::ResponseType::Yes
            && let Some(ui) = weak_ui.upgrade()
        {
            update_store(
                &ui,
                "Вид удалён",
                "Не удалось удалить вид",
                |store| store.delete_table_view(&view.scope, &view.name),
            );
        }
    });
    dialogs::install(&dialog);
    dialog.present();
}

fn update_store(
    ui: &Rc<Ui>,
    success: &str,
    failure: &str,
    change: impl FnOnce(&mut SettingsStore) -> Result<(), String>,
) {
    let result = ui
        .store
        .borrow_mut()
        .as_mut()
        .ok_or_else(|| "Хранилище настроек не загружено".to_owned())
        .and_then(change);
    match result {
        Ok(()) => ui.status.set_text(success),
        Err(error) => show_error_dialog(ui, failure, &error),
    }
}

const UNSAVED_EDITS: &str = "Сохраните или отмените изменения перед применением вида";

fn apply_view_to_table(ui: &Rc<Ui>, key: &str, saved: &SavedTableView) -> Result<(), String> {
    if scope(ui, key).as_ref() != Some(&saved.scope) || !is_loaded(ui, key) {
        return Err("Дождитесь загрузки таблицы".to_owned());
    }
    if has_unsaved_edits(ui, key) {
        return Err(UNSAVED_EDITS.to_owned());
    }
    let layout = saved.layout.clone();
    if let Some(raw) = &layout.view.raw_condition {
        database::validate_raw_condition(raw)?;
    }
    let grid = ui
        .tabulator_grids
        .borrow()
        .get(key)
        .cloned()
        .ok_or_else(|| "Вкладка таблицы закрыта".to_owned())?;
    let generation = grid.generation();
    let key = key.to_owned();
    let weak_ui = Rc::downgrade(ui);
    grid.prepare_view_change(move |ready| {
        let Some(ui) = weak_ui.upgrade() else { return };
        let Some(grid) = ui.tabulator_grids.borrow().get(&key).cloned() else {
            return;
        };
        if grid.generation() != generation || !grid.webview.is_sensitive() {
            return;
        }
        if !ready || !is_loaded(&ui, &key) || has_unsaved_edits(&ui, &key) {
            if ready {
                grid.set_locked(false);
            }
            show_error_dialog(
                &ui,
                "Не удалось применить вид",
                if has_unsaved_edits(&ui, &key) {
                    UNSAVED_EDITS
                } else {
                    "Завершите редактирование ячейки или дождитесь загрузки таблицы"
                },
            );
            return;
        }
        if !load_table_view(&ui, &key, Some(layout)) {
            grid.set_locked(false);
        }
    });
    Ok(())
}

pub fn commit_loaded(
    ui: &Ui,
    key: &str,
    layout: SavedViewLayout,
    source: &DbPool,
    columns: &[String],
) -> Result<(), String> {
    let current = ui.table_profiles.borrow().get(key).is_some_and(|profile| {
        ui.sessions
            .borrow()
            .get(&profile_session_key(profile))
            .is_some_and(|(_, pool)| pool.same_pool(source))
    });
    if !current {
        return Err("Подключение изменилось. Примените вид снова".to_owned());
    }
    if has_unsaved_edits(ui, key) {
        return Err(UNSAVED_EDITS.to_owned());
    }
    let view = layout.view;
    if let Some(column) = view
        .filters
        .iter()
        .map(|filter| &filter.column)
        .chain(view.sort.iter().map(|sort| &sort.column))
        .find(|column| !columns.contains(column))
    {
        return Err(format!("Столбец «{column}» недоступен в таблице"));
    }
    let (Some(panel), Some(grid)) = (
        ui.table_filter_panels.borrow().get(key).cloned(),
        ui.tabulator_grids.borrow().get(key).cloned(),
    ) else {
        return Err("Вкладка таблицы закрыта".to_owned());
    };

    super::clear_filter_rows(&panel);
    *panel.columns.borrow_mut() = columns.to_vec();
    for filter in &view.filters {
        super::append_filter_row(&panel, columns, Some(filter));
    }
    if view.filters.is_empty() && !columns.is_empty() {
        super::append_filter_row(&panel, columns, None);
    }
    panel.sql_mode.set_active(view.raw_condition.is_some());
    panel
        .raw
        .set_text(view.raw_condition.as_deref().unwrap_or_default());
    let summary = super::table_filter_summary(&view);
    panel.set_summary(&summary);
    panel.toggle.set_active(false);
    grid.restore_column_visibility(layout.hidden_columns);
    ui.table_view_states
        .borrow_mut()
        .insert(key.to_owned(), view);
    Ok(())
}
