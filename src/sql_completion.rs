//! Best-effort completion for incomplete SQL. Bindings are local to one query.
use crate::database::{SqlCompletionCatalog, quote_mysql_identifier, quote_pg_identifier};
use sqlparser::{
    dialect::{MySqlDialect, PostgreSqlDialect},
    keywords::{ALL_KEYWORDS, Keyword, RESERVED_FOR_TABLE_ALIAS},
    tokenizer::{Token, Tokenizer, Whitespace, Word},
};
use std::collections::HashSet;

const KEYWORDS: &str = "SELECT FROM WHERE JOIN LEFT RIGHT INNER OUTER ON GROUP BY ORDER HAVING LIMIT OFFSET INSERT INTO VALUES UPDATE SET DELETE CREATE ALTER DROP TABLE VIEW INDEX DATABASE SCHEMA AND OR NOT NULL IS IN LIKE ILIKE BETWEEN EXISTS AS DISTINCT UNION ALL CASE WHEN THEN ELSE END COUNT SUM AVG MIN MAX COALESCE CAST DESC ASC TRUE FALSE RETURNING WITH PRIMARY KEY REFERENCES DEFAULT CURRENT_TIMESTAMP BEGIN COMMIT ROLLBACK EXPLAIN ANALYZE";

#[derive(Clone, Copy, Default)]
pub enum Dialect {
    #[default]
    PostgreSql,
    MySql,
}

impl Dialect {
    pub fn parser(self) -> &'static dyn sqlparser::dialect::Dialect {
        match self {
            Self::PostgreSql => &PostgreSqlDialect {},
            Self::MySql => &MySqlDialect {},
        }
    }

    pub fn identifier_delimiter(self) -> char {
        match self {
            Self::PostgreSql => '"',
            Self::MySql => '`',
        }
    }

    fn quote(self, name: &str, force: bool) -> String {
        let parser = self.parser();
        let mut chars = name.chars();
        let bare = chars.next().is_some_and(|c| parser.is_identifier_start(c))
            && chars.all(|c| parser.is_identifier_part(c))
            && !ALL_KEYWORDS.contains(&name.to_ascii_uppercase().as_str())
            && (!matches!(self, Self::PostgreSql) || name == name.to_ascii_lowercase());
        if bare && !force {
            return name.to_owned();
        }
        match self {
            Self::PostgreSql => quote_pg_identifier(name),
            Self::MySql => quote_mysql_identifier(name),
        }
    }

    fn resolved_name(self, word: &Word) -> String {
        if matches!(self, Self::PostgreSql) && word.quote_style.is_none() {
            word.value.to_ascii_lowercase()
        } else {
            word.value.clone()
        }
    }
}

#[derive(Default)]
pub struct Completion {
    pub catalog: SqlCompletionCatalog,
    pub dialect: Dialect,
    pub schema: Option<String>,
}

