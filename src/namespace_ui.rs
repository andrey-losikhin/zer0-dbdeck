//! One sidebar selector for MySQL databases and PostgreSQL schemas.
use super::*;

#[derive(Default)]
pub(super) struct State {
    choices: HashMap<String, Vec<String>>,
    pub revision: u64,
    updating: bool,
    /// Inside notify::selected: replacing the model there crashes GTK.
    handling: bool,
    selection: Option<(String, String)>,
}

/// Shown while no PostgreSQL schema is open; DropDown cannot be left empty.
const SCHEMA_PLACEHOLDER: &str = "Выберите схему";

fn scope(profile: &ConnectionProfile) -> String {
    let mut profile = profile.clone();
    if !matches!(profile.kind, DatabaseKind::PostgreSql) {
        profile.database.clear();
    }
    profile_session_key(&profile)
}

pub(super) fn widgets() -> (gtk::Box, gtk::Label, gtk::DropDown, adw::Spinner) {
    let row = gtk::Box::new(gtk::Orientation::Vertical, 4);
    row.set_widget_name("namespace-picker");
    row.set_visible(false);
    let label = gtk::Label::new(None);
    label.set_xalign(0.0);
    label.add_css_class("dim-label");
    let heading = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    let spinner = adw::Spinner::new();
    spinner.set_size_request(14, 14);
    spinner.set_visible(false);
    heading.append(&label);
    heading.append(&spinner);
    let selector = gtk::DropDown::from_strings(&[]);
    selector.set_widget_name("namespace-selector");
    selector.set_enable_search(true);
    selector.set_hexpand(true);
    let factory = gtk::SignalListItemFactory::new();
    factory.connect_setup(|_, item| {
        let Some(item) = item.downcast_ref::<gtk::ListItem>() else {
            return;
        };
        let label = gtk::Label::new(None);
        label.set_xalign(0.0);
        label.set_ellipsize(gtk::pango::EllipsizeMode::End);
        label.set_width_chars(1);
        label.set_max_width_chars(28);
        item.set_child(Some(&label));
    });
    factory.connect_bind(|_, item| {
        let Some(item) = item.downcast_ref::<gtk::ListItem>() else {
            return;
        };
        let (Some(value), Some(label)) = (
            item.item().and_downcast::<gtk::StringObject>(),
            item.child().and_downcast::<gtk::Label>(),
        ) else {
            return;
        };
        label.set_text(&value.string());
        label.set_tooltip_text(Some(&value.string()));
    });
    selector.set_factory(Some(&factory));
    row.append(&heading);
    row.append(&selector);
    (row, label, selector, spinner)
}

pub(super) fn install(ui: &Rc<Ui>) {
    let owner = Rc::downgrade(ui);
    ui.namespace_selector
        .connect_selected_notify(move |selector| {
            let Some(ui) = owner.upgrade() else {
                return;
            };
            if ui.namespace_state.borrow().updating {
                return;
            }
            let Some(value) = selector
                .selected_item()
                .and_downcast::<gtk::StringObject>()
                .map(|s| s.string().to_string())
                .filter(|value| value != SCHEMA_PLACEHOLDER)
            else {
                return;
            };
            let (Some(profile), Some(pool)) = (
                ui.current_profile.borrow().clone(),
                ui.pool.borrow().clone(),
            ) else {
                return;
            };
            let selection = (scope(&profile), value.clone());
            if ui.namespace_state.borrow().selection.as_ref() == Some(&selection) {
                return;
            }
            ui.namespace_state.borrow_mut().selection = Some(selection);
            ui.namespace_state.borrow_mut().handling = true;
            // Drop the schema placeholder once the handler has returned.
            let idle_owner = owner.clone();
            gtk::glib::idle_add_local_once(move || {
                if let Some(ui) = idle_owner.upgrade() {
                    ui.namespace_state.borrow_mut().handling = false;
                    sync(&ui);
                }
            });
            if matches!(profile.kind, DatabaseKind::PostgreSql) {
                if profile.database.is_empty() {
                    return;
                }
                let session = profile_session_key(&profile);
                let group = workspace_ui::ensure(&ui, &session, Some(&value));
                ensure_session_table_tabs(&ui, &group);
                workspace_ui::select(&ui, &group);
                ui.catalog_tree
                    .borrow_mut()
                    .schema_expanded
                    .insert((session, value), true);
                catalog_tree::render(&ui);
            } else {
                let mut target = profile.clone();
                target.database = value.clone();
                let target_key = profile_session_key(&target);
                open_database_session(&ui, profile, pool, &value);
                if !ui.sessions.borrow().contains_key(&target_key)
                    && !ui.connect_requests.borrow().contains_key(&target_key)
                {
                    sync(&ui);
                }
            }
        });
}

