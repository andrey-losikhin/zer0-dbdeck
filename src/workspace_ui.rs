//! Workspace groups are independent of database connections and transactions.
use super::*;

#[derive(Clone)]
pub(super) struct Group {
    pub session_key: String,
    pub schema: Option<String>,
}

pub(super) fn key(profile: &ConnectionProfile, schema: Option<&str>) -> String {
    let session = profile_session_key(profile);
    key_from_session(
        &session,
        if matches!(profile.kind, DatabaseKind::PostgreSql) {
            schema
        } else {
            None
        },
    )
}

pub(super) fn key_from_session(session: &str, schema: Option<&str>) -> String {
    match schema {
        Some(schema) if schema != "public" => {
            serde_json::to_string(&(session, schema)).expect("workspace identity")
        }
        _ => session.to_owned(),
    }
}

pub(super) fn source(ui: &Ui, group: &str) -> String {
    ui.workspace_groups
        .borrow()
        .get(group)
        .map(|g| g.session_key.clone())
        .unwrap_or_else(|| group.to_owned())
}

pub(super) fn active(ui: &Ui, session: &str) -> String {
    ui.active_group_by_session
        .borrow()
        .get(session)
        .cloned()
        .unwrap_or_else(|| session.to_owned())
}

pub(super) fn is_active(ui: &Ui, group: &str) -> bool {
    ui.current_profile.borrow().as_ref().is_some_and(|profile| {
        let session = profile_session_key(profile);
        source(ui, group) == session && active(ui, &session) == group
    })
}

pub(super) fn ensure(ui: &Rc<Ui>, session: &str, schema: Option<&str>) -> String {
    let Some((profile, _)) = ui.sessions.borrow().get(session).cloned() else {
        return session.to_owned();
    };
    let schema = matches!(profile.kind, DatabaseKind::PostgreSql)
        .then(|| schema.unwrap_or("public").to_owned());
    let group = key(&profile, schema.as_deref());
    if profile.database.trim().is_empty() {
        return group;
    }
    let fresh = !ui.workspace_groups.borrow().contains_key(&group);
    ui.workspace_groups
        .borrow_mut()
        .entry(group.clone())
        .or_insert_with(|| Group {
            session_key: session.to_owned(),
            schema: schema.clone(),
        });
    add_workspace_tab(ui, &profile, &group, schema.as_deref());
    if fresh {
        sql_history_ui::restore_group_drafts(ui, &profile, schema.as_deref());
    }
    group
}

pub(super) fn select(ui: &Rc<Ui>, group: &str) {
    let session = source(ui, group);
    ui.active_group_by_session
        .borrow_mut()
        .insert(session.clone(), group.to_owned());
    select_session(ui, &session);
}

pub(super) fn table_group(ui: &Ui, table: &str) -> Option<String> {
    if let Some(id) = table.strip_prefix("sql::") {
        return ui.sql_tabs.borrow().get(id).map(|t| t.group_key.clone());
    }
    let profile = ui.table_profiles.borrow().get(table).cloned()?;
    let object = ui.table_objects.borrow().get(table).cloned()?;
    Some(key(&profile, Some(&object.schema)))
}

pub(super) fn close(ui: &Rc<Ui>, group: &str) {
    let session = source(ui, group);
    let was_active = is_active(ui, group);
    let siblings = ui
        .workspace_groups
        .borrow()
        .iter()
        .filter(|(key, g)| key.as_str() != group && g.session_key == session)
        .count();
    if siblings == 0 {
        close_session(ui, &session);
        return;
    }
    if ui.sql_tabs.borrow().values().any(|t| {
        t.group_key == group
            && (!t.run_button.is_sensitive() || t.session.has_pending_transaction())
    }) {
        ui.status
            .set_text("Завершите SQL-запросы и транзакции этой группы перед закрытием");
        return;
    }
    let tables: Vec<_> = ui
        .table_profiles
        .borrow()
        .keys()
        .filter(|key| table_group(ui, key).as_deref() == Some(group))
        .cloned()
        .collect();
    if tables.iter().any(|key| {
        ui.table_edit_states
            .borrow()
            .get(key)
            .is_some_and(|s| s.saving.get())
    }) {
        ui.status
            .set_text("Подождите завершения сохранения изменений таблицы");
        return;
    }
    let dirty: Vec<_> = tables
        .iter()
        .filter(|key| {
            ui.table_edit_states
                .borrow()
                .get(*key)
                .is_some_and(|s| !s.pending.borrow().is_empty() || s.bulk_column.borrow().is_some())
        })
        .cloned()
        .collect();
    if !dirty.is_empty() {
        let dialog = gtk::MessageDialog::builder()
            .transient_for(&ui.window)
            .buttons(gtk::ButtonsType::YesNo)
            .text("Отменить несохранённые изменения и закрыть группу?")
            .secondary_text("Изменения в таблицах этой группы будут потеряны.")
            .build();
        let owner = Rc::downgrade(ui);
        let group = group.to_owned();
        dialog.connect_response(move |dialog, response| {
            dialog.close();
            if response == gtk::ResponseType::Yes
                && let Some(ui) = owner.upgrade()
            {
                for key in &dirty {
                    if let Some(state) = ui.table_edit_states.borrow().get(key) {
                        state.pending.borrow_mut().clear();
                        state.bulk_column.borrow_mut().take();
                    }
                }
                close(&ui, &group);
            }
        });
        dialogs::install(&dialog);
        dialog.present();
        return;
    }
    let Some((profile, _)) = ui.sessions.borrow().get(&session).cloned() else {
        return;
    };
    if let Err(error) = sql_history_ui::save_drafts(ui, &profile.id) {
        ui.status.set_text(&error);
        return;
    }
    let queries: Vec<_> = ui
        .sql_tabs
        .borrow()
        .iter()
        .filter(|(_, t)| t.group_key == group)
        .map(|(id, _)| id.clone())
        .collect();
    ui.restoring_drafts.set(true);
    for key in tables {
        if !key.starts_with("sql::") {
            close_table_tab(ui, &key);
        }
    }
    for id in queries {
        close_sql_tab(ui, &id);
    }
    ui.restoring_drafts.set(false);
    if let Some(index) = session_page_index(ui, group) {
        ui.session_tabs.remove_page(Some(index));
    }
    if let Some(notebook) = ui.session_table_tabs.borrow_mut().remove(group) {
        ui.table_tabs.remove(&notebook);
    }
    ui.active_table_by_session.borrow_mut().remove(group);
    ui.workspace_groups.borrow_mut().remove(group);
    if ui
        .active_group_by_session
        .borrow()
        .get(&session)
        .is_some_and(|g| g == group)
    {
        let next = ui
            .workspace_groups
            .borrow()
            .iter()
            .find(|(_, g)| g.session_key == session)
            .map(|(key, _)| key.clone());
        if let Some(next) = next {
            ui.active_group_by_session
                .borrow_mut()
                .insert(session.clone(), next.clone());
            if was_active {
                select(ui, &next);
            }
        }
    }
    ui.session_tabs.set_visible(ui.session_tabs.n_pages() > 0);
}