impl Completion {
    pub fn complete(
        &self,
        before: &str,
        prefix: &str,
        after: &str,
        existing: &str,
        quoted: bool,
    ) -> Vec<String> {
        let mut result = Suggestions::new(prefix, existing, quoted);
        let Ok(left) = Tokenizer::new(self.dialect.parser(), before).tokenize() else {
            return result.items;
        };
        if matches!(
            left.last(),
            Some(Token::Whitespace(Whitespace::SingleLineComment { .. }))
        ) && !before.ends_with('\n')
        {
            return result.items;
        }
        // Keep tokens before a lexical error in the suffix. An unfinished
        // statement later in the editor should not disable earlier completion.
        let mut right = Vec::new();
        let _ = Tokenizer::new(self.dialect.parser(), after)
            .tokenize_with_location_into_buf(&mut right);
        let left: Vec<_> = left.iter().filter(|t| significant(t)).collect();
        let mut tokens = left.clone();
        tokens.extend(right.iter().map(|t| &t.token).filter(|t| significant(t)));
        let (scope, branch, cursor, at_query_level) = query_scope(&tokens, left.len());
        let ctes = cte_names(&scope, self.dialect);
        let bindings = bindings(&scope[branch..], self.dialect, &ctes);
        let (schema, qualifier) = match left.as_slice() {
            [
                ..,
                Token::Word(schema),
                Token::Period,
                Token::Word(table),
                Token::Period,
            ] => (Some(schema), Some(table)),
            [.., Token::Word(word), Token::Period] => (None, Some(word)),
            [.., Token::Period] => return result.items,
            _ => (None, None),
        };

        if at_query_level && relation_expected(&scope[branch..cursor], self.dialect) {
            self.relations(&mut result, qualifier);
        } else if let Some(qualifier) = qualifier {
            let direct = Binding {
                schema,
                table: Some(qualifier),
                label: None,
            };
            let binding = if let Some(schema) = schema {
                let schema = self.dialect.resolved_name(schema);
                let table = self.dialect.resolved_name(qualifier);
                bindings.iter().find(|binding| {
                    binding
                        .table
                        .zip(binding.label)
                        .is_some_and(|(relation, label)| {
                            std::ptr::eq(relation, label)
                                && self.dialect.resolved_name(relation) == table
                                && self.binding_schema(binding, &table).as_deref() == Some(&schema)
                        })
                })
            } else {
                let name = self.dialect.resolved_name(qualifier);
                let mut matches = bindings.iter().filter(|binding| {
                    binding
                        .label
                        .is_some_and(|label| self.dialect.resolved_name(label) == name)
                });
                let found = matches.next();
                if matches.next().is_some() {
                    return result.items;
                }
                found.or_else(|| bindings.is_empty().then_some(&direct))
            };
            if let Some(columns) = binding.and_then(|binding| self.columns(binding)) {
                self.identifiers(&mut result, columns.iter().map(String::as_str));
            }
        } else {
            if !quoted {
                for keyword in KEYWORDS.split_whitespace() {
                    result.add(keyword, || keyword.to_owned(), true);
                }
            }
            for binding in &bindings {
                if let Some(columns) = self.columns(binding) {
                    self.identifiers(&mut result, columns.iter().map(String::as_str));
                }
            }
            if bindings.is_empty() {
                self.relations(&mut result, None);
            }
        }
        result.items
    }

    fn identifiers<'a>(&self, result: &mut Suggestions, names: impl Iterator<Item = &'a str>) {
        for name in names {
            if result.items.len() == 60 {
                break;
            }
            let force = result.quoted;
            result.add(name, || self.dialect.quote(name, force), false);
        }
    }

    fn relations(&self, result: &mut Suggestions, qualifier: Option<&Word>) {
        let namespaces = &self.catalog.namespaces;
        if let Some(schema) = qualifier {
            if let Some(tables) = namespaces.get(&self.dialect.resolved_name(schema)) {
                self.identifiers(result, tables.keys().map(String::as_str));
            }
            return;
        }

        let path = self.search_path();
        for schema in path {
            if let Some(tables) = namespaces.get(schema) {
                self.identifiers(result, tables.keys().map(String::as_str));
            }
        }
        self.identifiers(result, namespaces.keys().map(String::as_str));
        for (schema, tables) in namespaces {
            for name in tables.keys() {
                if result.items.len() == 60 {
                    return;
                }
                if self.visible_schema(name) == Some(schema) {
                    continue;
                }
                let force = result.quoted;
                result.add(
                    name,
                    || {
                        format!(
                            "{}.{}",
                            self.dialect.quote(schema, force),
                            self.dialect.quote(name, force)
                        )
                    },
                    false,
                );
            }
        }
    }

    fn search_path(&self) -> &[String] {
        self.schema
            .as_ref()
            .map_or(&self.catalog.search_path, std::slice::from_ref)
    }

    fn visible_schema(&self, relation: &str) -> Option<&String> {
        self.search_path().iter().find(|schema| {
            self.catalog
                .namespaces
                .get(*schema)
                .is_some_and(|tables| tables.contains_key(relation))
        })
    }

    fn binding_schema<'a>(
        &'a self,
        binding: &Binding<'_>,
        relation: &str,
    ) -> Option<std::borrow::Cow<'a, str>> {
        if let Some(schema) = binding.schema {
            Some(self.dialect.resolved_name(schema).into())
        } else {
            self.visible_schema(relation)
                .map(|schema| schema.as_str().into())
        }
    }

    fn columns(&self, binding: &Binding<'_>) -> Option<&[String]> {
        let relation = self.dialect.resolved_name(binding.table?);
        let schema = self.binding_schema(binding, &relation)?;
        Some(
            self.catalog
                .namespaces
                .get(schema.as_ref())?
                .get(&relation)?,
        )
    }
}

