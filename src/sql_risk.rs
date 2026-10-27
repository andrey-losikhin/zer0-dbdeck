#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dialect {
    PostgreSql,
    MySql,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SqlRisk {
    Empty,
    ReadOnly,
    ChangesData,
    Unknown,
}

pub fn analyze(sql: &str, dialect: Dialect) -> SqlRisk {
    let tokens = tokenize(sql, dialect);
    let mut result = SqlRisk::Empty;
    for statement in tokens.split(|token| token == ";").filter(|s| !s.is_empty()) {
        match statement_risk(statement, 0) {
            SqlRisk::ChangesData => return SqlRisk::ChangesData,
            SqlRisk::Unknown => result = SqlRisk::Unknown,
            SqlRisk::ReadOnly if result == SqlRisk::Empty => result = SqlRisk::ReadOnly,
            _ => {}
        }
    }
    result
}

fn is_mutating(word: &str) -> bool {
    matches!(
        word,
        "INSERT"
            | "UPDATE"
            | "DELETE"
            | "MERGE"
            | "REPLACE"
            | "UPSERT"
            | "CREATE"
            | "ALTER"
            | "DROP"
            | "TRUNCATE"
            | "RENAME"
            | "COMMENT"
            | "GRANT"
            | "REVOKE"
            | "REFRESH"
            | "COPY"
            | "LOAD"
            | "DO"
            | "CALL"
            | "EXEC"
            | "EXECUTE"
    )
}

fn side_effect_call(tokens: &[String]) -> bool {
    tokens.windows(2).any(|pair| {
        let name = pair[0].strip_prefix("<quoted>:").unwrap_or(&pair[0]);
        pair[1] == "("
            && (name.starts_with("PG_ADVISORY_")
                || name.starts_with("PG_TRY_ADVISORY_")
                || matches!(
                    name,
                    "GET_LOCK"
                        | "RELEASE_LOCK"
                        | "RELEASE_ALL_LOCKS"
                        | "SET_CONFIG"
                        | "PG_NOTIFY"
                        | "PG_CANCEL_BACKEND"
                        | "PG_TERMINATE_BACKEND"
                        | "PG_RELOAD_CONF"
                        | "PG_ROTATE_LOGFILE"
                        | "PG_SWITCH_WAL"
                        | "PG_PROMOTE"
                        | "LO_IMPORT"
                        | "LO_EXPORT"
                        | "DBLINK_EXEC"
                        | "PG_LOGICAL_EMIT_MESSAGE"
                        | "NEXTVAL"
                        | "SETVAL"
                        | "LOAD_FILE"
                        | "PG_READ_FILE"
                        | "PG_READ_BINARY_FILE"
                        | "PG_LS_DIR"
                        | "PG_STAT_FILE"
                ))
    })
}

fn statement_risk(tokens: &[String], depth: usize) -> SqlRisk {
    let Some(first) = tokens.first().map(String::as_str) else {
        return SqlRisk::Empty;
    };
    if depth > 64 {
        return SqlRisk::Unknown;
    }
    if is_mutating(first) {
        return SqlRisk::ChangesData;
    }
    if first == "EXPLAIN" {
        let command = tokens.iter().enumerate().skip(1).find(|(_, token)| {
            matches!(token.as_str(), "SELECT" | "WITH" | "VALUES" | "TABLE") || is_mutating(token)
        });
        let Some((index, _)) = command else {
            return SqlRisk::Unknown;
        };
        let options = &tokens[1..index];
        let executes = options.iter().enumerate().any(|(i, token)| {
            if token != "ANALYZE" {
                return false;
            }
            let next = i + 1 + usize::from(options.get(i + 1).is_some_and(|s| s == "="));
            !options
                .get(next)
                .is_some_and(|s| matches!(s.as_str(), "FALSE" | "OFF" | "0"))
        });
        return if executes {
            statement_risk(&tokens[index..], depth + 1)
        } else {
            SqlRisk::ReadOnly
        };
    }
    if side_effect_call(tokens) {
        return SqlRisk::ChangesData;
    }
    if first == "WITH" {
        return with_risk(tokens, depth + 1);
    }
    if first == "SELECT" {
        return select_risk(tokens);
    }
    if matches!(first, "SHOW" | "DESCRIBE" | "DESC" | "VALUES" | "TABLE") {
        SqlRisk::ReadOnly
    } else {
        SqlRisk::Unknown
    }
}

