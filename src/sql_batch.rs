//! Ordered, read-only SQL execution into an atomic, bounded-memory report.
use crate::{
    database::DbPool, markdown_export, output_file::AtomicOutput, settings::ConnectionProfile,
    sql_session::SqlSession, transfer::Cancellation,
};
use sqlparser::{
    dialect::{Dialect, MySqlDialect, PostgreSqlDialect},
    tokenizer::{Token, Tokenizer, Whitespace},
};
use std::{
    io::{BufWriter, Write},
    path::Path,
    sync::Arc,
    time::{Duration, Instant},
};

/// Keep executable comments whole while splitting. The session guard still
/// examines their executable content before authorizing each query.
#[derive(Debug)]
struct MySqlSplitter;
impl Dialect for MySqlSplitter {
    fn dialect(&self) -> std::any::TypeId {
        MySqlDialect {}.dialect()
    }
    fn is_identifier_start(&self, ch: char) -> bool {
        MySqlDialect {}.is_identifier_start(ch)
    }
    fn is_identifier_part(&self, ch: char) -> bool {
        MySqlDialect {}.is_identifier_part(ch)
    }
    fn is_delimited_identifier_start(&self, ch: char) -> bool {
        MySqlDialect {}.is_delimited_identifier_start(ch)
    }
    fn supports_string_literal_backslash_escape(&self) -> bool {
        true
    }
    fn ignores_wildcard_escapes(&self) -> bool {
        true
    }
    fn supports_numeric_prefix(&self) -> bool {
        true
    }
}

/// Use token spans, not reconstructed tokens: preserve literals and original SQL.
pub fn split(sql: &str, mysql: bool) -> Vec<String> {
    let dialect: &dyn Dialect = if mysql {
        &MySqlSplitter
    } else {
        &PostgreSqlDialect {}
    };
    let mut tokens = Vec::new();
    // A lexical error leaves the ambiguous remainder as one failing query.
    let error = Tokenizer::new(dialect, sql)
        .tokenize_with_location_into_buf(&mut tokens)
        .is_err();
    let mut lines = vec![0];
    lines.extend(sql.match_indices('\n').map(|(offset, _)| offset + 1));
    let mut start = 0;
    let mut meaningful = false;
    let mut statements = Vec::new();
    for token in tokens {
        match token.token {
            Token::SemiColon => {
                let location = token.span.start;
                let offset = lines
                    .get(location.line.saturating_sub(1) as usize)
                    .and_then(|&line| {
                        sql[line..]
                            .char_indices()
                            .nth(location.column.saturating_sub(1) as usize)
                            .map(|(offset, _)| line + offset)
                    });
                let Some(offset) = offset
                    .filter(|&offset| offset >= start && sql.as_bytes().get(offset) == Some(&b';'))
                else {
                    // Preserve uncertain boundaries; the single-statement guard decides.
                    return vec![sql.to_owned()];
                };
                if meaningful {
                    statements.push(sql[start..offset + 1].trim().to_owned());
                }
                start = offset + 1;
                meaningful = false;
            }
            Token::Whitespace(Whitespace::MultiLineComment(comment))
                if mysql && (comment.starts_with('!') || comment.starts_with("M!")) =>
            {
                meaningful = true
            }
            Token::Whitespace(_) => {}
            _ => meaningful = true,
        }
    }
    if meaningful || error {
        let tail = sql[start..].trim();
        if !tail.is_empty() {
            statements.push(tail.to_owned());
        }
    }
    statements
}

#[derive(Debug)]
pub struct Summary {
    pub succeeded: usize,
    pub failed: usize,
    pub skipped: usize,
    pub stopped: bool,
}

#[cfg(test)]
pub async fn run(
    pool: DbPool,
    profile: &ConnectionProfile,
    statements: &[String],
    path: &Path,
    timeout_seconds: u64,
    cancel: Arc<Cancellation>,
    progress: impl Fn(usize, usize),
) -> Result<Summary, String> {
    run_with_schema(
        pool,
        profile,
        statements,
        path,
        timeout_seconds,
        cancel,
        progress,
        None,
    )
    .await
}