struct Suggestions<'a> {
    prefix: String,
    existing: &'a str,
    quoted: bool,
    items: Vec<String>,
    seen: HashSet<String>,
}

impl<'a> Suggestions<'a> {
    fn new(prefix: &str, existing: &'a str, quoted: bool) -> Self {
        Self {
            prefix: prefix.to_lowercase(),
            existing,
            quoted,
            items: Vec::new(),
            seen: HashSet::new(),
        }
    }

    fn add(&mut self, name: &str, insert: impl FnOnce() -> String, keyword: bool) {
        if name.is_empty() || name.len() > 512 || name.chars().any(char::is_control) {
            return;
        }
        let folded = name.to_lowercase();
        if self.items.len() == 60 || !folded.starts_with(&self.prefix) {
            return;
        }
        let text = insert();
        if text.len() <= 512
            && !text.chars().any(char::is_control)
            && text != self.existing
            && (!keyword || !text.eq_ignore_ascii_case(self.existing))
            && self.seen.insert(text.clone())
        {
            self.items.push(text);
        }
    }
}

fn significant(token: &Token) -> bool {
    !matches!(token, Token::Whitespace(_) | Token::EOF)
}

fn keyword(token: &Token) -> Keyword {
    match token {
        Token::Word(word) => word.keyword,
        _ => Keyword::NoKeyword,
    }
}

fn query_scope<'a>(tokens: &[&'a Token], cursor: usize) -> (Vec<&'a Token>, usize, usize, bool) {
    let mut frames = vec![0];
    for (i, token) in tokens[..cursor].iter().enumerate() {
        match token {
            Token::LParen => frames.push(i + 1),
            Token::RParen if frames.len() > 1 => {
                frames.pop();
            }
            Token::SemiColon => {
                frames.clear();
                frames.push(i + 1);
            }
            _ => {}
        }
    }
    let level = frames
        .iter()
        .rposition(|&start| {
            start < cursor
                && matches!(
                    keyword(tokens[start]),
                    Keyword::SELECT
                        | Keyword::WITH
                        | Keyword::UPDATE
                        | Keyword::DELETE
                        | Keyword::INSERT
                )
        })
        .unwrap_or(0);
    let mut flat = Vec::new();
    let mut depth = 0;
    let mut branch = 0;
    let mut flat_cursor = 0;
    for (i, &token) in tokens.iter().enumerate().skip(frames[level]) {
        if i == cursor {
            flat_cursor = flat.len();
        }
        match token {
            Token::LParen => {
                if depth == 0 {
                    flat.push(token);
                }
                depth += 1;
            }
            Token::RParen if depth == 0 => break,
            Token::RParen => {
                depth -= 1;
                if depth == 0 {
                    flat.push(token);
                }
            }
            Token::SemiColon if depth == 0 => break,
            _ if depth == 0
                && matches!(
                    keyword(token),
                    Keyword::UNION | Keyword::INTERSECT | Keyword::EXCEPT
                ) =>
            {
                if i >= cursor {
                    break;
                }
                branch = flat.len();
            }
            _ if depth == 0 => flat.push(token),
            _ => {}
        }
    }
    if cursor == tokens.len() {
        flat_cursor = flat.len();
    }
    (flat, branch, flat_cursor, level + 1 == frames.len())
}