fn closing_parenthesis(tokens: &[String], start: usize) -> Option<usize> {
    if tokens.get(start)? != "(" {
        return None;
    }
    let mut depth = 0usize;
    for (index, token) in tokens.iter().enumerate().skip(start) {
        if token == "(" {
            depth += 1;
        }
        if token == ")" {
            depth -= 1;
            if depth == 0 {
                return Some(index);
            }
        }
    }
    None
}

fn with_risk(tokens: &[String], depth: usize) -> SqlRisk {
    let mut index = 1 + usize::from(tokens.get(1).is_some_and(|s| s == "RECURSIVE"));
    let mut unknown = false;
    loop {
        if tokens.get(index).is_none() {
            return SqlRisk::Unknown;
        }
        index += 1; // CTE name, including a quoted identifier.
        if tokens.get(index).is_some_and(|s| s == "(") {
            let Some(end) = closing_parenthesis(tokens, index) else {
                return SqlRisk::Unknown;
            };
            index = end + 1;
        }
        if !tokens.get(index).is_some_and(|s| s == "AS") {
            return SqlRisk::Unknown;
        }
        index += 1;
        if tokens.get(index).is_some_and(|s| s == "NOT") {
            index += 1;
        }
        if tokens.get(index).is_some_and(|s| s == "MATERIALIZED") {
            index += 1;
        }
        let Some(end) = closing_parenthesis(tokens, index) else {
            return SqlRisk::Unknown;
        };
        match statement_risk(&tokens[index + 1..end], depth) {
            SqlRisk::ChangesData => return SqlRisk::ChangesData,
            SqlRisk::Unknown | SqlRisk::Empty => unknown = true,
            SqlRisk::ReadOnly => {}
        }
        index = end + 1;
        if tokens.get(index).is_some_and(|s| s == ",") {
            index += 1;
        } else {
            break;
        }
    }
    match statement_risk(&tokens[index..], depth) {
        SqlRisk::ChangesData => SqlRisk::ChangesData,
        SqlRisk::ReadOnly if !unknown => SqlRisk::ReadOnly,
        _ => SqlRisk::Unknown,
    }
}

fn select_risk(tokens: &[String]) -> SqlRisk {
    if tokens.iter().any(|token| token == "INTO")
        || tokens
            .windows(2)
            .any(|p| p[0] == "FOR" && matches!(p[1].as_str(), "UPDATE" | "SHARE"))
        || tokens
            .windows(4)
            .any(|p| p == ["LOCK", "IN", "SHARE", "MODE"])
    {
        SqlRisk::ChangesData
    } else {
        SqlRisk::ReadOnly
    }
}