pub async fn run_with_schema(
    pool: DbPool,
    profile: &ConnectionProfile,
    statements: &[String],
    path: &Path,
    timeout_seconds: u64,
    cancel: Arc<Cancellation>,
    progress: impl Fn(usize, usize),
    schema: Option<String>,
) -> Result<Summary, String> {
    let mut profile = profile.clone();
    profile.read_only = true;
    let session = SqlSession::with_profile_and_schema(pool, &profile, schema);
    let result = write_report(
        &session,
        &profile,
        statements,
        path,
        timeout_seconds,
        &cancel,
        progress,
    )
    .await;
    session.close().await;
    result.map_err(|e| format!("Не удалось сохранить Markdown-отчёт: {e}"))
}

async fn write_report(
    session: &SqlSession,
    profile: &ConnectionProfile,
    statements: &[String],
    path: &Path,
    timeout_seconds: u64,
    cancel: &Cancellation,
    progress: impl Fn(usize, usize),
) -> Result<Summary, String> {
    let mut output = AtomicOutput::new(path)?;
    let mut out = BufWriter::new(&mut output.file);
    let io = |e: std::io::Error| e.to_string();
    writeln!(out, "# Результаты SQL-исследования\n").map_err(io)?;
    markdown_export::fenced(&mut out, "text", &format!("Подключение: {}\nБаза: {}\nЗапросов: {}\nЛимит: 1000 строк, 128 столбцов, 1024 символа на ячейку; действует лимит объёма предпросмотра.\nКаждый запрос выполняется в отдельной транзакции чтения; общего снимка данных нет.", profile.name, profile.database, statements.len())).map_err(io)?;
    if let Some(schema) = session.schema() {
        markdown_export::fenced(&mut out, "text", &format!("Схема: {schema}")).map_err(io)?;
    }
    let mut summary = Summary {
        succeeded: 0,
        failed: 0,
        skipped: 0,
        stopped: false,
    };
    let mut reason = None;
    for (index, sql) in statements.iter().enumerate() {
        if cancel.is_cancelled() {
            reason = Some("Остановлено пользователем");
            break;
        }
        progress(index + 1, statements.len());
        let started = Instant::now();
        let query = Box::pin(session.execute_with_timeout(sql, timeout_seconds));
        let cancellation = Box::pin(async {
            while !cancel.is_cancelled() {
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        });
        let result = match futures_util::future::select(query, cancellation).await {
            futures_util::future::Either::Left((result, _)) => result,
            futures_util::future::Either::Right((_, query)) => {
                session.request_cancel();
                query.await
            }
        };
        let result = result.map_err(String::from);
        if result.is_ok() {
            summary.succeeded += 1;
        } else {
            summary.failed += 1;
        }
        markdown_export::query(
            &mut out,
            index + 1,
            sql,
            &result,
            started.elapsed().as_millis(),
        )
        .map_err(io)?;
        if cancel.is_cancelled() {
            reason = Some("Остановлено пользователем");
            break;
        }
        if session.is_closed() {
            reason = Some("SQL-сессия потеряна; пакет остановлен без переподключения");
            break;
        }
    }
    summary.skipped = statements.len() - summary.succeeded - summary.failed;
    summary.stopped = reason.is_some();
    if let Some(reason) = reason {
        writeln!(out, "## Выполнение остановлено\n\n{reason}.\n").map_err(io)?;
        for (index, sql) in statements
            .iter()
            .enumerate()
            .skip(summary.succeeded + summary.failed)
        {
            writeln!(out, "### {}. Не выполнен\n", index + 1).map_err(io)?;
            markdown_export::fenced(&mut out, "sql", sql).map_err(io)?;
        }
    }
    writeln!(
        out,
        "## Итог\n\nУспешно: {}. Ошибок: {}. Не выполнено: {}.",
        summary.succeeded, summary.failed, summary.skipped
    )
    .map_err(io)?;
    out.flush().map_err(io)?;
    drop(out);
    // Cancellation stops queries, but intentionally publishes the partial report.
    output.commit(&Cancellation::new())?;
    Ok(summary)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn split_preserves_sql_and_ignores_quoted_semicolons() {
        let sql = "-- начало;\r\nSELECT 'Привет; мир', $$a;b$$; /* ; */ SELECT 2; -- end";
        let parts = split(sql, false);
        assert_eq!(
            parts,
            vec![
                "-- начало;\r\nSELECT 'Привет; мир', $$a;b$$;",
                "/* ; */ SELECT 2;"
            ]
        );
        assert_eq!(split("SELECT 'a;'; SELECT FROM; SELECT 3", false).len(), 3);
        assert_eq!(split("; -- comment\n /* ; */ ;", false).len(), 0);
        assert_eq!(
            split("SELECT 1; SELECT 'unterminated; SELECT 3;", false).len(),
            2
        );
        assert_eq!(
            split("# comment;\nSELECT `a;b`, 'c;d'; SELECT 2", true).len(),
            2
        );
        assert_eq!(
            split("SELECT E'a\\\';b'; SELECT $tag$x;y$tag$;", false).len(),
            2
        );
        assert_eq!(
            split("/*!80000 SELECT 1; SELECT 2 */; SELECT 3;", true),
            vec!["/*!80000 SELECT 1; SELECT 2 */;", "SELECT 3;"]
        );
        assert_eq!(split("/*M! SELECT 1; */; SELECT 3;", true).len(), 2);
    }

    #[test]
    #[ignore = "requires local Compose PostgreSQL, MariaDB and MySQL"]
    fn sql_batch_reports_all_engines() {
        tokio::runtime::Runtime::new().unwrap().block_on(async {
            for (kind, port, key) in [
                (crate::settings::DatabaseKind::PostgreSql, 55432, "DBDECK_PG_PASSWORD"),
                (crate::settings::DatabaseKind::MariaDb, 53306, "DBDECK_MARIADB_PASSWORD"),
                (crate::settings::DatabaseKind::MySql, 53307, "DBDECK_MYSQL_PASSWORD"),
            ] {
                let profile: ConnectionProfile = serde_json::from_value(serde_json::json!({
                    "id":"batch-test", "name":"batch-test", "kind":kind, "host":"127.0.0.1", "port":port,
                    "database":"dbdeck_test", "username":"dbdeck_test", "environment":"local",
                    "color":"#ffffff", "read_only":false, "tls_mode":"disabled"
                })).unwrap();
                let pool = DbPool::connect(&profile, &std::env::var(key).expect("fixture password from .env")).await.unwrap();
                let mysql = !matches!(kind, crate::settings::DatabaseKind::PostgreSql);
                let root = std::env::temp_dir().join(format!("dbdeck-batch-{}", uuid::Uuid::new_v4()));
                std::fs::create_dir(&root).unwrap();
                let path = root.join("report.md");
                let long = "SELECT REPEAT('x', 1100) AS long_value";
                let many = if mysql {
                    "SELECT a.id FROM sample_records a CROSS JOIN sample_records b CROSS JOIN sample_records c LIMIT 1100"
                } else { "SELECT generate_series(1,1100) AS id" };
                let statements = split(&format!("SELECT 'Привет; ``` | <b>' AS value, NULL AS missing, '' AS empty_value; SELECT missing_batch_column FROM sample_records; SELECT FROM; DELETE FROM sample_records WHERE 1=0; BEGIN; SELECT 2 AS value WHERE 1=0; {long}; {many}; SELECT 42 AS final_answer;"), mysql);
                let mut order = Vec::new();
                let summary = run(pool.clone(), &profile, &statements, &path, 5, Arc::new(Cancellation::new()), |_, _| {}).await.unwrap();
                assert_eq!((summary.succeeded, summary.failed, summary.skipped), (5,4,0), "{kind:?}");
                assert!(!summary.stopped);
                let report = std::fs::read_to_string(&path).unwrap();
                for index in 1..=9 { order.push(report.find(&format!("## {index}. ")).unwrap()); }
                assert!(order.windows(2).all(|w| w[0] < w[1]));
                for expected in ["&#124;", "&lt;b&gt;", "**NULL**", "*(пустая строка)*", "Строк: 0", "[обрезано]", "Сохранено: 1000", "| 42 |", "Только чтение"] {
                    assert!(report.contains(expected), "{kind:?}: missing {expected}");
                }
                use std::os::unix::fs::PermissionsExt;
                assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);

                if mysql {
                    let summary = run(pool.clone(), &profile,
                        &split("/*!80000 SELECT 1; SELECT 2 */; SELECT 3 AS after_comment;", true),
                        &path, 5, Arc::new(Cancellation::new()), |_, _| {}).await.unwrap();
                    assert_eq!((summary.succeeded, summary.failed, summary.skipped), (1, 1, 0));
                    assert!(std::fs::read_to_string(&path).unwrap().contains("| 3 |"));
                }

                let cancel = Arc::new(Cancellation::new());
                cancel.request();
                let summary = run(pool.clone(), &profile, &statements, &path, 5, cancel, |_, _| panic!("cancelled batch executed")).await.unwrap();
                assert!(summary.stopped && summary.skipped == statements.len());

                let slow = if mysql { "SELECT SLEEP(5)" } else { "SELECT pg_sleep(5)" };
                let statements = split(&format!("SELECT 1; {slow}; SELECT 3;"), mysql);
                let summary = run(pool.clone(), &profile, &statements, &path, 1, Arc::new(Cancellation::new()), |_, _| {}).await.unwrap();
                assert_eq!((summary.succeeded, summary.failed, summary.skipped), (2,1,0), "timeout {kind:?}");
                assert!(std::fs::read_to_string(&path).unwrap().contains("Превышено время"));

                let cancel = Arc::new(Cancellation::new());
                let stop = cancel.clone();
                let summary = run(pool.clone(), &profile, &statements, &path, 10, cancel, |index, _| {
                    if index == 2 { assert!(stop.request()); }
                }).await.unwrap();
                assert!(summary.stopped);
                assert_eq!(summary.skipped, 1);
                let report = std::fs::read_to_string(&path).unwrap();
                assert!(report.contains("### 3. Не выполнен") && report.contains("Остановлено пользователем"));

                // Cancellation during an already running server query.
                let cancel = Arc::new(Cancellation::new());
                let stop = cancel.clone();
                let task = tokio::spawn(async move {
                    tokio::time::sleep(Duration::from_millis(400)).await;
                    stop.request();
                });
                let summary = run(pool.clone(), &profile, &statements, &path, 10, cancel, |_, _| {}).await.unwrap();
                task.await.unwrap();
                assert!(summary.stopped && summary.skipped == 1);

                // Disconnect only the dedicated connection owned by this test.
                let mut readonly = profile.clone(); readonly.read_only = true;
                let session = SqlSession::with_profile(pool.clone(), &readonly);
                let backend_sql = if mysql { "SELECT CONNECTION_ID()" } else { "SELECT pg_backend_pid()" };
                let backend = session.execute_with_timeout(backend_sql, 5).await.unwrap();
                let crate::database::CellValue::Text(id) = &backend.data.rows[0][0] else { panic!("backend ID"); };
                let id: u64 = id.parse().unwrap();
                match &pool {
                    DbPool::PostgreSql(p) => { sqlx::query("SELECT pg_terminate_backend($1)").bind(id as i32).execute(p).await.unwrap(); }
                    DbPool::MySql(p) => { sqlx::query(&format!("KILL CONNECTION {id}")).execute(p).await.unwrap(); }
                }
                let summary = write_report(&session, &readonly, &statements, &path, 5, &Cancellation::new(), |_, _| {}).await.unwrap();
                assert!(summary.stopped && summary.failed == 1 && summary.skipped == 2, "lost session {kind:?}: {summary:?}");
                session.close().await;

                // A file write/publication failure must preserve the destination.
                std::fs::create_dir(root.join("destination-directory")).unwrap();
                assert!(run(pool.clone(), &profile, &split("SELECT 1", mysql), &root.join("destination-directory"), 5,
                    Arc::new(Cancellation::new()), |_, _| {}).await.is_err());
                assert!(root.join("destination-directory").is_dir());
                assert!(std::fs::read_dir(&root).unwrap().all(|entry| !entry.unwrap().file_name().to_string_lossy().ends_with(".tmp")));
                println!("{kind:?}: ordered report, syntax/server errors, write guards, NULL/empty, clipping, timeout, cancellation, connection loss, atomic output passed; {}", path.display());
                match pool { DbPool::PostgreSql(p) => p.close().await, DbPool::MySql(p) => p.close().await }
            }
        });
    }
}