fn relation_starts<'a>(tokens: &'a [&Token]) -> impl Iterator<Item = usize> + 'a {
    let mut in_from = false;
    tokens.iter().enumerate().filter_map(move |(i, token)| {
        let starts = match keyword(token) {
            Keyword::FROM => {
                in_from = true;
                true
            }
            Keyword::JOIN => {
                in_from = true;
                true
            }
            Keyword::UPDATE if i == 0 => true,
            Keyword::INTO
                if tokens[..i]
                    .iter()
                    .any(|token| keyword(token) == Keyword::INSERT) =>
            {
                true
            }
            Keyword::WHERE
            | Keyword::GROUP
            | Keyword::HAVING
            | Keyword::ORDER
            | Keyword::LIMIT
            | Keyword::OFFSET
            | Keyword::RETURNING
            | Keyword::WINDOW
            | Keyword::SET
            | Keyword::VALUES => {
                in_from = false;
                false
            }
            _ => in_from && **token == Token::Comma,
        };
        starts.then_some(i + 1)
    })
}

fn skip_only<'a, 't>(tokens: &'a [&'t Token], dialect: Dialect) -> &'a [&'t Token] {
    match tokens {
        [only, tail @ ..]
            if matches!(dialect, Dialect::PostgreSql) && keyword(only) == Keyword::ONLY =>
        {
            tail
        }
        _ => tokens,
    }
}

fn relation_expected(tokens: &[&Token], dialect: Dialect) -> bool {
    relation_starts(tokens).last().is_some_and(|i| {
        matches!(
            skip_only(&tokens[i..], dialect),
            [] | [Token::Word(_), Token::Period]
        )
    })
}

struct Binding<'a> {
    schema: Option<&'a Word>,
    table: Option<&'a Word>,
    label: Option<&'a Word>,
}

fn bindings<'a>(
    tokens: &[&'a Token],
    dialect: Dialect,
    ctes: &HashSet<String>,
) -> Vec<Binding<'a>> {
    relation_starts(tokens)
        .filter_map(|start| {
            let (schema, table, rest) = match skip_only(&tokens[start..], dialect) {
                [
                    Token::Word(schema),
                    Token::Period,
                    Token::Word(table),
                    rest @ ..,
                ] => (Some(schema), Some(table), rest),
                [Token::Word(table), rest @ ..] => (None, Some(table), rest),
                [Token::LParen, Token::RParen, rest @ ..] => (None, None, rest),
                _ => return None,
            };
            let label = match rest {
                [Token::Word(as_kw), Token::Word(alias), ..] if as_kw.keyword == Keyword::AS => {
                    Some(alias)
                }
                [Token::Word(alias), ..]
                    if alias.keyword != Keyword::AS
                        && !RESERVED_FOR_TABLE_ALIAS.contains(&alias.keyword) =>
                {
                    Some(alias)
                }
                _ => table,
            };
            let table = table
                .filter(|word| schema.is_some() || !ctes.contains(&dialect.resolved_name(word)));
            Some(Binding {
                schema,
                table,
                label,
            })
        })
        .collect()
}

fn cte_names(tokens: &[&Token], dialect: Dialect) -> HashSet<String> {
    let mut names = HashSet::new();
    let [first, tail @ ..] = tokens else {
        return names;
    };
    if keyword(first) != Keyword::WITH {
        return names;
    }
    let mut rest = tail;
    if rest
        .first()
        .is_some_and(|token| keyword(token) == Keyword::RECURSIVE)
    {
        rest = &rest[1..];
    }
    while let [Token::Word(name), tail @ ..] = rest {
        let tail = match tail {
            [Token::LParen, Token::RParen, rest @ ..] => rest,
            _ => tail,
        };
        let [as_kw, tail @ ..] = tail else { break };
        if keyword(as_kw) != Keyword::AS {
            break;
        }
        let tail = match tail {
            [materialized, rest @ ..] if keyword(materialized) == Keyword::MATERIALIZED => rest,
            [not, materialized, rest @ ..]
                if keyword(not) == Keyword::NOT
                    && keyword(materialized) == Keyword::MATERIALIZED =>
            {
                rest
            }
            _ => tail,
        };
        let [Token::LParen, Token::RParen, tail @ ..] = tail else {
            break;
        };
        names.insert(dialect.resolved_name(name));
        let [Token::Comma, tail @ ..] = tail else {
            break;
        };
        rest = tail;
    }
    names
}

