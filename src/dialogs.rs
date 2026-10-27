//! Shared dismissal for application-owned windows. Dismissal never accepts a form.
use relm4::gtk::{self, prelude::*};
use std::cell::RefCell;

thread_local! {
    static WATCHING: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    static SURFACES: RefCell<Vec<gtk::glib::WeakRef<gtk::Window>>> = const { RefCell::new(Vec::new()) };
    static WINDOWS: RefCell<Vec<gtk::glib::WeakRef<gtk::Window>>> = const { RefCell::new(Vec::new()) };
}

/// Include GTK-owned confirmation dialogs (e.g. overwrite confirmations).
pub fn watch_toplevels() {
    if WATCHING.with(|watching| watching.replace(true)) {
        return;
    }
    gtk::Window::toplevels().connect_items_changed(|model, position, _, added| {
        for index in position..position + added {
            if let Some(window) = model
                .item(index)
                .and_then(|w| w.downcast::<gtk::Window>().ok())
            {
                let attach = |window: &gtk::Window| {
                    let weak = window.downgrade();
                    gtk::glib::idle_add_local_once(move || {
                        if let Some(window) = weak.upgrade() {
                            if window.transient_for().is_some() {
                                install(&window);
                            }
                        }
                    });
                };
                attach(&window);
                window.connect_transient_for_notify(attach);
            }
        }
    });
}

pub fn install(window: &impl IsA<gtk::Window>) {
    let window = window.as_ref();
    let exists = WINDOWS.with(|windows| {
        let mut windows = windows.borrow_mut();
        windows.retain(|w| w.upgrade().is_some());
        let exists = windows.iter().any(|w| w.upgrade().as_ref() == Some(window));
        if !exists {
            windows.push(window.downgrade());
        }
        exists
    });
    if exists {
        return;
    }
    // A native modal surface consumes the parent's pointer events on Wayland.
    // Keep a transient window and consume the outside click before its action.
    window.set_modal(false);
    let weak = window.downgrade();
    window.connect_map(move |_| {
        if let Some(window) = weak.upgrade() {
            WINDOWS.with(|windows| {
                let mut windows = windows.borrow_mut();
                windows.retain(|w| w.upgrade().is_some_and(|w| w != window));
                windows.push(window.downgrade());
            });
        }
    });
    let keys = gtk::EventControllerKey::new();
    keys.set_propagation_phase(gtk::PropagationPhase::Capture);
    let weak = window.downgrade();
    keys.connect_key_pressed(move |_, key, _, _| {
        if key != gtk::gdk::Key::Escape {
            return gtk::glib::Propagation::Proceed;
        }
        if let Some(window) = weak.upgrade() {
            window.close();
        }
        gtk::glib::Propagation::Stop
    });
    window.add_controller(keys);
    window.set_destroy_with_parent(true);
    install_surface(window);
    if let Some(parent) = window.transient_for() {
        install_surface(&parent);
    }
}

fn install_surface(surface: &gtk::Window) {
    let exists = SURFACES.with(|surfaces| {
        let mut surfaces = surfaces.borrow_mut();
        surfaces.retain(|s| s.upgrade().is_some());
        let exists = surfaces
            .iter()
            .any(|s| s.upgrade().as_ref() == Some(surface));
        if !exists {
            surfaces.push(surface.downgrade());
        }
        exists
    });
    if exists {
        return;
    }
    let click = gtk::GestureClick::new();
    click.set_button(0);
    click.set_propagation_phase(gtk::PropagationPhase::Capture);
    let weak = surface.downgrade();
    click.connect_pressed(move |gesture, _, _, _| {
        let Some(surface) = weak.upgrade() else {
            return;
        };
        let top = WINDOWS.with(|windows| {
            windows
                .borrow()
                .iter()
                .rev()
                .filter_map(|w| w.upgrade())
                .find(|w| w.is_visible())
        });
        if let Some(top) = top {
            if top != surface {
                gesture.set_state(gtk::EventSequenceState::Claimed);
                top.close();
            }
        }
    });
    surface.add_controller(click);
}

pub fn file_chooser(
    title: Option<&str>,
    parent: Option<&impl IsA<gtk::Window>>,
    action: gtk::FileChooserAction,
    accept: Option<&str>,
    cancel: Option<&str>,
) -> gtk::FileChooserDialog {
    let dialog = gtk::FileChooserDialog::new(
        title,
        parent,
        action,
        &[
            (cancel.unwrap_or("Отмена"), gtk::ResponseType::Cancel),
            (accept.unwrap_or("Выбрать"), gtk::ResponseType::Accept),
        ],
    );
    dialog.set_default_response(gtk::ResponseType::Accept);
    install(&dialog);
    dialog
}
