//! Local editor assistance. Neither completion nor syntax checks execute SQL.
use relm4::gtk::{self, gio, glib, prelude::*, subclass::prelude::*};
use sourceview5::subclass::prelude::*;
use std::{
    cell::{Cell, RefCell},
    future::Future,
    pin::Pin,
    rc::Rc,
    sync::Arc,
    time::Duration,
};

pub const KEYWORDS: &str = "SELECT FROM WHERE JOIN LEFT RIGHT INNER OUTER ON GROUP BY ORDER HAVING LIMIT OFFSET INSERT INTO VALUES UPDATE SET DELETE CREATE ALTER DROP TABLE VIEW INDEX DATABASE SCHEMA AND OR NOT NULL IS IN LIKE ILIKE BETWEEN EXISTS AS DISTINCT UNION ALL CASE WHEN THEN ELSE END COUNT SUM AVG MIN MAX COALESCE CAST DESC ASC TRUE FALSE RETURNING WITH PRIMARY KEY REFERENCES DEFAULT CURRENT_TIMESTAMP BEGIN COMMIT ROLLBACK EXPLAIN ANALYZE";

#[derive(Clone)]
struct Candidate {
    text: String,
    folded: String,
    keyword: bool,
}
fn candidates(catalog: &[String]) -> Vec<Candidate> {
    let mut seen = std::collections::HashSet::new();
    KEYWORDS
        .split_whitespace()
        .map(|w| (w.to_owned(), true))
        .chain(catalog.iter().cloned().map(|w| (w, false)))
        .filter_map(|(text, keyword)| {
            let folded = text.to_lowercase();
            if text.is_empty()
                || text.len() > 512
                || text.chars().any(char::is_control)
                || !seen.insert(folded.clone())
            {
                return None;
            }
            Some(Candidate {
                text,
                folded,
                keyword,
            })
        })
        .collect()
}
fn matching(words: &[Candidate], prefix: &str, after_dot: bool) -> Vec<String> {
    let prefix = prefix.to_lowercase();
    words
        .iter()
        .filter(|w| {
            w.folded.starts_with(&prefix)
                && (prefix.is_empty() || w.folded != prefix)
                && (!after_dot || (!w.keyword && !w.text.contains('.')))
        })
        .take(60)
        .map(|w| w.text.clone())
        .collect()
}
fn after_dot(context: &sourceview5::CompletionContext) -> bool {
    context
        .bounds()
        .is_some_and(|(mut start, _)| start.backward_char() && start.char() == '.')
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
        pub(super) words: RefCell<Vec<Candidate>>,
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
            let suppressed =
                context
                    .buffer()
                    .zip(context.bounds())
                    .is_some_and(|(buffer, (start, end))| {
                        sourceview5::prelude::BufferExt::ensure_highlight(&buffer, &start, &end);
                        ["comment", "string"].iter().any(|class| {
                            sourceview5::prelude::BufferExt::iter_has_context_class(
                                &buffer, &start, class,
                            )
                        })
                    });
            let items = matching(&self.words.borrow(), &context.word(), after_dot(context))
                .into_iter()
                .filter(|_| !suppressed)
                .map(SqlProposal::new)
                .collect::<Vec<_>>();
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
            let (Some(proposal), Some(buffer), Some((mut start, mut end))) = (
                proposal.downcast_ref::<SqlProposal>(),
                context.buffer(),
                context.bounds(),
            ) else {
                return;
            };
            let text = proposal.text();
            while !end.is_end() && (end.char().is_alphanumeric() || end.char() == '_') {
                end.forward_char();
            }
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
    pub fn new() -> Self {
        let provider: Self = glib::Object::new();
        provider.set_catalog(&[]);
        provider
    }
    pub fn set_catalog(&self, words: &[String]) {
        *self.imp().words.borrow_mut() = candidates(words);
    }
    pub fn suggestions(&self, prefix: &str, after_dot: bool) -> Vec<String> {
        matching(&self.imp().words.borrow(), prefix, after_dot)
    }
}

#[derive(Clone, Copy)]
pub enum Dialect {
    PostgreSql,
    MySql,
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
    let dialect: &dyn sqlparser::dialect::Dialect = match dialect {
        Dialect::PostgreSql => &sqlparser::dialect::PostgreSqlDialect {},
        Dialect::MySql => &sqlparser::dialect::MySqlDialect {},
    };
    let result = sqlparser::parser::Parser::new(dialect)
        .with_recursion_limit(64)
        .try_with_sql(sql)
        .and_then(|mut parser| parser.parse_statements());
    let error = result.err()?.to_string();
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
    fn prefix_matching_is_immediate_case_insensitive_and_preserves_keywords() {
        let words = candidates(&[
            "sample_records".into(),
            "public.sample_records".into(),
            "Сотрудники".into(),
            "select".into(),
        ]);
        for prefix in ["s", "se", "sel", "SeL"] {
            assert!(matching(&words, prefix, false).contains(&"SELECT".into()));
        }
        assert_eq!(matching(&words, "СОТР", false), vec!["Сотрудники"]);
        assert_eq!(matching(&words, "sam", true), vec!["sample_records"]);
        assert!(
            !matching(&words, "", true)
                .iter()
                .any(|w| w == "SELECT" || w.contains('.'))
        );
        assert!(matching(&words, "nothing", false).is_empty());
        assert_eq!(words.iter().filter(|w| w.folded == "select").count(), 1);
        let many = (0..100)
            .map(|i| format!("table_{i}"))
            .chain(["bad\nname".into(), "x".repeat(513)])
            .collect::<Vec<_>>();
        let many = candidates(&many);
        assert_eq!(matching(&many, "table_", false).len(), 60);
        assert!(
            !many
                .iter()
                .any(|w| w.text.contains('\n') || w.text.len() > 512)
        );
    }
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