#[cfg(test)]
mod tests {
    use super::*;

    impl Completion {
        fn suggestions(&self, before: &str, prefix: &str, after: &str) -> Vec<String> {
            self.complete(before, prefix, after, prefix, false)
        }
    }

    fn catalog(dialect: Dialect) -> Completion {
        let columns = |names: &[&str]| names.iter().map(|s| (*s).to_owned()).collect();
        Completion {
            catalog: SqlCompletionCatalog {
                namespaces: [
                    (
                        "public".into(),
                        [
                            (
                                "accounts".into(),
                                columns(&["id", "Name", "display name", "say\"hi", "Сотрудник"]),
                            ),
                            ("orders".into(), columns(&["id", "total"])),
                        ]
                        .into(),
                    ),
                    (
                        "audit".into(),
                        [
                            ("accounts".into(), columns(&["id", "event"])),
                            ("select".into(), columns(&["id"])),
                        ]
                        .into(),
                    ),
                    (
                        "Other DB".into(),
                        [("a`b".into(), columns(&["select"]))].into(),
                    ),
                ]
                .into(),
                search_path: vec!["public".into(), "audit".into()],
            },
            dialect,
            schema: None,
        }
    }

    #[test]
    fn relation_and_schema_completion_preserve_qualification() {
        let mut completion = catalog(Dialect::PostgreSql);
        let suggest = |before, prefix| completion.suggestions(before, prefix, "");
        assert_eq!(
            suggest("SELECT * FROM ", "acc"),
            ["accounts", "\"audit\".accounts"]
        );
        assert_eq!(
            suggest("SELECT * FROM orders JOIN audit.", "acc"),
            ["accounts"]
        );
        assert_eq!(
            suggest("SELECT * FROM orders, audit.", "sel"),
            ["\"select\""]
        );
        assert_eq!(suggest("SELECT * FROM ", "disp"), Vec::<String>::new());
        completion.schema = Some("audit".into());
        assert_eq!(
            completion.suggestions("SELECT * FROM ", "acc", ""),
            ["accounts", "\"public\".accounts"]
        );
        assert_eq!(
            completion.suggestions("SELECT a.", "ev", " FROM accounts a"),
            ["\"event\""]
        );
    }

