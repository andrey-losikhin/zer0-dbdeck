use std::{cell::RefCell, collections::HashMap};

use relm4::{gtk, gtk::prelude::*};

pub const TRANSFER: &str = "transfer";
const HISTORY_LIMIT: usize = 10;

#[derive(Clone, Copy)]
pub enum Outcome {
    Completed,
    Failed,
    WithErrors,
    PartialReport,
}

impl Outcome {
    pub fn for_result<T, E>(result: &Result<T, E>) -> Self {
        if result.is_ok() {
            Self::Completed
        } else {
            Self::Failed
        }
    }
}

pub struct BackgroundTasks {
    pub button: gtk::MenuButton,
    count: gtk::Label,
    running: gtk::Box,
    recent: gtk::Box,
    empty: gtk::Label,
    active: RefCell<HashMap<String, TaskRow>>,
}

struct TaskRow {
    root: gtk::Box,
    status: gtk::Label,
    cancel: gtk::Button,
    cancellation_requested: bool,
}

impl BackgroundTasks {
    pub fn new() -> Self {
        let count = gtk::Label::new(None);
        count.set_visible(false);
        let indicator = gtk::Box::new(gtk::Orientation::Horizontal, 4);
        indicator.append(&gtk::Image::from_icon_name("view-list-symbolic"));
        indicator.append(&count);
        let button = gtk::MenuButton::builder()
            .child(&indicator)
            .tooltip_text("Фоновые задачи")
            .build();
        button.update_property(&[gtk::accessible::Property::Label("Фоновые задачи")]);
        let body = gtk::Box::new(gtk::Orientation::Vertical, 8);
        let header = gtk::Label::new(Some("Фоновые задачи"));
        header.set_xalign(0.0);
        header.add_css_class("heading");
        body.append(&header);

        let list = gtk::Box::new(gtk::Orientation::Vertical, 8);
        let empty = gtk::Label::new(Some("Задач пока нет"));
        empty.set_xalign(0.0);
        list.append(&empty);
        let running = gtk::Box::new(gtk::Orientation::Vertical, 8);
        let recent = gtk::Box::new(gtk::Orientation::Vertical, 8);
        list.append(&running);
        list.append(&recent);
        let scroll = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .min_content_width(280)
            .max_content_height(300)
            .propagate_natural_height(true)
            .child(&list)
            .build();
        body.append(&scroll);
        let popover = gtk::Popover::builder().child(&body).build();
        button.set_popover(Some(&popover));
        Self {
            button,
            count,
            running,
            recent,
            empty,
            active: RefCell::default(),
        }
    }

    pub fn start(&self, id: &str, title: &str, cancel: impl Fn() + 'static) {
        let root = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        let labels = gtk::Box::new(gtk::Orientation::Vertical, 2);
        labels.set_hexpand(true);
        let heading = gtk::Label::new(Some(title));
        heading.set_xalign(0.0);
        heading.set_ellipsize(gtk::pango::EllipsizeMode::End);
        heading.set_tooltip_text(Some(title));
        let status = gtk::Label::new(Some("Выполняется…"));
        status.set_xalign(0.0);
        status.set_wrap(true);
        status.add_css_class("dim-label");
        labels.append(&heading);
        labels.append(&status);
        root.append(&labels);

        let stop = gtk::Button::with_label("Отменить");
        stop.set_valign(gtk::Align::Center);
        stop.connect_clicked(move |_| cancel());
        root.append(&stop);
        self.running.prepend(&root);
        self.active.borrow_mut().insert(
            id.to_owned(),
            TaskRow {
                root,
                status,
                cancel: stop,
                cancellation_requested: false,
            },
        );
        self.update_button();
    }

    pub fn can_cancel(&self, id: &str) -> bool {
        self.active
            .borrow()
            .get(id)
            .is_some_and(|row| row.cancel.is_sensitive())
    }

    pub fn cancel_status(&self, id: &str, accepted: bool) {
        if let Some(row) = self.active.borrow_mut().get_mut(id) {
            row.cancellation_requested = accepted;
            row.cancel.set_sensitive(false);
            row.status.set_text(if accepted {
                "Запрошена отмена…"
            } else {
                "Завершается…"
            });
        }
    }

    pub fn progress(&self, id: &str, text: &str) -> bool {
        if let Some(row) = self.active.borrow().get(id)
            && row.cancel.is_sensitive()
        {
            row.status.set_text(text);
            return true;
        }
        false
    }

    pub fn finish(&self, id: &str, outcome: Outcome) {
        let Some(row) = self.active.borrow_mut().remove(id) else {
            return;
        };
        row.status.set_text(match outcome {
            Outcome::Completed => "Завершено",
            Outcome::Failed if row.cancellation_requested => "Не завершено · запрошена отмена",
            Outcome::Failed => "Ошибка",
            Outcome::WithErrors => "Отчёт сохранён с ошибками запросов",
            Outcome::PartialReport => "Частичный отчёт сохранён",
        });
        row.root.remove(&row.cancel);
        self.running.remove(&row.root);
        self.recent.prepend(&row.root);
        if let Some(overflow) =
            std::iter::successors(self.recent.first_child(), gtk::Widget::next_sibling)
                .nth(HISTORY_LIMIT)
        {
            self.recent.remove(&overflow);
        }
        self.update_button();
    }

    fn update_button(&self) {
        let count = self.active.borrow().len();
        let title = if count == 0 {
            "Фоновые задачи".to_owned()
        } else {
            format!("Фоновые задачи · выполняется: {count}")
        };
        self.button.set_tooltip_text(Some(&title));
        self.button
            .update_property(&[gtk::accessible::Property::Label(&title)]);
        self.count.set_visible(count > 0);
        self.count.set_text(&count.to_string());
        self.empty
            .set_visible(count == 0 && self.recent.first_child().is_none());
    }
}
