//! Conservative provenance for directly projected, single-table SELECT results.
//! Unsupported SQL stays readable; never infer a write target from matching column names.
#[derive(Debug, PartialEq)]
pub(super) struct Source {
    pub schema: Option<String>,
    pub table: String,
    pub columns: Option<Vec<String>>,
}
#[derive(Debug, PartialEq)]
enum Token {
    Word(String),
    Quoted(String),
    Punct(char),
    Literal,
}
impl Token {
    fn keyword(&self, value: &str) -> bool {
        matches!(self, Self::Word(w) if w.eq_ignore_ascii_case(value))
    }
    fn ident(&self, mysql: bool) -> Option<String> {
        match self {
            Self::Word(w) if w.chars().next()?.is_alphabetic() || w.starts_with('_') => {
                Some(if mysql { w.clone() } else { w.to_lowercase() })
            }
            Self::Quoted(w) => Some(w.clone()),
            _ => None,
        }
    }
}
fn tokens(sql: &str, mysql: bool) -> Option<Vec<Token>> {
    let mut chars = sql.chars().peekable();
    let mut out = Vec::new();
    while let Some(c) = chars.next() {
        if c.is_whitespace() {
            continue;
        }
        if c == '-' && chars.peek() == Some(&'-') {
            chars.next();
            // MySQL requires whitespace after --. Reject the ambiguous spelling.
            if mysql && chars.peek().is_some_and(|c| !c.is_whitespace()) {
                return None;
            }
            for c in chars.by_ref() {
                if c == '\n' {
                    break;
                }
            }
        } else if c == '/' && chars.peek() == Some(&'*') {
            chars.next();
            // Executable comments and nested/dialect-specific comments aren't provenance.
            if matches!(chars.peek(), Some('!' | '+')) {
                return None;
            }
            let mut closed = false;
            while let Some(c) = chars.next() {
                if c == '/' && chars.peek() == Some(&'*') {
                    return None;
                }
                if c == '*' && chars.peek() == Some(&'/') {
                    chars.next();
                    closed = true;
                    break;
                }
            }
            if !closed {
                return None;
            }
        } else if c == '\'' || c == '"' || c == '`' {
            if (mysql && c == '"') || (!mysql && c == '`') {
                return None;
            }
            let mut value = String::new();
            loop {
                let next = chars.next()?;
                if next == '\\' {
                    return None;
                }
                if next == c {
                    if chars.peek() == Some(&c) {
                        chars.next();
                        value.push(c);
                    } else {
                        break;
                    }
                } else {
                    value.push(next);
                }
            }
            out.push(if c == '\'' {
                Token::Literal
            } else {
                Token::Quoted(value)
            });
        } else if c.is_alphanumeric() || c == '_' {
            let mut value = c.to_string();
            while chars
                .peek()
                .is_some_and(|c| c.is_alphanumeric() || *c == '_')
            {
                value.push(chars.next()?);
            }
            out.push(Token::Word(value));
        } else if c == '$' || c == '#' || c == '\\' {
            return None;
        } else {
            out.push(Token::Punct(c));
        }
    }
    Some(out)
}
pub(super) fn source(sql: &str, mysql: bool) -> Option<Source> {
    let mut t = tokens(sql, mysql)?;
    if t.last() == Some(&Token::Punct(';')) {
        t.pop();
    }
    if t.iter().any(|t| *t == Token::Punct(';')) || !t.first()?.keyword("select") {
        return None;
    }
    let from = t.iter().position(|t| t.keyword("from"))?;
    let mut i = from + 1;
    if t.get(i)?.keyword("only") {
        return None;
    }
    let first = t.get(i)?.ident(mysql)?;
    i += 1;
    let (schema, table) = if t.get(i) == Some(&Token::Punct('.')) {
        i += 1;
        let table = t.get(i)?.ident(mysql)?;
        i += 1;
        (Some(first), table)
    } else {
        (None, first)
    };
    let mut alias = None;
    if t.get(i).is_some_and(|t| t.keyword("as")) {
        i += 1;
        alias = Some(t.get(i)?.ident(mysql)?);
        i += 1;
    } else if t.get(i).is_some_and(|t| {
        !["where", "order", "limit", "offset"]
            .iter()
            .any(|w| t.keyword(w))
    }) && t.get(i).and_then(|t| t.ident(mysql)).is_some()
    {
        alias = Some(t.get(i)?.ident(mysql)?);
        i += 1;
    }
    if i < t.len()
        && !["where", "order", "limit", "offset"]
            .iter()
            .any(|w| t[i].keyword(w))
    {
        return None;
    }
    let mut depth = 0usize;
    for token in &t[i..] {
        match token {
            Token::Punct('(') => depth += 1,
            Token::Punct(')') => depth = depth.checked_sub(1)?,
            Token::Punct(',') if depth == 0 && !t[i..].iter().any(|t| t.keyword("order")) => {
                return None;
            }
            _ if depth == 0
                && [
                    "join",
                    "inner",
                    "left",
                    "right",
                    "cross",
                    "natural",
                    "full",
                    "union",
                    "intersect",
                    "except",
                    "group",
                    "having",
                    "window",
                    "for",
                    "into",
                    "procedure",
                    "lock",
                    "returning",
                    "from",
                    "select",
                    "distinct",
                ]
                .iter()
                .any(|w| token.keyword(w)) =>
            {
                return None;
            }
            _ => {}
        }
    }
    if depth != 0 {
        return None;
    }
    let mut columns = Vec::new();
    let mut wildcard = false;
    for item in t[1..from].split(|t| *t == Token::Punct(',')) {
        let column = match item {
            [Token::Punct('*')] => {
                wildcard = true;
                None
            }
            [name] => Some(name.ident(mysql)?),
            [qualifier, Token::Punct('.'), Token::Punct('*')]
                if qualifier.ident(mysql).as_ref() == Some(alias.as_ref().unwrap_or(&table)) =>
            {
                wildcard = true;
                None
            }
            [qualifier, Token::Punct('.'), name]
                if qualifier.ident(mysql).as_ref() == Some(alias.as_ref().unwrap_or(&table)) =>
            {
                Some(name.ident(mysql)?)
            }
            _ => return None,
        };
        if let Some(column) = column {
            columns.push(column);
        }
    }
    if wildcard && !columns.is_empty() {
        return None;
    }
    if !wildcard
        && (columns.is_empty()
            || columns
                .iter()
                .collect::<std::collections::HashSet<_>>()
                .len()
                != columns.len())
    {
        return None;
    }
    Some(Source {
        schema,
        table,
        columns: (!wildcard).then_some(columns),
    })
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn direct_projection_and_complex_filter_have_one_target() {
        let parsed=source("-- demo\nSELECT s.id, s.name FROM public.sample_records AS s WHERE id != 4 AND (note IS NULL OR id IN (SELECT id FROM other_table)) ORDER BY id DESC LIMIT 20;",false).unwrap();
        assert_eq!(parsed.schema.as_deref(), Some("public"));
        assert_eq!(parsed.columns, Some(vec!["id".into(), "name".into()]));
        assert!(
            source(
                "SELECT `t`.* FROM `db`.`table` t WHERE name = 'a; JOIN b'",
                true
            )
            .is_some()
        );
        assert_eq!(
            source("SELECT * FROM \"CaseTable\"", false).unwrap().table,
            "CaseTable"
        );
    }
    #[test]
    fn ambiguous_or_computed_results_never_get_write_targets() {
        for sql in [
            "SELECT count(*) FROM t",
            "SELECT DISTINCT id FROM t",
            "SELECT id AS name FROM t",
            "SELECT id + 1 FROM t",
            "SELECT * FROM t JOIN u ON t.id=u.id",
            "SELECT * FROM t,u ORDER BY id",
            "SELECT * FROM t UNION SELECT * FROM u",
            "SELECT * FROM t GROUP BY id",
            "WITH x AS (SELECT * FROM t) SELECT * FROM x",
            "SELECT * FROM t; SELECT * FROM u",
            "SELECT * FROM (SELECT * FROM t) x",
            "SELECT id, id FROM t",
            "SELECT * FROM t FOR UPDATE",
            "SELECT * FROM ONLY t",
            "SELECT t.* FROM ONLY t",
            "SELECT * FROM t /*! UNION SELECT * FROM u */",
        ] {
            assert!(source(sql, false).is_none(), "{sql}");
            assert!(source(sql, true).is_none(), "{sql}");
        }
    }
}