    #[test]
    fn columns_follow_bindings_and_never_leak_across_query_scopes() {
        let completion = catalog(Dialect::PostgreSql);
        let suggest = |before, prefix, after| completion.suggestions(before, prefix, after);
        assert_eq!(
            suggest(
                "SELECT a.",
                "",
                " FROM public.accounts AS a JOIN orders o ON a.id = o.id"
            ),
            [
                "\"id\"",
                "\"Name\"",
                "\"display name\"",
                "\"say\"\"hi\"",
                "Сотрудник"
            ]
        );
        assert_eq!(
            suggest("SELECT \"a.b\".", "СОТР", " FROM public.accounts \"a.b\""),
            ["Сотрудник"]
        );
        assert_eq!(suggest("SELECT total + ", "to", " FROM orders"), ["total"]);
        assert_eq!(
            suggest("SELECT a.", "ev", " FROM audit.accounts a"),
            ["\"event\""]
        );
        assert_eq!(
            suggest("SELECT audit.accounts.", "ev", " FROM audit.accounts"),
            ["\"event\""]
        );
        assert!(suggest("SELECT audit.accounts.", "ev", " FROM audit.accounts a").is_empty());
        assert!(suggest("SELECT audit.accounts.", "ev", " FROM public.accounts").is_empty());
        assert_eq!(
            suggest("SELECT a.", "ev", " FROM ONLY audit.accounts a"),
            ["\"event\""]
        );
        assert_eq!(
            suggest("SELECT * FROM ONLY audit.", "acc", ""),
            ["accounts"]
        );
        assert_eq!(
            suggest("SELECT COUNT(a.", "na", ") FROM accounts a"),
            ["\"Name\""]
        );
        assert_eq!(
            suggest(
                "SELECT * FROM orders x WHERE EXISTS (SELECT x.",
                "ev",
                " FROM audit.accounts x)"
            ),
            ["\"event\""]
        );
        assert!(suggest("SELECT * FROM orders x; SELECT x.", "to", "").is_empty());
        assert!(suggest("SELECT * FROM orders x UNION ALL SELECT x.", "to", "").is_empty());
        assert!(suggest("SELECT a.", "na", " FROM (SELECT 1) a").is_empty());
        assert!(
            suggest(
                "WITH accounts AS (SELECT 1) SELECT a.",
                "na",
                " FROM accounts a"
            )
            .is_empty()
        );
        assert!(
            suggest(
                "WITH accounts AS (SELECT 1) SELECT 1 UNION ALL SELECT a.",
                "na",
                " FROM accounts a"
            )
            .is_empty()
        );
        assert!(suggest("SELECT unknown.", "", " FROM accounts a").is_empty());
    }

    #[test]
    fn mysql_uses_backticks_and_handles_an_empty_default_database() {
        let mut completion = catalog(Dialect::MySql);
        completion.catalog.search_path.clear();
        assert_eq!(
            completion.suggestions("SELECT * FROM ", "acc", ""),
            ["`audit`.accounts", "`public`.accounts"]
        );
        assert_eq!(
            completion.suggestions("SELECT * FROM `Other DB`.", "a", ""),
            ["`a``b`"]
        );
        assert_eq!(
            completion.suggestions("SELECT a.", "sel", " FROM `Other DB`.`a``b` a"),
            ["`select`"]
        );
        assert!(
            completion
                .suggestions("SELECT a.", "na", " FROM accounts a")
                .is_empty()
        );
    }

    #[test]
    fn prefix_limits_and_lexical_context() {
        let mut completion = catalog(Dialect::PostgreSql);
        for prefix in ["s", "se", "sel", "SeL"] {
            assert!(
                completion
                    .suggestions("", prefix, "")
                    .contains(&"SELECT".into())
            );
        }
        assert!(completion.suggestions("SELECT '", "sel", "'").is_empty());
        assert!(
            completion
                .suggestions("SELECT /* ", "sel", " */")
                .is_empty()
        );
        assert!(completion.suggestions("-- ", "sel", "").is_empty());
        assert_eq!(
            completion.suggestions("-- start\n", "sel", ""),
            ["SELECT", "\"select\""]
        );
        assert_eq!(
            completion.suggestions("SELECT o.", "to", " FROM orders o; SELECT '"),
            ["total"]
        );
        assert_eq!(
            completion.complete(
                "SELECT * FROM public.",
                "accounts",
                "",
                "accounts_old",
                false
            ),
            ["accounts"]
        );
        assert_eq!(
            completion.complete("SELECT * FROM audit.", "sel", "", "\"sel", true),
            ["\"select\""]
        );
        let namespace = completion.catalog.namespaces.get_mut("public").unwrap();
        for i in 0..100 {
            namespace.insert(format!("table_{i}"), Vec::new());
        }
        namespace.insert("bad\nname".into(), Vec::new());
        assert_eq!(
            completion.suggestions("SELECT * FROM ", "table_", "").len(),
            60
        );
        assert!(
            completion
                .suggestions("SELECT * FROM ", "bad", "")
                .is_empty()
        );
    }
}
