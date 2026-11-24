//! Browse the stored rows linked to a table grid row.
use super::*;
use database::{CellDetailKeyColumn, DbObjectKind, ForeignKeyLink};

struct RelationSource {
    ui: std::rc::Weak<Ui>,
    table_key: String,
    generation: Uuid,
    session_key: String,
    pool: DbPool,
    object: DbObject,
    key: Vec<(CellDetailKeyColumn, String)>,
}

impl RelationSource {
    fn is_current(&self) -> bool {
        self.ui.upgrade().is_some_and(|ui| {
            ui.tabulator_grids
                .borrow()
                .get(&self.table_key)
                .is_some_and(|grid| {
                    grid.generation() == Some(self.generation) && grid.webview.is_sensitive()
                })
                && ui
                    .sessions
                    .borrow()
                    .get(&self.session_key)
                    .is_some_and(|(_, pool)| self.pool.same_pool(pool))
        })
    }
}

const STALE: &str =
    "Таблица обновилась или подключение изменилось. Откройте связанные строки заново.";

pub(super) fn show(ui: &Rc<Ui>, table_key: &str, generation: Uuid, row: usize) {
    if !ui
        .tabulator_grids
        .borrow()
        .get(table_key)
        .is_some_and(|grid| grid.generation() == Some(generation) && grid.webview.is_sensitive())
    {
        return;
    }
    let Some(object) = ui.table_objects.borrow().get(table_key).cloned() else {
        return;
    };
    if object.kind != DbObjectKind::Table {
        return;
    }
    let key = {
        let tables = ui.table_data.borrow();
        let Some(data) = tables.get(table_key) else {
            return;
        };
        let Some(cells) = data.rows.get(row) else {
            return;
        };
        data.cell_detail_key(cells)
    };
    let Some(key) = key else {
        let message = "Для просмотра связанных строк нужен полный первичный ключ строки";
        if let Some(state) = ui.table_edit_states.borrow().get(table_key) {
            state.status.set_text(message);
        } else {
            ui.status.set_text(message);
        }
        return;
    };
    let Some(session_key) = ui
        .table_profiles
        .borrow()
        .get(table_key)
        .map(profile_session_key)
    else {
        return;
    };
    if block_if_broken(ui, &session_key, None) {
        return;
    }
    let Some(pool) = ui
        .sessions
        .borrow()
        .get(&session_key)
        .map(|(_, pool)| pool.clone())
    else {
        return;
    };
    let source = Rc::new(RelationSource {
        ui: Rc::downgrade(ui),
        table_key: table_key.to_owned(),
        generation,
        session_key,
        pool,
        object,
        key,
    });

    let window = gtk::Window::builder()
        .transient_for(&ui.window)
        .destroy_with_parent(true)
        .modal(true)
        .title(format!(
            "Связанные строки · {}.{}",
            source.object.schema, source.object.name
        ))
        .default_width(840)
        .default_height(470)
        .build();
    window.set_widget_name("table-relations");
    let keys = gtk::EventControllerKey::new();
    let weak_window = window.downgrade();
    keys.connect_key_pressed(move |_, key, _, _| {
        if key == gtk::gdk::Key::Escape
            && let Some(window) = weak_window.upgrade()
        {
            window.close();
            return gtk::glib::Propagation::Stop;
        }
        gtk::glib::Propagation::Proceed
    });
    window.add_controller(keys);

    let content = gtk::Box::new(gtk::Orientation::Vertical, 8);
    content.set_margin_start(12);
    content.set_margin_end(12);
    content.set_margin_top(12);
    content.set_margin_bottom(12);
    let note = gtk::Label::new(Some("Загружаю внешние ключи…"));
    note.set_widget_name("table-relations-status");
    note.set_wrap(true);
    note.set_xalign(0.0);

    let controls = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    let select = gtk::DropDown::from_strings(&[]);
    select.set_hexpand(true);
    select.set_size_request(200, -1);
    select.set_sensitive(false);
    select.set_tooltip_text(Some("Внешний ключ и направление связи"));
    let show = gtk::Button::with_label("Показать строки");
    show.set_widget_name("table-relations-show");
    show.set_sensitive(false);
    controls.append(&select);
    controls.append(&show);
    content.append(&controls);
    content.append(&note);

    let grid = TabulatorGrid::new(false, |_| {});
    grid.set_theme(&theme::grid_css(ui.active_theme.get()));
    grid.webview.set_visible(false);
    content.append(&grid.webview);
    window.set_child(Some(&content));
    window.present();

    let links = Rc::new(RefCell::new(Vec::<ForeignKeyLink>::new()));
    let (weak_note, weak_show, weak_grid) =
        (note.downgrade(), show.downgrade(), grid.webview.downgrade());
    select.connect_selected_notify(move |_| {
        if let (Some(note), Some(show), Some(grid)) = (
            weak_note.upgrade(),
            weak_show.upgrade(),
            weak_grid.upgrade(),
        ) {
            note.set_text("Строки читаются из БД с учётом сохранённых изменений");
            show.set_sensitive(true);
            grid.set_visible(false);
        }
    });

    let (weak_select, weak_note) = (select.downgrade(), note.downgrade());
    let (runtime, row_source, row_links) = (ui.runtime.clone(), source.clone(), links.clone());
    show.connect_clicked(move |button| {
        let (Some(select), Some(note)) = (weak_select.upgrade(), weak_note.upgrade()) else {
            return;
        };
        let Some(link) = row_links.borrow().get(select.selected() as usize).cloned() else {
            return;
        };
        grid.webview.set_visible(false);
        if !row_source.is_current() {
            note.set_text(STALE);
            button.set_sensitive(false);
            select.set_sensitive(false);
            return;
        }
        button.set_sensitive(false);
        select.set_sensitive(false);
        note.set_text("Загружаю связанные строки…");
        let (pool, object, key) = (
            row_source.pool.clone(),
            row_source.object.clone(),
            row_source.key.clone(),
        );
        let worker = runtime
            .spawn(async move { bounded(pool.read_related_rows(&object, &key, &link)).await });
        let (note, select, button) = (note.downgrade(), select.downgrade(), button.downgrade());
        let (grid, row_source) = (grid.clone(), row_source.clone());
        gtk::glib::MainContext::default().spawn_local(async move {
            let result = worker.await;
            let (Some(note), Some(select), Some(button)) =
                (note.upgrade(), select.upgrade(), button.upgrade())
            else {
                return;
            };
            if !row_source.is_current() {
                note.set_text(STALE);
                return;
            }
            select.set_sensitive(true);
            button.set_sensitive(true);
            match result {
                Ok(Ok(data)) if !data.rows.is_empty() => {
                    let mut text = if data.truncated {
                        format!("Показано {} строк · есть ещё", data.rows.len())
                    } else {
                        format!("Строк: {}", data.rows.len())
                    };
                    if data.truncated_columns {
                        text.push_str(" · показаны первые столбцы");
                    }
                    note.set_text(&text);
                    grid.set_data(
                        &data.columns,
                        &grid_rows(&data),
                        false,
                        false,
                        false,
                        &[],
                        &[],
                    );
                    grid.webview.set_visible(true);
                }
                Ok(Ok(_)) => note.set_text("Связанные строки не найдены"),
                Ok(Err(error)) => note.set_text(&format!("Не удалось загрузить строки: {error}")),
                Err(_) => note.set_text("Не удалось завершить загрузку строк"),
            }
        });
    });

    let (pool, object) = (source.pool.clone(), source.object.clone());
    let worker = ui
        .runtime
        .spawn(async move { bounded(pool.foreign_key_links(&object)).await });
    let (select, note, show) = (select.downgrade(), note.downgrade(), show.downgrade());
    gtk::glib::MainContext::default().spawn_local(async move {
        let result = worker.await;
        let (Some(select), Some(note), Some(show)) =
            (select.upgrade(), note.upgrade(), show.upgrade())
        else {
            return;
        };
        if !source.is_current() {
            note.set_text(STALE);
            return;
        }
        match result {
            Ok(Ok(found)) if !found.is_empty() => {
                let items = gtk::StringList::new(&[]);
                for link in &found {
                    items.append(&link_label(link));
                }
                links.replace(found);
                select.set_model(Some(&items));
                select.set_sensitive(true);
                show.set_sensitive(true);
                note.set_text("Строки читаются из БД с учётом сохранённых изменений");
            }
            Ok(Ok(_)) => note.set_text("Объявленные внешние ключи не найдены"),
            Ok(Err(error)) => {
                note.set_text(&format!("Не удалось прочитать внешние ключи: {error}"))
            }
            Err(_) => note.set_text("Не удалось завершить загрузку внешних ключей"),
        }
    });
}

fn link_label(link: &ForeignKeyLink) -> String {
    let target = link.target();
    let (direction, arrow) = if link.outbound() {
        ("Ссылка на", "→")
    } else {
        ("Ссылка из", "←")
    };
    let columns = link
        .columns()
        .map(|(source, target)| format!("{source} {arrow} {target}"))
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        "{direction} {}.{} · {} ({columns})",
        target.schema,
        target.name,
        link.name()
    )
}

async fn bounded<T>(
    query: impl Future<Output = Result<T, db_error::DbError>>,
) -> Result<T, String> {
    tokio::time::timeout(Duration::from_secs(15), query)
        .await
        .map_err(|_| "Истекло время ожидания ответа от БД".to_owned())?
        .map_err(String::from)
}