// A lexer, not a SQL validator: comments, strings and quoted identifiers must
// never contribute mutation keywords (including PostgreSQL dollar strings).
fn tokenize(sql: &str, dialect: Dialect) -> Vec<String> {
    let chars: Vec<char> = sql.chars().collect();
    let mut tokens = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        let ch = chars[i];
        let dash_comment = ch == '-'
            && chars.get(i + 1) == Some(&'-')
            && (dialect == Dialect::PostgreSql
                || chars.get(i + 2).is_none_or(|ch| ch.is_whitespace()));
        if dash_comment || (ch == '#' && dialect == Dialect::MySql) {
            while i < chars.len() && chars[i] != '\n' {
                i += 1;
            }
        } else if ch == '/' && chars.get(i + 1) == Some(&'*') {
            let executable = dialect == Dialect::MySql
                && (chars.get(i + 2) == Some(&'!')
                    || (chars.get(i + 2) == Some(&'M') && chars.get(i + 3) == Some(&'!')));
            if executable {
                i += if chars.get(i + 2) == Some(&'M') { 4 } else { 3 };
                while chars.get(i).is_some_and(|ch| ch.is_ascii_digit()) {
                    i += 1;
                }
                let start = i;
                while i < chars.len() && !(chars[i] == '*' && chars.get(i + 1) == Some(&'/')) {
                    i += 1;
                }
                tokens.extend(tokenize(
                    &chars[start..i].iter().collect::<String>(),
                    dialect,
                ));
                i = (i + 2).min(chars.len());
                continue;
            }
            i += 2;
            let mut depth = 1;
            while i < chars.len() && depth > 0 {
                if dialect == Dialect::PostgreSql
                    && chars[i] == '/'
                    && chars.get(i + 1) == Some(&'*')
                {
                    depth += 1;
                    i += 2;
                } else if chars[i] == '*' && chars.get(i + 1) == Some(&'/') {
                    depth -= 1;
                    i += 2;
                } else {
                    i += 1;
                }
            }
        } else if matches!(ch, '\'' | '"' | '`') {
            let token_index = tokens.len();
            tokens.push("<quoted>".to_owned());
            let escaped = (dialect == Dialect::MySql && ch != '`')
                || (ch == '\''
                    && i > 0
                    && matches!(chars[i - 1], 'E' | 'e')
                    && (i < 2 || !(chars[i - 2].is_alphanumeric() || chars[i - 2] == '_')));
            i += 1;
            let quoted_start = i;
            while i < chars.len() {
                if escaped && chars[i] == '\\' {
                    i = (i + 2).min(chars.len());
                } else if chars[i] == ch {
                    i += 1;
                    if chars.get(i) == Some(&ch) {
                        i += 1;
                    } else {
                        break;
                    }
                } else {
                    i += 1;
                }
            }
            // Quoted names cannot become SQL keywords, but a function call
            // must retain its name (e.g. pg_catalog."pg_try_advisory_lock"()).
            tokens[token_index] = format!(
                "<quoted>:{}",
                chars[quoted_start..i.saturating_sub(1).max(quoted_start)]
                    .iter()
                    .collect::<String>()
                    .to_ascii_uppercase()
            );
        } else if ch == '$' && dialect == Dialect::PostgreSql {
            let start = i;
            i += 1;
            while i < chars.len() && (chars[i].is_ascii_alphanumeric() || chars[i] == '_') {
                i += 1;
            }
            if chars.get(i) == Some(&'$') {
                let tag = chars[start..=i].to_vec();
                i += 1;
                while i < chars.len() && !chars[i..].starts_with(&tag) {
                    i += 1;
                }
                i = (i + tag.len()).min(chars.len());
                tokens.push("<quoted>".to_owned());
            }
        } else if ch.is_alphanumeric() || ch == '_' {
            let start = i;
            i += 1;
            while i < chars.len() && (chars[i].is_alphanumeric() || matches!(chars[i], '_' | '$')) {
                i += 1;
            }
            tokens.push(
                chars[start..i]
                    .iter()
                    .collect::<String>()
                    .to_ascii_uppercase(),
            );
        } else {
            if !ch.is_whitespace() {
                tokens.push(ch.to_string());
            }
            i += 1;
        }
    }
    tokens
}

#[cfg(test)]
mod tests {
    #[test]
    fn session_side_effects_need_write_permission() {
        use super::*;
        for sql in [
            "SELECT GET_LOCK('x', 0)",
            "SELECT pg_catalog.\"pg_try_advisory_lock\"(42)",
            "SELECT `GET_LOCK`('x',0)",
            "VALUES (pg_notify('x','y'))",
            "SELECT set_config('work_mem','1GB',false)",
            "SELECT LOAD_FILE('/tmp/x')",
            "SELECT * FROM t FOR UPDATE",
            "SELECT * FROM t LOCK IN SHARE MODE",
        ] {
            assert_eq!(
                super::analyze(sql, Dialect::PostgreSql),
                SqlRisk::ChangesData,
                "{sql}"
            );
        }
        for sql in [
            "SELECT 'GET_LOCK(x)' AS value",
            r#"SELECT "UPDATE" FROM t"#,
            "EXPLAIN SELECT GET_LOCK('x',0)",
            "SELECT lower('value')",
        ] {
            assert_eq!(
                super::analyze(sql, Dialect::PostgreSql),
                SqlRisk::ReadOnly,
                "{sql}"
            );
        }
    }

    use super::*;
    fn analyze(sql: &str) -> SqlRisk {
        super::analyze(sql, Dialect::PostgreSql)
    }

    #[test]
    fn cte_and_explain_functions_are_not_write_commands() {
        for sql in [
            "WITH x AS (SELECT replace(name, 'a', 'b') FROM t) SELECT * FROM x",
            "WITH x(id) AS NOT MATERIALIZED (SELECT 1), y AS (SELECT 2) SELECT * FROM x",
            "EXPLAIN ANALYZE SELECT replace(name, 'a', 'b') FROM t",
            "EXPLAIN (ANALYZE = false) DELETE FROM t",
        ] {
            assert_eq!(analyze(sql), SqlRisk::ReadOnly, "{sql}");
        }
        assert_eq!(
            analyze(
                "WITH x AS (SELECT 1), changed AS (UPDATE t SET x=1 RETURNING *) SELECT * FROM changed"
            ),
            SqlRisk::ChangesData
        );
        assert_eq!(
            analyze(
                "EXPLAIN (ANALYZE true) WITH changed AS (DELETE FROM t RETURNING *) SELECT * FROM changed"
            ),
            SqlRisk::ChangesData
        );
    }

