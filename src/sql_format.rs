//! Format whitespace between SQL tokens while retaining their source spelling.
use crate::sql_completion::Dialect;
use sqlparser::{
    ast::Statement,
    keywords::Keyword,
    parser::Parser,
    tokenizer::{Location, Token, TokenWithSpan, Tokenizer, Whitespace},
};
use std::ops::Range;

pub const MAX_PARSE_TOKENS: usize = 4096;

pub fn exceeds_parse_budget(tokens: &[TokenWithSpan]) -> bool {
    // Flat operator chains create deep ASTs even with a parser recursion limit.
    tokens
        .iter()
        .filter(|token| !matches!(token.token, Token::Whitespace(_)))
        .nth(MAX_PARSE_TOKENS)
        .is_some()
}

struct Edit {
    range: Range<usize>,
    replacement: &'static str,
}

pub struct FormattedSql {
    pub text: String,
    edits: Vec<Edit>,
}

impl FormattedSql {
    // GTK offsets are measured in characters, including for Unicode SQL.
    pub fn map_offset(&self, offset: i32, is_selection_end: bool) -> i32 {
        let mut delta = 0;
        for edit in &self.edits {
            let start = edit.range.start as i32;
            let end = edit.range.end as i32;
            let new_len = edit.replacement.len() as i32;
            if offset < start || (offset == start && start == end && is_selection_end) {
                break;
            }
            if offset < end {
                return start + delta + (offset - start).min(new_len);
            }
            delta += new_len - (end - start);
        }
        offset + delta
    }
}

fn is_space(token: &Token) -> bool {
    matches!(
        token,
        Token::Whitespace(Whitespace::Space | Whitespace::Tab | Whitespace::Newline)
    )
}

fn keyword(token: &Token) -> Keyword {
    match token {
        Token::Word(word) => word.keyword,
        _ => Keyword::NoKeyword,
    }
}

fn starts_clause(previous: Keyword, current: Keyword, next: Keyword) -> bool {
    use Keyword::*;
    match current {
        FROM => !matches!(previous, DELETE | DISTINCT),
        GROUP | ORDER => next == BY,
        LEFT | RIGHT | FULL => matches!(next, JOIN | OUTER),
        INNER | CROSS | OUTER => next == JOIN && !matches!(previous, LEFT | RIGHT | FULL),
        JOIN => !matches!(
            previous,
            LEFT | RIGHT | FULL | INNER | CROSS | OUTER | NATURAL
        ),
        NATURAL => matches!(next, JOIN | LEFT | RIGHT | FULL | INNER),
        WHERE | HAVING | LIMIT | OFFSET | UNION | INTERSECT | EXCEPT | RETURNING | VALUES | SET => {
            true
        }
        _ => false,
    }
}