fn model_values(ui: &Ui) -> Vec<String> {
    ui.namespace_selector
        .model()
        .map(|model| {
            (0..model.n_items())
                .filter_map(|i| {
                    model
                        .item(i)
                        .and_downcast::<gtk::StringObject>()
                        .map(|s| s.string().to_string())
                })
                .collect()
        })
        .unwrap_or_default()
}

pub(super) fn sync(ui: &Ui) {
    let Some(profile) = ui.current_profile.borrow().clone() else {
        ui.namespace_row.set_visible(false);
        return;
    };
    let pg = matches!(profile.kind, DatabaseKind::PostgreSql);
    // PostgreSQL schemas are chosen in the catalog tree.
    if pg {
        ui.namespace_row.set_visible(false);
        return;
    }
    let selected = if pg {
        let group = workspace_ui::active(ui, &profile_session_key(&profile));
        ui.workspace_groups
            .borrow()
            .get(&group)
            .and_then(|g| g.schema.clone())
            .unwrap_or_default()
    } else {
        profile.database.clone()
    };
    let cached = ui
        .namespace_state
        .borrow()
        .choices
        .get(&scope(&profile))
        .cloned();
    let ready = cached.is_some();
    let mut values = cached.unwrap_or_default();
    let selected = if pg && selected.is_empty() {
        SCHEMA_PLACEHOLDER.to_owned()
    } else {
        selected
    };
    if !selected.is_empty() && !values.contains(&selected) {
        values.insert(0, selected.clone());
    }
    if ui.namespace_state.borrow().handling {
        let previous = model_values(ui);
        if previous.contains(&selected) {
            values = previous;
        }
    }
    let index = values
        .iter()
        .position(|value| value == &selected)
        .map(|i| i as u32)
        .unwrap_or(gtk::INVALID_LIST_POSITION);
    let refs: Vec<_> = values.iter().map(String::as_str).collect();
    {
        let mut state = ui.namespace_state.borrow_mut();
        state.updating = true;
        // GTK may deliver notify::selected after the surrounding signal returns.
        state.selection = Some((scope(&profile), selected.clone()));
    }
    ui.namespace_label.set_text(if pg {
        "Схема"
    } else {
        "База данных"
    });
    let previous = model_values(ui);
    if previous != values {
        ui.namespace_selector
            .set_model(Some(&gtk::StringList::new(&refs)));
    }
    if ui.namespace_selector.selected() != index {
        ui.namespace_selector.set_selected(index);
    }
    ui.namespace_selector.set_sensitive(ready);
    ui.namespace_selector
        .set_tooltip_text(Some(if selected.is_empty() {
            "Выберите базу данных"
        } else {
            &selected
        }));
    ui.namespace_row.set_visible(true);
    ui.namespace_state.borrow_mut().updating = false;
}

pub(super) fn refresh(ui: &Rc<Ui>) {
    let revision = {
        let mut state = ui.namespace_state.borrow_mut();
        state.revision = state.revision.wrapping_add(1);
        state.revision
    };
    sync(ui);
    let (Some(profile), Some(pool)) = (
        ui.current_profile.borrow().clone(),
        ui.pool.borrow().clone(),
    ) else {
        return;
    };
    if matches!(profile.kind, DatabaseKind::PostgreSql) {
        return;
    }
    let key = scope(&profile);
    let sender = ui.sender.clone();
    let include_system = show_system_schemas(ui);
    let kind = profile.kind;
    ui.namespace_spinner.set_visible(true);
    let guard = ui.activity.start();
    ui.runtime.spawn(async move {
        let _guard = guard;
        let result = pool
            .database_targets(&profile.database)
            .await
            .map(|choices| {
                choices
                    .into_iter()
                    .map(|(name, _)| name)
                    .filter(|name| include_system || !database::is_system_schema(kind, name))
                    .collect()
            });
        let _ = sender.send(Event::NamespaceChoices(key, revision, result));
    });
}

pub(super) fn schemas_loaded(ui: &Ui, profile: &ConnectionProfile, choices: Vec<String>) {
    ui.namespace_state
        .borrow_mut()
        .choices
        .insert(scope(profile), choices);
    sync(ui);
}

pub(super) fn loaded(ui: &Rc<Ui>, key: &str, revision: u64, result: Result<Vec<String>, String>) {
    if revision == ui.namespace_state.borrow().revision {
        ui.namespace_spinner.set_visible(false);
    }
    if revision != ui.namespace_state.borrow().revision
        || ui
            .current_profile
            .borrow()
            .as_ref()
            .is_none_or(|p| scope(p) != key)
    {
        return;
    }
    match result {
        Ok(choices) => {
            ui.namespace_state
                .borrow_mut()
                .choices
                .insert(key.to_owned(), choices);
            sync(ui);
        }
        Err(error) => {
            sync(ui);
            ui.status.set_text(&error);
            show_error_dialog(ui, "Не удалось загрузить список баз данных", &error);
        }
    }
}
