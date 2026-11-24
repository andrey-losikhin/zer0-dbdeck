//! Local SQL editor assistance.
use relm4::gtk::{self, gio, glib, prelude::*, subclass::prelude::*};
use sourceview5::{prelude::ViewExt, subclass::prelude::*};
use std::{
    cell::{Cell, RefCell},
    future::Future,
    pin::Pin,
    rc::Rc,
    sync::Arc,
    time::Duration,
};

pub use crate::sql_completion::Dialect;
use crate::{database::SqlCompletionCatalog, sql_completion::Completion};

pub fn format_buffer(editor: &sourceview5::View, dialect: Dialect) -> Result<(), String> {
    let buffer = editor.buffer();
    let (mut start, mut end) = buffer.bounds();
    let original = buffer.text(&start, &end, true);
    let Some(formatted) = crate::sql_format::format(&original, dialect)? else {
        return Ok(());
    };
    let caret = buffer.cursor_position();
    let anchor = buffer.iter_at_mark(&buffer.selection_bound()).offset();
    let new_caret = formatted.map_offset(caret, caret > anchor);
    let new_anchor = formatted.map_offset(anchor, anchor > caret);

    let completion = editor.completion();
    completion.block_interactive();
    completion.hide();
    buffer.begin_user_action();
    buffer.delete(&mut start, &mut end);
    buffer.insert(&mut start, &formatted.text);
    buffer.select_range(
        &buffer.iter_at_offset(new_caret),
        &buffer.iter_at_offset(new_anchor),
    );
    buffer.end_user_action();
    completion.unblock_interactive();
    Ok(())
}

struct Replacement {
    start: gtk::TextIter,
    caret: gtk::TextIter,
    end: gtk::TextIter,
    delimiter: Option<char>,
}

fn replacement_bounds(
    buffer: &sourceview5::Buffer,
    context: &sourceview5::CompletionContext,
    dialect: Dialect,
) -> Option<Replacement> {
    if buffer.char_count() > 128 * 1024 {
        return None;
    }
    let (mut start, caret) = context.bounds()?;
    let parser = dialect.parser();
    let mut previous = start;
    while previous.backward_char() && parser.is_identifier_part(previous.char()) {
        start = previous;
    }
    let mut end = caret;
    while !end.is_end() && parser.is_identifier_part(end.char()) {
        end.forward_char();
    }
    let mut replacement = Replacement {
        start,
        caret,
        end,
        delimiter: None,
    };

    let delimiter = dialect.identifier_delimiter();
    let line = buffer.iter_at_line_offset(caret.line(), 0)?;
    if !buffer.text(&line, &caret, true).contains(delimiter) {
        return Some(replacement);
    }
    // Lexing up to the caret distinguishes an open identifier from an earlier
    // closed identifier or quote characters in SQL strings and comments.
    let before = buffer.text(&buffer.start_iter(), &caret, true);
    let Err(error) = sqlparser::tokenizer::Tokenizer::new(parser, &before).tokenize() else {
        return Some(replacement);
    };
    if error.location.line != caret.line() as u64 + 1 {
        return Some(replacement);
    }
    let quote =
        buffer.iter_at_line_offset(caret.line(), error.location.column.saturating_sub(1) as i32)?;
    if quote.char() != delimiter || quote.offset() >= caret.offset() {
        return Some(replacement);
    }
    replacement.start = quote;
    replacement.delimiter = Some(delimiter);

    // Include an existing closing delimiter, ignoring doubled escapes.
    // Keep later lines intact when the identifier has no closing delimiter.
    let mut probe = caret;
    while !probe.is_end() && !probe.ends_line() {
        if probe.char() == delimiter {
            probe.forward_char();
            if probe.char() != delimiter {
                replacement.end = probe;
                break;
            }
        }
        probe.forward_char();
    }
    Some(replacement)
}