    #[test]
    fn comments_and_escapes_follow_the_connection_dialect() {
        assert_eq!(
            analyze("SELECT data #>> '{name}' FROM t; DELETE FROM t"),
            SqlRisk::ChangesData
        );
        assert_eq!(
            super::analyze("# DELETE FROM t\nSELECT 1", Dialect::MySql),
            SqlRisk::ReadOnly
        );
        assert_eq!(
            super::analyze("/*!50000 UPDATE t SET x=1 */", Dialect::MySql),
            SqlRisk::ChangesData
        );
        assert_eq!(
            super::analyze("/*M!100100 DELETE FROM t */", Dialect::MySql),
            SqlRisk::ChangesData
        );
        assert_eq!(
            analyze("/*!50000 UPDATE t SET x=1 */ SELECT 1"),
            SqlRisk::ReadOnly
        );
        assert_eq!(analyze(r"SELECT '\'; DELETE FROM t"), SqlRisk::ChangesData);
        assert_eq!(
            super::analyze(r"SELECT 'a\' DELETE FROM t'", Dialect::MySql),
            SqlRisk::ReadOnly
        );
    }

    #[test]
    fn garbage_and_unknown_commands_are_not_mutation_warnings() {
        for sql in [
            "фвавыа",
            "asdf",
            "update_me",
            "nonsense DELETE",
            "WITH nonsense",
        ] {
            assert_eq!(analyze(sql), SqlRisk::Unknown, "{sql}");
        }
    }
    #[test]
    fn comments_literals_and_identifiers_do_not_trigger_warning() {
        assert_eq!(
            analyze("-- DELETE\n/* DROP /* UPDATE */ */"),
            SqlRisk::Empty
        );
        for sql in [
            "SELECT 'DELETE; UPDATE'",
            "SELECT \"DROP\" FROM t",
            "SELECT `UPDATE` FROM t",
            "SELECT $$DROP TABLE x;$$",
            "SELECT $tag$DELETE$tag$",
            "SELECT 1 /* UPDATE */; SELECT 2",
            "SELECT 'it''s UPDATE'",
        ] {
            assert_eq!(analyze(sql), SqlRisk::ReadOnly, "{sql}");
        }
    }
    #[test]
    fn mutating_statements_scripts_and_ctes_are_detected() {
        for sql in [
            "UPDATE t SET x=1",
            "DELETE FROM t",
            "INSERT INTO t VALUES (1)",
            "DROP TABLE t",
            "TRUNCATE t",
            "SELECT 1; ALTER TABLE t ADD x int",
            "WITH removed AS (DELETE FROM t RETURNING *) SELECT * FROM removed",
            "WITH source AS (SELECT 1) UPDATE t SET x=1",
            "DO $$ BEGIN DELETE FROM t; END $$",
        ] {
            assert_eq!(analyze(sql), SqlRisk::ChangesData, "{sql}");
        }
    }
    #[test]
    fn reads_ctes_and_known_side_effects_are_distinguished() {
        assert_eq!(
            analyze("WITH x AS (SELECT 1) SELECT * FROM x"),
            SqlRisk::ReadOnly
        );
        assert_eq!(analyze("SELECT nextvalue FROM t"), SqlRisk::ReadOnly);
        assert_eq!(analyze("SELECT nextval('seq')"), SqlRisk::ChangesData);
        assert_eq!(
            analyze("SELECT * INTO new_table FROM t"),
            SqlRisk::ChangesData
        );
        assert_eq!(analyze("EXPLAIN UPDATE t SET x=1"), SqlRisk::ReadOnly);
        assert_eq!(
            analyze("EXPLAIN ANALYZE UPDATE t SET x=1"),
            SqlRisk::ChangesData
        );
        assert_eq!(
            analyze("EXPLAIN (ANALYZE FALSE) DELETE FROM t"),
            SqlRisk::ReadOnly
        );
    }
}