pub fn format(sql: &str, dialect: Dialect) -> Result<Option<FormattedSql>, String> {
    if sql.len() > 128 * 1024 {
        return Err("Размер SQL превышает 128 КиБ.".into());
    }
    // Session settings may change whether a backslash escapes a closing quote.
    if sql.contains("\\'") || sql.contains("\\\"") {
        return Err(
            "Форматирование SQL с обратной косой чертой перед кавычкой не поддерживается.".into(),
        );
    }
    let mysql = matches!(dialect, Dialect::MySql);
    let dialect = dialect.parser();
    let tokens = Tokenizer::new(dialect, sql)
        .tokenize_with_location()
        .map_err(|error| error.to_string())?;

    let mut bytes = Vec::new();
    let mut lines = vec![0];
    for (byte, ch) in sql.char_indices() {
        bytes.push(byte);
        if ch == '\n' {
            lines.push(bytes.len());
        }
    }
    bytes.push(sql.len());
    let offset = |at: Location| {
        let offset = lines[at.line as usize - 1] + at.column as usize - 1;
        (offset, bytes[offset])
    };

    let mut end = Location::new(1, 1);
    for token in &tokens {
        // The MySQL tokenizer expands executable comments into synthetic tokens.
        let source = &sql[offset(token.span.start).1..];
        if token.span.start != end
            || (mysql && (source.starts_with("/*!") || source.starts_with("/*M!")))
        {
            return Err(
                "Форматирование исполняемых комментариев MySQL/MariaDB не поддерживается.".into(),
            );
        }
        end = token.span.end;
    }
    if offset(end).1 != sql.len() {
        return Err("Не удалось определить границы SQL-токенов.".into());
    }

    let mut edits = Vec::new();
    let mut depth = 0u32;
    let mut significant = tokens
        .iter()
        .filter(|token| !is_space(&token.token))
        .peekable();
    let Some(mut previous) = significant.next() else {
        return Ok(None);
    };
    let mut projection = keyword(&previous.token) == Keyword::SELECT;
    while let Some(current) = significant.next() {
        match previous.token {
            Token::LParen | Token::LBracket => depth += 1,
            Token::RParen | Token::RBracket => depth = depth.saturating_sub(1),
            _ => {}
        }
        let word = keyword(&current.token);
        let (start, start_byte) = offset(previous.span.end);
        let (end, end_byte) = offset(current.span.start);
        let gap = &sql[start_byte..end_byte];
        let clause = depth == 0
            && !gap.is_empty()
            && starts_clause(
                keyword(&previous.token),
                word,
                significant
                    .peek()
                    .map_or(Keyword::NoKeyword, |token| keyword(&token.token)),
            );
        let statement_break = depth == 0 && previous.token == Token::SemiColon;
        let projection_break = depth == 0 && projection && previous.token == Token::Comma;
        // Existing newlines can be significant, for example between two PG strings.
        if gap.bytes().all(|byte| matches!(byte, b' ' | b'\t')) {
            let replacement = if clause || statement_break {
                "\n"
            } else if projection_break {
                "\n    "
            } else if gap.is_empty() {
                ""
            } else {
                " "
            };
            if gap != replacement {
                edits.push(Edit {
                    range: start..end,
                    replacement,
                });
            }
        }
        if statement_break || clause {
            projection = false;
        }
        if depth == 0 && word == Keyword::SELECT {
            projection = true;
        }
        previous = current;
    }
    if edits.is_empty() {
        return Ok(None);
    }
    if exceeds_parse_budget(&tokens) {
        return Err(format!(
            "В SQL больше {MAX_PARSE_TOKENS} синтаксических токенов."
        ));
    }

    let statements = Parser::new(dialect)
        .with_recursion_limit(64)
        .with_tokens_with_locations(tokens)
        .parse_statements()
        .map_err(|error| error.to_string())?;
    if statements
        .iter()
        .any(|stmt| matches!(stmt, Statement::Copy { .. }))
    {
        return Err("Форматирование сценариев COPY не поддерживается.".into());
    }

    let mut text = String::with_capacity(sql.len());
    let mut end = 0;
    for edit in &edits {
        text.push_str(&sql[end..bytes[edit.range.start]]);
        text.push_str(edit.replacement);
        end = bytes[edit.range.end];
    }
    text.push_str(&sql[end..]);
    Ok(Some(FormattedSql { text, edits }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn formatted(sql: &str, dialect: Dialect) -> FormattedSql {
        let result = format(sql, dialect).unwrap().unwrap();
        assert!(format(&result.text, dialect).unwrap().is_none());
        result
    }

    #[test]
    fn lays_out_postgres_queries_and_preserves_source_tokens() {
        let sql = r#"select  "Имя",coalesce(amount, 0), $$from  where$$ from logs LEFT OUTER JOIN accounts on accounts.id=logs.id where value = E'a\nb' order  by 1;select 2;"#;
        let result = formatted(sql, Dialect::PostgreSql);
        assert_eq!(
            result.text,
            "select \"Имя\",\n    coalesce(amount, 0),\n    $$from  where$$\nfrom logs\nLEFT OUTER JOIN accounts on accounts.id=logs.id\nwhere value = E'a\\nb'\norder by 1;\nselect 2;"
        );
        for token in ["Имя", "coalesce", "where value", "select 2;"] {
            let index = |text: &str| text[..text.find(token).unwrap()].chars().count() as i32;
            assert_eq!(result.map_offset(index(sql), false), index(&result.text));
        }
    }

    #[test]
    fn preserves_mysql_quotes_comments_and_operator_adjacency() {
        let sql = "SELECT  \x60col,name\x60,2--1, 'a  b' FROM \x60table\x60 # WHERE remains here\r\nWHERE id = 1;UPDATE \x60table\x60 SET id = 2";
        assert_eq!(
            formatted(sql, Dialect::MySql).text,
            "SELECT \x60col,name\x60,\n    2--1,\n    'a  b'\nFROM \x60table\x60 # WHERE remains here\r\nWHERE id = 1;\nUPDATE \x60table\x60\nSET id = 2"
        );
    }

    #[test]
    fn preserves_significant_newlines_and_comments() {
        let sql = "SELECT  'a'\r\n  'b', /* keep  this */ ARRAY[1, 2] FROM t -- Keep  spaces\nWHERE id = 1";
        assert_eq!(
            formatted(sql, Dialect::PostgreSql).text,
            "SELECT 'a'\r\n  'b',\n    /* keep  this */ ARRAY[1, 2]\nFROM t -- Keep  spaces\nWHERE id = 1"
        );
        assert!(
            format("-- a comment\n", Dialect::PostgreSql)
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn skips_input_it_cannot_safely_reformat() {
        for (sql, dialect) in [
            ("SELECT  'unfinished", Dialect::PostgreSql),
            ("SELECT  id FROM", Dialect::PostgreSql),
            ("COPY  t FROM STDIN;\na  b\n\\.\n", Dialect::PostgreSql),
            ("SELECT  1 /*!50000 + 1 */", Dialect::MySql),
            ("SELECT  1 /*M!100100 + 1 */", Dialect::MySql),
            (r"SELECT  'a\' b'", Dialect::MySql),
        ] {
            assert!(format(sql, dialect).is_err(), "{sql}");
        }
        assert!(format(&" ".repeat(128 * 1024 + 1), Dialect::MySql).is_err());
    }

    #[test]
    fn rejects_flat_expressions_before_building_a_deep_ast() {
        let sql = format!("SELECT  1{}", "+1".repeat(65_000));
        assert!(format(&sql, Dialect::PostgreSql).is_err());
        assert!(crate::sql_editor::check_syntax(&sql, Dialect::PostgreSql).is_none());
    }
}