mod proposal {
    use super::*;
    #[derive(Default)]
    pub struct Proposal {
        pub text: RefCell<String>,
    }
    #[glib::object_subclass]
    impl ObjectSubclass for Proposal {
        const NAME: &'static str = "DbdeckSqlCompletionProposal";
        type Type = super::SqlProposal;
        type Interfaces = (sourceview5::CompletionProposal,);
    }
    impl ObjectImpl for Proposal {}
    impl CompletionProposalImpl for Proposal {}
}
glib::wrapper! { pub struct SqlProposal(ObjectSubclass<proposal::Proposal>) @implements sourceview5::CompletionProposal; }
impl SqlProposal {
    fn new(text: String) -> Self {
        let item: Self = glib::Object::new();
        *item.imp().text.borrow_mut() = text;
        item
    }
    fn text(&self) -> String {
        self.imp().text.borrow().clone()
    }
}
mod provider {
    use super::*;
    #[derive(Default)]
    pub struct Provider {
        pub(super) completion: RefCell<Completion>,
    }
    #[glib::object_subclass]
    impl ObjectSubclass for Provider {
        const NAME: &'static str = "DbdeckSqlCompletionProvider";
        type Type = super::SqlProvider;
        type Interfaces = (sourceview5::CompletionProvider,);
    }
    impl ObjectImpl for Provider {}
    impl Provider {
        fn fill(&self, context: &sourceview5::CompletionContext, model: &gio::ListStore) {
            let completion = self.completion.borrow();
            let items = context
                .buffer()
                .and_then(|buffer| {
                    replacement_bounds(&buffer, context, completion.dialect)
                        .map(|replacement| (buffer, replacement))
                })
                .map(
                    |(
                        buffer,
                        Replacement {
                            start,
                            caret,
                            end,
                            delimiter,
                        },
                    )| {
                        sourceview5::prelude::BufferExt::ensure_highlight(&buffer, &start, &caret);
                        if delimiter.is_none()
                            && ["comment", "string"].iter().any(|class| {
                                sourceview5::prelude::BufferExt::iter_has_context_class(
                                    &buffer, &start, class,
                                )
                            })
                        {
                            return Vec::new();
                        }
                        let typed = buffer.text(&start, &caret, true);
                        let prefix = match delimiter {
                            Some(quote) => typed[quote.len_utf8()..]
                                .replace(&quote.to_string().repeat(2), &quote.to_string()),
                            None => typed.to_string(),
                        };
                        completion
                            .complete(
                                &buffer.text(&buffer.start_iter(), &start, true),
                                &prefix,
                                &buffer.text(&end, &buffer.end_iter(), true),
                                &buffer.text(&start, &end, true),
                                delimiter.is_some(),
                            )
                            .into_iter()
                            .map(SqlProposal::new)
                            .collect()
                    },
                )
                .unwrap_or_default();
            model.splice(0, model.n_items(), &items);
        }
    }
    impl CompletionProviderImpl for Provider {
        fn title(&self) -> Option<glib::GString> {
            Some("SQL · таблицы · поля".into())
        }
        fn priority(&self, _: &sourceview5::CompletionContext) -> i32 {
            100
        }
        fn is_trigger(&self, _: &gtk::TextIter, c: char) -> bool {
            c == '.'
        }
        fn populate(
            &self,
            context: &sourceview5::CompletionContext,
        ) -> Result<gio::ListModel, glib::Error> {
            let model = gio::ListStore::new::<SqlProposal>();
            self.fill(context, &model);
            Ok(model.upcast())
        }
        fn populate_future(
            &self,
            context: &sourceview5::CompletionContext,
        ) -> Pin<Box<dyn Future<Output = Result<gio::ListModel, glib::Error>>>> {
            let result = self.populate(context);
            Box::pin(async move { result })
        }
        fn refilter(&self, context: &sourceview5::CompletionContext, model: &gio::ListModel) {
            if let Some(model) = model.downcast_ref::<gio::ListStore>() {
                self.fill(context, model)
            }
        }
        fn display(
            &self,
            _: &sourceview5::CompletionContext,
            proposal: &sourceview5::CompletionProposal,
            cell: &sourceview5::CompletionCell,
        ) {
            let Some(proposal) = proposal.downcast_ref::<SqlProposal>() else {
                return;
            };
            match cell.column() {
                sourceview5::CompletionColumn::TypedText => cell.set_text(Some(&proposal.text())),
                sourceview5::CompletionColumn::Icon => cell.set_icon_name("insert-text-symbolic"),
                _ => cell.set_text(None),
            }
        }
        fn activate(
            &self,
            context: &sourceview5::CompletionContext,
            proposal: &sourceview5::CompletionProposal,
        ) {
            let (Some(proposal), Some(buffer)) =
                (proposal.downcast_ref::<SqlProposal>(), context.buffer())
            else {
                return;
            };
            let Some(Replacement {
                mut start, mut end, ..
            }) = replacement_bounds(&buffer, context, self.completion.borrow().dialect)
            else {
                return;
            };
            let text = proposal.text();
            buffer.begin_user_action();
            buffer.delete(&mut start, &mut end);
            buffer.insert(&mut start, &text);
            buffer.end_user_action();
        }
        fn key_activates(
            &self,
            _: &sourceview5::CompletionContext,
            _: &sourceview5::CompletionProposal,
            key: gtk::gdk::Key,
            state: gtk::gdk::ModifierType,
        ) -> bool {
            !state.intersects(
                gtk::gdk::ModifierType::CONTROL_MASK
                    | gtk::gdk::ModifierType::ALT_MASK
                    | gtk::gdk::ModifierType::SUPER_MASK
                    | gtk::gdk::ModifierType::SHIFT_MASK,
            ) && matches!(
                key,
                gtk::gdk::Key::Tab | gtk::gdk::Key::Return | gtk::gdk::Key::KP_Enter
            )
        }
    }
}
glib::wrapper! { pub struct SqlProvider(ObjectSubclass<provider::Provider>) @implements sourceview5::CompletionProvider; }
impl SqlProvider {
    pub fn new(dialect: Dialect, schema: Option<String>) -> Self {
        let provider: Self = glib::Object::new();
        *provider.imp().completion.borrow_mut() = Completion {
            dialect,
            schema,
            ..Completion::default()
        };
        provider
    }
    pub fn set_catalog(&self, catalog: SqlCompletionCatalog) {
        self.imp().completion.borrow_mut().catalog = catalog;
    }
}
#[derive(Debug, PartialEq)]
pub struct SyntaxIssue {
    pub start: i32,
    pub end: i32,
    pub message: String,
}
pub fn check_syntax(sql: &str, dialect: Dialect) -> Option<SyntaxIssue> {
    if sql.trim().is_empty() || sql.len() > 128 * 1024 {
        return None;
    }
    let dialect = dialect.parser();
    let error = match sqlparser::tokenizer::Tokenizer::new(dialect, sql).tokenize_with_location() {
        Ok(tokens) => {
            if crate::sql_format::exceeds_parse_budget(&tokens) {
                return None;
            }
            sqlparser::parser::Parser::new(dialect)
                .with_recursion_limit(64)
                .with_tokens_with_locations(tokens)
                .parse_statements()
                .err()?
                .to_string()
        }
        Err(error) => error.to_string(),
    };
    let location = error
        .rsplit_once(" at Line: ")
        .and_then(|(_, loc)| loc.split_once(", Column: "))
        .and_then(|(line, col)| Some((line.parse::<usize>().ok()?, col.parse::<usize>().ok()?)));
    let chars = sql.chars().collect::<Vec<_>>();
    let mut position = 0;
    if let Some((line, col)) = location {
        let mut current_line = 1;
        while position < chars.len() && current_line < line {
            if chars[position] == '\n' {
                current_line += 1;
            }
            position += 1;
        }
        position = (position + col.saturating_sub(1)).min(chars.len());
    }
    if position == chars.len() {
        position = chars.iter().rposition(|c| !c.is_whitespace()).unwrap_or(0);
    }
    let mut start = position;
    let mut end = (position + 1).min(chars.len());
    if chars
        .get(position)
        .is_some_and(|c| c.is_alphanumeric() || *c == '_')
    {
        while start > 0 && (chars[start - 1].is_alphanumeric() || chars[start - 1] == '_') {
            start -= 1;
        }
        while end < chars.len() && (chars[end].is_alphanumeric() || chars[end] == '_') {
            end += 1;
        }
    }
    Some(SyntaxIssue {
        start: start as i32,
        end: end as i32,
        message: format!("Синтаксис: {error}"),
    })
}

pub struct Diagnostics {
    buffer: sourceview5::Buffer,
    tag: gtk::TextTag,
    label: gtk::Label,
    revision: Cell<u64>,
    timer: RefCell<Option<glib::SourceId>>,
    closed: Cell<bool>,
    runtime: Arc<tokio::runtime::Runtime>,
    dialect: Dialect,
}
impl Diagnostics {
    pub fn install(
        buffer: &sourceview5::Buffer,
        label: &gtk::Label,
        runtime: Arc<tokio::runtime::Runtime>,
        dialect: Dialect,
        error_color: &str,
    ) -> Rc<Self> {
        let tag = gtk::TextTag::builder()
            .name("dbdeck-sql-syntax-error")
            .underline(gtk::pango::Underline::Error)
            .build();
        buffer.tag_table().add(&tag);
        let controller = Rc::new(Self {
            buffer: buffer.clone(),
            tag,
            label: label.clone(),
            revision: Cell::new(0),
            timer: RefCell::new(None),
            closed: Cell::new(false),
            runtime,
            dialect,
        });
        controller.set_color(error_color);
        let weak = Rc::downgrade(&controller);
        buffer.connect_changed(move |_| {
            if let Some(controller) = weak.upgrade() {
                controller.schedule();
            }
        });
        controller.schedule();
        controller
    }
    pub fn set_color(&self, color: &str) {
        if let Ok(color) = gtk::gdk::RGBA::parse(color) {
            self.tag.set_underline_rgba(Some(&color));
        }
    }
    pub fn close(&self) {
        self.closed.set(true);
        if let Some(timer) = self.timer.borrow_mut().take() {
            timer.remove();
        }
    }
    fn schedule(self: &Rc<Self>) {
        if self.closed.get() {
            return;
        }
        self.revision.set(self.revision.get().wrapping_add(1));
        self.buffer.remove_tag(
            &self.tag,
            &self.buffer.start_iter(),
            &self.buffer.end_iter(),
        );
        self.label.set_visible(false);
        if let Some(timer) = self.timer.borrow_mut().take() {
            timer.remove();
        }
        let weak = Rc::downgrade(self);
        *self.timer.borrow_mut() = Some(glib::timeout_add_local_once(
            Duration::from_millis(220),
            move || {
                let Some(controller) = weak.upgrade() else {
                    return;
                };
                controller.timer.borrow_mut().take();
                let revision = controller.revision.get();
                let sql = controller
                    .buffer
                    .text(
                        &controller.buffer.start_iter(),
                        &controller.buffer.end_iter(),
                        true,
                    )
                    .to_string();
                let dialect = controller.dialect;
                let worker = controller
                    .runtime
                    .spawn_blocking(move || check_syntax(&sql, dialect));
                let weak = Rc::downgrade(&controller);
                glib::MainContext::default().spawn_local(async move {
                    let Ok(result) = worker.await else { return };
                    let Some(controller) = weak.upgrade() else {
                        return;
                    };
                    if controller.closed.get() || controller.revision.get() != revision {
                        return;
                    }
                    if let Some(issue) = result {
                        controller.buffer.apply_tag(
                            &controller.tag,
                            &controller.buffer.iter_at_offset(issue.start),
                            &controller.buffer.iter_at_offset(issue.end),
                        );
                        controller.label.set_text(&issue.message);
                        controller.label.set_tooltip_text(Some(&format!("{}\nЛокальная проверка синтаксиса. Объекты БД проверяет сервер; расширения диалекта могут не поддерживаться парсером.",issue.message)));
                        controller.label.set_visible(true);
                    }
                });
            },
        ));
    }
}
impl Drop for Diagnostics {
    fn drop(&mut self) {
        if let Some(timer) = self.timer.get_mut().take() {
            timer.remove();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn syntax_checks_dialects_scripts_and_unicode_spans() {
        for dialect in [Dialect::PostgreSql, Dialect::MySql] {
            for sql in [
                "SELECT * FROM sample_records WHERE id != 4;",
                "UPDATE sample_records SET note='Привет' WHERE id=1; DELETE FROM sample_records WHERE id=2;",
                "-- comment only",
                "SELECT 'unterminated-looking '' string';",
            ] {
                assert!(check_syntax(sql, dialect).is_none(), "{sql}");
            }
            let sql = "-- Привет\nSELEC * FROM sample_records";
            let issue = check_syntax(sql, dialect).unwrap();
            assert_eq!(
                sql.chars()
                    .skip(issue.start as usize)
                    .take((issue.end - issue.start) as usize)
                    .collect::<String>(),
                "SELEC"
            );
            assert!(check_syntax("SELECT (", dialect).is_some());
            assert!(check_syntax("SELECT 'unclosed", dialect).is_some());
        }
        assert!(
            check_syntax(
                "SELECT metadata::jsonb ->> 'name' FROM typed_values",
                Dialect::PostgreSql
            )
            .is_none()
        );
        assert!(
            check_syntax(
                "SELECT `name` FROM `sample_records` LIMIT 10",
                Dialect::MySql
            )
            .is_none()
        );
    }
}
