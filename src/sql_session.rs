//! An SQL tab owns its connection. Session state is never returned to a shared pool.
use crate::database::{
    DbPool, QueryConnection, QueryResult, configure_pg_connection,
    execute_on_connection_controlled, stream_query_csv,
};
use crate::db_error::{DbError, DbErrorKind, classify};
use crate::transfer::Cancellation;
use sqlparser::{
    dialect::{MySqlDialect, PostgreSqlDialect},
    keywords::Keyword,
    tokenizer::{Token, Tokenizer},
};
use sqlx::Connection as _;
use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicU64, Ordering},
};
use std::time::Duration;
use tokio::sync::{Mutex, Notify};
use tokio::time::Instant;

const POSTGRES_IDLE_AFTER: Duration = Duration::from_secs(60);
const POSTGRES_PROBE_TIMEOUT: Duration = Duration::from_secs(5);
const POSTGRES_RECONNECT_TIMEOUT: Duration = Duration::from_secs(8);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TransactionAction {
    Manual,
    AutoCommit,
    Commit,
    Rollback,
}

#[derive(Clone, Copy, Default, Debug, PartialEq, Eq)]
pub struct TransactionState {
    pub manual: bool,
    pub pending: bool,
    pub failed: bool,
    pub read_only: bool,
}

/// Session-level errors keep the raw sqlx text (unlike pool-level ones).
fn raw_error(error: &sqlx::Error) -> DbError {
    DbError {
        kind: classify(error),
        message: error.to_string(),
    }
}

enum Connection {
    PostgreSql(sqlx::PgConnection),
    MySql(sqlx::MySqlConnection),
}
impl Connection {
    fn query_connection(&mut self) -> QueryConnection<'_> {
        match self {
            Self::PostgreSql(c) => QueryConnection::PostgreSql(c),
            Self::MySql(c) => QueryConnection::MySql(c),
        }
    }
    fn command<'a>(
        &'a mut self,
        sql: &'a str,
    ) -> futures_util::future::BoxFuture<'a, Result<(), DbError>> {
        Box::pin(async move {
            let result = match self {
                Self::PostgreSql(c) => sqlx::Executor::execute(&mut *c, sql).await.map(|_| ()),
                Self::MySql(c) => sqlx::Executor::execute(&mut *c, sql).await.map(|_| ()),
            };
            result.map_err(|e| raw_error(&e))
        })
    }
}
#[derive(Default)]
struct Inner {
    connection: Option<Connection>,
    state: TransactionState,
    last_activity: Option<Instant>,
}
#[derive(Clone)]
pub struct SqlSession {
    pool: DbPool,
    inner: Arc<Mutex<Inner>>,
    pending: Arc<AtomicBool>,
    closed: Arc<AtomicBool>,
    backend_id: Arc<AtomicU64>,
    revision: Arc<AtomicU64>,
    cancel_requested: Arc<AtomicBool>,
    cancel_notify: Arc<Notify>,
    read_only: bool,
    production: bool,
    schema: Option<String>,
}
impl SqlSession {
    pub fn new(pool: DbPool) -> Self {
        Self {
            pool,
            inner: Arc::new(Mutex::new(Inner::default())),
            pending: Arc::new(AtomicBool::new(false)),
            closed: Arc::new(AtomicBool::new(false)),
            backend_id: Arc::new(AtomicU64::new(0)),
            revision: Arc::new(AtomicU64::new(0)),
            cancel_requested: Arc::new(AtomicBool::new(false)),
            cancel_notify: Arc::new(Notify::new()),
            read_only: false,
            production: false,
            schema: None,
        }
    }
    pub fn with_profile(pool: DbPool, profile: &crate::settings::ConnectionProfile) -> Self {
        Self {
            read_only: profile.read_only,
            production: matches!(profile.environment, crate::settings::EnvironmentTag::Prod),
            ..Self::new(pool)
        }
    }
    pub fn with_profile_and_schema(
        pool: DbPool,
        profile: &crate::settings::ConnectionProfile,
        schema: Option<String>,
    ) -> Self {
        Self {
            schema: if matches!(profile.kind, crate::settings::DatabaseKind::PostgreSql) {
                schema
            } else {
                None
            },
            ..Self::with_profile(pool, profile)
        }
    }
    pub fn isolated_read_only(&self) -> Self {
        // Starting a read-only transaction must leave the editor's connection alone.
        Self {
            read_only: true,
            schema: self.schema.clone(),
            ..Self::new(self.pool.clone())
        }
    }
    pub fn validate_csv_query(&self, sql: &str) -> Result<(), String> {
        let mysql = matches!(self.pool, DbPool::MySql(_));
        let policy = statement_policy(sql, mysql)?;
        validate_read_only(sql_risk(sql, mysql), &policy)
    }

    pub async fn export_csv(
        &self,
        sql: &str,
        timeout_seconds: u64,
        file: &mut std::fs::File,
        cancel: &Cancellation,
        progress: impl FnMut(usize) + Send,
    ) -> Result<usize, String> {
        use futures_util::future::{Either, select};

        self.validate_csv_query(sql)?;
        let session = self.isolated_read_only();
        let mut inner = Inner::default();
        let export = async {
            session
                .connect(&mut inner)
                .await
                .map_err(|e| e.to_string())?;
            let connection = inner.connection.as_mut().unwrap();
            let begin = match connection {
                Connection::PostgreSql(_) => "BEGIN READ ONLY",
                Connection::MySql(_) => "START TRANSACTION READ ONLY",
            };
            connection.command(begin).await.map_err(|e| e.to_string())?;
            if matches!(connection, Connection::PostgreSql(_)) {
                let settings = format!(
                    "SET LOCAL statement_timeout = '{}s'; SET LOCAL bytea_output = 'hex'",
                    timeout_seconds
                );
                connection
                    .command(&settings)
                    .await
                    .map_err(|e| e.to_string())?;
            }
            let rows = stream_query_csv(connection.query_connection(), sql, file, progress).await?;
            connection
                .command("ROLLBACK")
                .await
                .map_err(|e| e.to_string())?;
            Ok(rows)
        };
        let timeout = async {
            if timeout_seconds == 0 {
                std::future::pending::<()>().await;
            }
            tokio::time::sleep(Duration::from_secs(timeout_seconds)).await;
        };
        // Stop covers connection acquisition, preparation and the wait for the first row.
        let stop = select(Box::pin(cancel.cancelled()), Box::pin(timeout));
        let reason = match select(Box::pin(stop), Box::pin(export)).await {
            Either::Right((result, _)) => return result,
            Either::Left((Either::Left(_), _)) => "Экспорт отменён",
            Either::Left((Either::Right(_), _)) => "Превышено время выполнения SQL",
        };
        // Keep the detached connection alive while cancelling its server query.
        if inner.connection.is_some() {
            let _ = tokio::time::timeout(Duration::from_secs(2), session.cancel_backend()).await;
        }
        Err(reason.into())
    }
    pub fn requires_write_approval(&self, sql: &str, state: TransactionState) -> bool {
        self.production
            && !self.read_only
            && (crate::sql_risk::analyze(
                sql,
                if matches!(self.pool, DbPool::MySql(_)) {
                    crate::sql_risk::Dialect::MySql
                } else {
                    crate::sql_risk::Dialect::PostgreSql
                },
            ) != crate::sql_risk::SqlRisk::ReadOnly
                || (state.pending && !state.read_only))
    }
    pub fn pool(&self) -> &DbPool {
        &self.pool
    }
    pub fn revision(&self) -> u64 {
        self.revision.load(Ordering::Acquire)
    }
    pub fn identity(&self) -> usize {
        Arc::as_ptr(&self.inner) as usize
    }
    pub fn has_pending_transaction(&self) -> bool {
        self.pending.load(Ordering::Acquire)
    }
    pub fn is_closed(&self) -> bool {
        self.closed.load(Ordering::Acquire)
    }
    pub fn schema(&self) -> Option<&str> {
        self.schema.as_deref()
    }

    async fn prepare_postgres(&self, connection: &mut sqlx::PgConnection) -> Result<u64, DbError> {
        if let Some(schema) = &self.schema {
            let path = format!("\"{}\"", schema.replace('"', "\"\""));
            sqlx::query("SELECT pg_catalog.set_config('search_path', $1, false)")
                .bind(path)
                .execute(&mut *connection)
                .await
                .map_err(|e| raw_error(&e))?;
        }
        sqlx::query_scalar::<_, i32>("SELECT pg_catalog.pg_backend_pid()")
            .fetch_one(connection)
            .await
            .map(|id| id as u64)
            .map_err(|e| raw_error(&e))
    }

    async fn connect(&self, inner: &mut Inner) -> Result<(), DbError> {
        if self.closed.load(Ordering::Acquire) {
            return Err("SQL-сессия закрыта".into());
        }
        if inner.connection.is_none() {
            let (connection, id) = match &self.pool {
                DbPool::PostgreSql(pool) => {
                    let mut connection = pool.acquire().await.map_err(|e| raw_error(&e))?.detach();
                    let id = self.prepare_postgres(&mut connection).await?;
                    (Connection::PostgreSql(connection), id)
                }
                DbPool::MySql(pool) => {
                    let mut connection = pool.acquire().await.map_err(|e| raw_error(&e))?.detach();
                    let id = sqlx::query_scalar::<_, u64>("SELECT CONNECTION_ID()")
                        .fetch_one(&mut connection)
                        .await
                        .map_err(|e| raw_error(&e))?;
                    (Connection::MySql(connection), id)
                }
            };
            inner.connection = Some(connection);
            inner.last_activity = Some(Instant::now());
            self.backend_id.store(id, Ordering::Release);
        }
        Ok(())
    }

    async fn check_idle_postgres(&self, inner: &mut Inner) -> Result<bool, DbError> {
        let (DbPool::PostgreSql(pool), Some(Connection::PostgreSql(connection))) =
            (&self.pool, inner.connection.as_mut())
        else {
            return Ok(false);
        };
        if !inner
            .last_activity
            .is_some_and(|last| last.elapsed() >= POSTGRES_IDLE_AFTER)
        {
            return Ok(false);
        }
        // PostgreSQL Sync/ReadyForQuery also works in an aborted transaction.
        let cause = match tokio::time::timeout(POSTGRES_PROBE_TIMEOUT, connection.ping()).await {
            Ok(Ok(())) => {
                inner.last_activity = Some(Instant::now());
                return Ok(false);
            }
            Ok(Err(error)) => error.to_string(),
            Err(_) => "сервер не ответил за 5 с".to_owned(),
        };
        inner.connection.take();
        self.backend_id.store(0, Ordering::Release);
        if inner.state.pending {
            self.mark_closed(inner);
            return Err(DbError {
                kind: DbErrorKind::Disconnected,
                message: format!(
                    "Соединение потеряно во время простоя ({cause}). Открытая транзакция потеряна. SQL не отправлен. Переподключитесь вручную."
                ),
            });
        }

        // Acquire can keep opening sockets until its deadline. Connect directly
        // with the existing pool options to make exactly one replacement attempt.
        let replacement = tokio::time::timeout(POSTGRES_RECONNECT_TIMEOUT, async {
            let mut connection = sqlx::PgConnection::connect_with(&pool.connect_options())
                .await
                .map_err(|e| raw_error(&e))?;
            configure_pg_connection(&mut connection)
                .await
                .map_err(|e| raw_error(&e))?;
            let id = self.prepare_postgres(&mut connection).await?;
            Ok::<_, DbError>((connection, id))
        })
        .await
        .map(|result| result.map_err(|error| error.message))
        .unwrap_or_else(|_| Err("сервер не ответил за 8 с".to_owned()));
        match replacement {
            Ok((connection, id)) => {
                inner.connection = Some(Connection::PostgreSql(connection));
                inner.last_activity = Some(Instant::now());
                self.backend_id.store(id, Ordering::Release);
                Ok(true)
            }
            Err(detail) => {
                self.mark_closed(inner);
                Err(DbError {
                    kind: DbErrorKind::Disconnected,
                    message: format!(
                        "Соединение потеряно во время простоя ({cause}). Переподключение не удалось ({detail}). SQL не отправлен. Переподключитесь вручную."
                    ),
                })
            }
        }
    }

    fn mark_closed(&self, inner: &mut Inner) {
        inner.connection.take();
        inner.state = TransactionState::default();
        self.backend_id.store(0, Ordering::Release);
        self.pending.store(false, Ordering::Release);
        self.closed.store(true, Ordering::Release);
    }
    pub fn request_cancel(&self) {
        self.cancel_requested.store(true, Ordering::Release);
        self.cancel_notify.notify_one();
    }
    /// Called before starting a new query on an idle tab. GTK may have accepted
    /// a Stop after the previous worker finished but before receiving its result.
    pub fn clear_cancel_request(&self) {
        self.cancel_requested.store(false, Ordering::Release);
    }
    async fn cancelled(&self) {
        loop {
            let notified = self.cancel_notify.notified();
            if self.cancel_requested.load(Ordering::Acquire) {
                return;
            }
            notified.await;
        }
    }
    async fn cancel_backend(&self) -> Result<(), String> {
        let id = self.backend_id.load(Ordering::Acquire);
        match &self.pool {
            DbPool::PostgreSql(pool) => {
                sqlx::query_scalar::<_, bool>("SELECT pg_cancel_backend($1)")
                    .bind(id as i32)
                    .fetch_one(pool)
                    .await
                    .map(|_| ())
                    .map_err(|e| e.to_string())
            }
            DbPool::MySql(pool) => {
                // Numeric server-generated identifier; never interpolate user SQL here.
                let command = format!("KILL QUERY {id}");
                sqlx::Executor::execute(pool, command.as_str())
                    .await
                    .map(|_| ())
                    .map_err(|e| e.to_string())
            }
        }
    }
    pub async fn state(&self) -> TransactionState {
        self.inner.lock().await.state
    }
    /// Checks this session's own detached connection (`DbPool::check` cannot see it).
    /// A busy or absent connection is skipped; an error inside an aborted transaction
    /// (25P02) stays `Other` because the connection is alive.
    pub async fn ping(&self) -> Result<(), DbError> {
        let Ok(mut inner) = self.inner.try_lock() else {
            return Ok(());
        };
        if self.closed.load(Ordering::Acquire) {
            return Ok(());
        }
        let Some(connection) = inner.connection.as_mut() else {
            return Ok(());
        };
        match tokio::time::timeout(Duration::from_secs(5), connection.command("SELECT 1")).await {
            Ok(result) => result,
            Err(_) => Err(DbError {
                kind: DbErrorKind::Disconnected,
                message: "Проверка соединения: сервер не ответил за 5 с".to_owned(),
            }),
        }
    }
    async fn control_command(&self, inner: &mut Inner, sql: &str) -> Result<(), DbError> {
        let result = tokio::time::timeout(
            Duration::from_secs(10),
            inner.connection.as_mut().unwrap().command(sql),
        )
        .await;
        match result {
            Ok(Ok(())) => {
                inner.last_activity = Some(Instant::now());
                Ok(())
            }
            error => {
                // The server may have applied COMMIT even when its reply was lost.
                // Never reuse or retry a connection whose transaction state is unknown.
                self.mark_closed(inner);
                let (kind, cause) = match error {
                    Ok(Err(e)) => (e.kind, e.message),
                    _ => (DbErrorKind::Other, "таймаут 10 с".to_owned()),
                };
                Err(DbError {
                    kind,
                    message: format!(
                        "SQL-сессия закрыта: ответ на {sql} не подтверждён ({cause}). Сбросьте сессию; перед повторной записью проверьте данные на сервере."
                    ),
                })
            }
        }
    }
    #[cfg(test)]
    pub async fn execute(&self, sql: &str) -> Result<QueryResult, DbError> {
        self.execute_with_timeout(sql, 60).await
    }
    pub async fn execute_with_timeout(
        &self,
        sql: &str,
        timeout_seconds: u64,
    ) -> Result<QueryResult, DbError> {
        self.execute_sql(sql, timeout_seconds, false, None).await
    }
    #[cfg(test)]
    pub async fn execute_authorized(
        &self,
        sql: &str,
        timeout_seconds: u64,
        write_approved: bool,
    ) -> Result<QueryResult, DbError> {
        self.execute_sql(sql, timeout_seconds, write_approved, None)
            .await
    }
    pub async fn execute_with_recovery(
        &self,
        sql: &str,
        timeout_seconds: u64,
        write_approved: bool,
    ) -> (Result<QueryResult, DbError>, bool) {
        let mut reconnected = false;
        let result = self
            .execute_sql(sql, timeout_seconds, write_approved, Some(&mut reconnected))
            .await;
        (result, reconnected)
    }
    async fn execute_sql(
        &self,
        sql: &str,
        timeout_seconds: u64,
        write_approved: bool,
        reconnected: Option<&mut bool>,
    ) -> Result<QueryResult, DbError> {
        let mysql = matches!(self.pool, DbPool::MySql(_));
        let mut inner = self.inner.lock().await;
        let policy = statement_policy(sql, mysql)?;
        let risk = sql_risk(sql, mysql);
        if self.read_only {
            validate_read_only(risk, &policy)?;
        }
        if self.requires_write_approval(sql, inner.state) && !write_approved {
            return Err("PROD: запись требует подтверждения целевой БД для этой операции".into());
        }
        if inner.state.manual && policy.managed_unsafe {
            return Err("В ручном режиме используйте Commit/Rollback на панели. Команды управления транзакциями, сессией и DDL выполняйте отдельно в Auto Commit: MySQL DDL может делать неявный commit.".into());
        }
        let guarded_read = self.read_only || (self.production && !write_approved);
        if guarded_read
            && (risk != crate::sql_risk::SqlRisk::ReadOnly
                || policy.managed_unsafe
                || policy.statements != 1)
        {
            return Err("PROD: запрос не распознан как одиночный читающий; подтвердите выполнение с правом записи".into());
        }
        if inner.state.pending && inner.state.read_only && !guarded_read {
            return Err("Текущая транзакция доступна только для чтения. Выполните Rollback/Commit и повторите запись в новой транзакции.".into());
        }
        self.connect(&mut inner).await?;
        if let Some(reconnected) = reconnected {
            *reconnected = self.check_idle_postgres(&mut inner).await?;
        }
        if self.closed.load(Ordering::Acquire) || self.cancel_requested.load(Ordering::Acquire) {
            return Err("Запрос отменён до выполнения SQL".into());
        }
        self.revision.fetch_add(1, Ordering::AcqRel);
        let auto_read = guarded_read && !inner.state.manual;
        if auto_read || (inner.state.manual && !inner.state.pending) {
            let begin = if guarded_read {
                if mysql {
                    "START TRANSACTION READ ONLY"
                } else {
                    "BEGIN READ ONLY"
                }
            } else {
                "BEGIN"
            };
            self.control_command(&mut inner, begin).await?;
            inner.state.read_only = guarded_read;
        }
        if inner.state.manual && !inner.state.pending {
            inner.state.pending = true;
            self.pending.store(true, Ordering::Release);
        }
        let pending_before_execution = inner.state.pending;
        let execution_started = AtomicBool::new(false);
        // For explicit transaction SQL, stay conservative even if a later statement fails.
        if policy.transaction_control {
            inner.state.pending = true;
            self.pending.store(true, Ordering::Release);
        }
        // Do not release this connection or allow the next query until cancellation is acknowledged.
        let mut connection_lost = false;
        let mut result = {
            let query = if guarded_read {
                crate::database::execute_read_only_on_connection(
                    inner.connection.as_mut().unwrap().query_connection(),
                    sql,
                    Some(&self.cancel_requested),
                    Some(&execution_started),
                )
            } else {
                execute_on_connection_controlled(
                    inner.connection.as_mut().unwrap().query_connection(),
                    sql,
                    Some(&self.cancel_requested),
                    Some(&execution_started),
                )
            };
            tokio::pin!(query);
            let timeout = async {
                if timeout_seconds == 0 {
                    std::future::pending::<()>().await;
                }
                tokio::time::sleep(Duration::from_secs(timeout_seconds)).await;
            };
            let cancellation =
                futures_util::future::select(Box::pin(self.cancelled()), Box::pin(timeout));
            let reason = match futures_util::future::select(&mut query, Box::pin(cancellation))
                .await
            {
                futures_util::future::Either::Left((result, _)) => Ok(result),
                futures_util::future::Either::Right((futures_util::future::Either::Left(_), _)) => {
                    Err("Запрос остановлен пользователем")
                }
                futures_util::future::Either::Right((
                    futures_util::future::Either::Right(_),
                    _,
                )) => Err("Превышено время выполнения SQL"),
            };
            match reason {
                Ok(result) => result,
                Err(reason) => {
                    self.cancel_requested.store(true, Ordering::Release);
                    let cancellation =
                        tokio::time::timeout(Duration::from_secs(8), self.cancel_backend()).await;
                    match tokio::time::timeout(Duration::from_secs(5), &mut query).await {
                        Ok(result) => {
                            let detail = match cancellation {
                                Ok(Err(e)) => format!("; отмена на сервере: {e}"),
                                Err(_) => "; сервер не подтвердил отмену вовремя".into(),
                                _ => String::new(),
                            };
                            match result {
                                Ok(_) => Err(format!(
                                    "{reason}. Запрос успел завершиться; проверьте результат изменений{detail}"
                                )
                                .into()),
                                Err(error) => Err(DbError {
                                    kind: error.kind,
                                    message: format!("{reason}: {error}{detail}"),
                                }),
                            }
                        }
                        Err(_) => {
                            connection_lost = true;
                            if mysql {
                                Err(format!(
                                    "{reason}. Ответ сервера не получен, SQL-сессия закрыта. Незавершённая транзакция откатится при закрытии соединения. Сбросьте SQL-сессию через меню; результат уже выполненных команд Auto Commit проверьте отдельно."
                                )
                                .into())
                            } else {
                                Err(DbError {
                                    kind: DbErrorKind::Disconnected,
                                    message: format!("{reason}. Ответ сервера не получен"),
                                })
                            }
                        }
                    }
                }
            }
        };
        inner.last_activity = Some(Instant::now());
        if !mysql
            && result
                .as_ref()
                .is_err_and(|error| error.kind == DbErrorKind::Disconnected)
        {
            connection_lost = true;
            let error = result.unwrap_err();
            let outcome = if execution_started.load(Ordering::Acquire) {
                "Результат запроса неизвестен."
            } else {
                "SQL не отправлен."
            };
            let transaction = if pending_before_execution {
                " Открытая транзакция потеряна."
            } else {
                ""
            };
            result = Err(DbError {
                kind: DbErrorKind::Disconnected,
                message: format!(
                    "Соединение потеряно: {error}. {outcome}{transaction} Переподключитесь вручную."
                ),
            });
        }
        if auto_read && !connection_lost {
            let rollback = if mysql {
                "ROLLBACK AND NO CHAIN NO RELEASE"
            } else {
                "ROLLBACK"
            };
            if let Err(error) = self.control_command(&mut inner, rollback).await {
                result = Err(DbError {
                    kind: error.kind,
                    message: format!("Не удалось завершить транзакцию чтения: {error}"),
                });
                connection_lost = true;
            }
            inner.state.read_only = false;
        }
        self.cancel_requested.store(false, Ordering::Release);
        if connection_lost {
            self.mark_closed(&mut inner);
        }
        if !connection_lost
            && policy.transaction_control
            && !execution_started.load(Ordering::Acquire)
        {
            inner.state.pending = pending_before_execution;
            self.pending
                .store(pending_before_execution, Ordering::Release);
        }
        if result.is_err() && inner.state.pending {
            inner.state.failed = true;
        }
        if result.is_ok() && !inner.state.manual {
            if let Some(pending) = policy.final_pending {
                inner.state.pending = pending;
                inner.state.failed = false;
                self.pending.store(pending, Ordering::Release);
            }
        }
        if !connection_lost {
            if let Some(Connection::MySql(connection)) = inner.connection.as_mut() {
                match sqlx::query_scalar::<_, i64>("SELECT @@session.autocommit")
                    .fetch_one(connection)
                    .await
                {
                    Ok(0) => {
                        inner.state.manual = true;
                        inner.state.pending = true;
                        self.pending.store(true, Ordering::Release);
                        if result.is_err() {
                            inner.state.failed = true;
                        }
                    }
                    Ok(_) => {}
                    Err(error) => {
                        // Unknown server state must never enable grid writes or silent close.
                        inner.state.manual = true;
                        inner.state.pending = true;
                        inner.state.failed = true;
                        self.pending.store(true, Ordering::Release);
                        result = Err(DbError {
                            kind: classify(&error),
                            message: format!(
                                "Не удалось подтвердить режим транзакций MySQL: {error}. Завершите транзакцию или сбросьте SQL-сессию."
                            ),
                        });
                    }
                }
            }
        }
        result
    }
    pub async fn update_cells(
        &self,
        object: &crate::database::DbObject,
        edits: &[crate::database::TableCellEdit],
    ) -> Result<usize, String> {
        self.update_cells_authorized(object, edits, false).await
    }
    pub async fn update_cells_authorized(
        &self,
        object: &crate::database::DbObject,
        edits: &[crate::database::TableCellEdit],
        approved: bool,
    ) -> Result<usize, String> {
        self.check_write(approved)?;
        let mut inner = self.inner.lock().await;
        self.connect(&mut inner).await?;
        if inner.state.manual || inner.state.pending {
            return Err("Правки результата доступны в Auto Commit вне транзакции".into());
        }
        let result = crate::database::update_table_cells_on(
            inner.connection.as_mut().unwrap().query_connection(),
            object,
            edits,
        )
        .await;
        inner.last_activity = Some(Instant::now());
        result
    }
    pub async fn delete_row(
        &self,
        object: &crate::database::DbObject,
        keys: &[crate::database::CellValue],
    ) -> Result<u64, String> {
        self.delete_row_authorized(object, keys, false).await
    }
    pub async fn delete_row_authorized(
        &self,
        object: &crate::database::DbObject,
        keys: &[crate::database::CellValue],
        approved: bool,
    ) -> Result<u64, String> {
        self.check_write(approved)?;
        let mut inner = self.inner.lock().await;
        self.connect(&mut inner).await?;
        if inner.state.manual || inner.state.pending {
            return Err("Удаление из результата доступно в Auto Commit вне транзакции".into());
        }
        let result = crate::database::delete_table_row_on(
            inner.connection.as_mut().unwrap().query_connection(),
            object,
            keys,
        )
        .await;
        inner.last_activity = Some(Instant::now());
        result
    }
    fn check_write(&self, approved: bool) -> Result<(), String> {
        if self.read_only {
            Err("Подключение доступно только для чтения".into())
        } else if self.production && !approved {
            Err("PROD: запись требует подтверждения целевой БД".into())
        } else {
            Ok(())
        }
    }
    #[cfg(test)]
    pub async fn transaction(
        &self,
        action: TransactionAction,
    ) -> Result<TransactionState, DbError> {
        self.transaction_with_approval(action, None).await
    }
    pub async fn transaction_with_approval(
        &self,
        action: TransactionAction,
        approved_revision: Option<u64>,
    ) -> Result<TransactionState, DbError> {
        let mut inner = self.inner.lock().await;
        if self.production
            && action == TransactionAction::Commit
            && inner.state.pending
            && !inner.state.read_only
            && approved_revision != Some(self.revision())
        {
            return Err("PROD: транзакция изменилась или Commit не подтверждён. Проверьте изменения и подтвердите Commit заново.".into());
        }
        self.revision.fetch_add(1, Ordering::AcqRel);
        self.connect(&mut inner).await?;
        match action {
            TransactionAction::Manual => {
                if inner.state.pending {
                    return Err("Сначала завершите текущую транзакцию".into());
                }
                inner.state.manual = true;
            }
            TransactionAction::AutoCommit => {
                if inner.state.pending {
                    return Err("Сначала выполните Commit или Rollback".into());
                }
                if let Some(Connection::MySql(connection)) = inner.connection.as_mut() {
                    sqlx::Executor::execute(&mut *connection, "SET SESSION autocommit=1")
                        .await
                        .map_err(|e| raw_error(&e))?;
                    let auto: i64 = sqlx::query_scalar("SELECT @@session.autocommit")
                        .fetch_one(connection)
                        .await
                        .map_err(|e| raw_error(&e))?;
                    if auto != 1 {
                        return Err("MySQL не подтвердил Auto Commit".into());
                    }
                }
                inner.state.manual = false;
            }
            TransactionAction::Commit | TransactionAction::Rollback => {
                if action == TransactionAction::Commit && inner.state.failed {
                    return Err(
                        "В транзакции была ошибка. Выполните Rollback перед продолжением.".into(),
                    );
                }
                let command = if action == TransactionAction::Commit {
                    "COMMIT"
                } else {
                    "ROLLBACK"
                };
                self.control_command(&mut inner, command).await?;
                inner.state.pending = false;
                inner.state.failed = false;
                inner.state.read_only = false;
                self.pending.store(false, Ordering::Release);
            }
        }
        Ok(inner.state)
    }
    pub async fn close(&self) {
        self.closed.store(true, Ordering::Release);
        // Closing the socket also rolls back raw SQL transactions, including aborted ones.
        let mut inner = self.inner.lock().await;
        inner.connection.take();
        inner.state.pending = false;
        self.pending.store(false, Ordering::Release);
    }
}
#[derive(Debug, Default)]
struct Policy {
    statements: usize,
    managed_unsafe: bool,
    transaction_control: bool,
    final_pending: Option<bool>,
}
fn sql_risk(sql: &str, mysql: bool) -> crate::sql_risk::SqlRisk {
    use crate::sql_risk::{Dialect, analyze};
    let dialect = if mysql {
        Dialect::MySql
    } else {
        Dialect::PostgreSql
    };
    analyze(sql, dialect)
}

fn validate_read_only(risk: crate::sql_risk::SqlRisk, policy: &Policy) -> Result<(), String> {
    if policy.statements == 1
        && !policy.managed_unsafe
        && risk == crate::sql_risk::SqlRisk::ReadOnly
    {
        Ok(())
    } else {
        Err("Только чтение: разрешён один читающий запрос. Управление сессией, запись и неизвестные команды запрещены.".into())
    }
}

fn statement_policy(sql: &str, mysql: bool) -> Result<Policy, String> {
    let dialect: &dyn sqlparser::dialect::Dialect = if mysql {
        &MySqlDialect {}
    } else {
        &PostgreSqlDialect {}
    };
    let tokens = Tokenizer::new(dialect, sql)
        .tokenize()
        .map_err(|e| e.to_string())?;
    // Semicolons inside a PostgreSQL SQL-standard routine body can make its
    // END (or a body statement) look like a top-level transaction command.
    let significant = tokens
        .iter()
        .filter(|token| !matches!(token, Token::Whitespace(_)));
    let has_atomic_body = !mysql
        && significant
            .clone()
            .zip(significant.skip(1))
            .any(|(first, second)| {
                matches!(first, Token::Word(word) if word.keyword == Keyword::BEGIN)
                    && matches!(second, Token::Word(word) if word.keyword == Keyword::ATOMIC)
            });
    let mut policy = Policy::default();
    for statement in tokens.split(|t| matches!(t, Token::SemiColon)) {
        let first = statement
            .iter()
            .find(|t| !matches!(t, Token::Whitespace(_)));
        if first.is_some() {
            policy.statements += 1;
        }
        if let Some(Token::Word(word)) = first {
            let keyword = word.value.to_ascii_uppercase();
            let closes_transaction =
                matches!(keyword.as_str(), "COMMIT" | "END" | "ROLLBACK" | "ABORT");
            if has_atomic_body && closes_transaction {
                // Keep any pending transaction until a separate command confirms closure.
                policy.managed_unsafe = true;
                continue;
            }
            let transaction = matches!(
                keyword.as_str(),
                "BEGIN"
                    | "START"
                    | "COMMIT"
                    | "END"
                    | "ROLLBACK"
                    | "ABORT"
                    | "SAVEPOINT"
                    | "RELEASE"
            ) || (mysql && matches!(keyword.as_str(), "CALL" | "EXECUTE" | "XA"));
            policy.transaction_control |= transaction;
            let words: Vec<_> = statement
                .iter()
                .filter_map(|t| {
                    if let Token::Word(w) = t {
                        Some(w.value.to_ascii_uppercase())
                    } else {
                        None
                    }
                })
                .collect();
            if mysql && matches!(keyword.as_str(), "CALL" | "EXECUTE" | "XA") {
                policy.final_pending = Some(true);
            }
            if matches!(keyword.as_str(), "BEGIN" | "START" | "SAVEPOINT") {
                policy.final_pending = Some(true);
            }
            if closes_transaction {
                let to_savepoint = keyword == "ROLLBACK" && words.iter().any(|w| w == "TO");
                let chain = words.windows(2).any(|w| w == ["AND", "CHAIN"]);
                policy.final_pending = Some(to_savepoint || chain);
            }
            // Whitelist statement families whose execution cannot implicitly commit MySQL DDL.
            policy.managed_unsafe |= transaction
                || !matches!(
                    keyword.as_str(),
                    "SELECT"
                        | "WITH"
                        | "VALUES"
                        | "TABLE"
                        | "INSERT"
                        | "UPDATE"
                        | "DELETE"
                        | "MERGE"
                        | "REPLACE"
                        | "EXPLAIN"
                        | "SHOW"
                        | "DESCRIBE"
                        | "DESC"
                );
        } else if first.is_some() {
            policy.managed_unsafe = true;
        }
    }
    // MySQL executable comments bypass ordinary SQL tokenization.
    if mysql && (sql.contains("/*!") || sql.to_ascii_uppercase().contains("/*M!")) {
        policy.managed_unsafe = true;
        policy.transaction_control = true;
    }
    // A session can turn standard_conforming_strings off. An escaped quote in
    // a batch can move a transaction command across the string boundary.
    if !mysql && sql.contains("\\'") && sql.trim_end().trim_end_matches(';').contains(';') {
        policy.transaction_control = true;
        policy.final_pending = Some(true);
    }
    Ok(policy)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn transaction_policy_ignores_literals_and_comments() {
        for mysql in [false, true] {
            let p = statement_policy(
                "-- BEGIN\n SELECT 'COMMIT; DROP', 1; UPDATE t SET a=2",
                mysql,
            )
            .unwrap();
            assert!(!p.managed_unsafe && !p.transaction_control);
            let p = statement_policy("SELECT 1; BEGIN; UPDATE t SET a=2; COMMIT", mysql).unwrap();
            assert!(p.managed_unsafe && p.transaction_control);
            assert!(
                statement_policy("CREATE TABLE t (id int)", mysql)
                    .unwrap()
                    .managed_unsafe
            );
            assert!(
                statement_policy("SET autocommit=1", mysql)
                    .unwrap()
                    .managed_unsafe
            );
        }
        assert_eq!(
            statement_policy("CALL test_procedure()", true)
                .unwrap()
                .final_pending,
            Some(true)
        );
        assert_eq!(
            statement_policy("EXECUTE stmt", true)
                .unwrap()
                .final_pending,
            Some(true)
        );
        assert!(
            statement_policy("/*! COMMIT */", true)
                .unwrap()
                .transaction_control
        );
    }
}

#[cfg(test)]
mod integration_tests {
    use super::*;
    use crate::{
        database::CellValue,
        settings::{ConnectionProfile, DatabaseKind, EnvironmentTag, TlsMode},
    };

    #[test]
    #[ignore = "requires local Compose DBs with sample_records fixture"]
    fn estimated_plans_all_engines() {
        use crate::sql_explain::EstimatedPlan;

        async fn estimate(
            editor: &SqlSession,
            kind: DatabaseKind,
            sql: &str,
        ) -> Result<QueryResult, DbError> {
            let plan = EstimatedPlan::new(kind, sql).unwrap();
            let session = editor.isolated_read_only();
            let result = session.execute_with_timeout(plan.sql(), 5).await;
            session.close().await;
            result
        }

        tokio::runtime::Runtime::new().unwrap().block_on(async {
            for (kind, port, password_key) in [
                (DatabaseKind::PostgreSql, 55432, "DBDECK_PG_PASSWORD"),
                (DatabaseKind::MariaDb, 53306, "DBDECK_MARIADB_PASSWORD"),
                (DatabaseKind::MySql, 53307, "DBDECK_MYSQL_PASSWORD"),
            ] {
                let profile: ConnectionProfile = serde_json::from_value(serde_json::json!({
                    "id":"estimate", "name":"estimate", "kind":kind, "host":"127.0.0.1", "port":port,
                    "database":"dbdeck_test", "username":"dbdeck_test", "environment":"local",
                    "color":"#ffffff", "read_only":false, "tls_mode":"disabled"
                })).unwrap();
                let password = std::env::var(password_key).unwrap();
                let pool = DbPool::connect(&profile, &password).await.unwrap();
                let editor = SqlSession::with_profile_and_schema(pool, &profile, Some("public".into()));
                let postgres = matches!(kind, DatabaseKind::PostgreSql);
                let before = editor.execute("SELECT name FROM sample_records WHERE id=1").await.unwrap();
                let result = estimate(&editor, kind, "SELECT name FROM sample_records WHERE id=1; -- tail")
                    .await.unwrap_or_else(|error| panic!("{kind:?}: {error}"));
                let expected_column = if postgres { "QUERY PLAN" } else { "select_type" };
                assert!(result.data.columns.iter().any(|name| name == expected_column));
                assert!(!result.data.rows.is_empty() && result.editable_object.is_none());
                match estimate(&editor, kind, "UPDATE sample_records SET name='estimated' WHERE id=1").await {
                    Ok(result) => assert!(!result.data.rows.is_empty()),
                    Err(error) if !postgres => assert!(error.message.contains("25006"), "{kind:?}: {error}"),
                    Err(error) => panic!("{kind:?}: {error}"),
                }
                let after = editor.execute("SELECT name FROM sample_records WHERE id=1").await.unwrap();
                assert_eq!(before.data.rows, after.data.rows, "{kind:?}");

                if postgres {
                    editor.execute("BEGIN READ ONLY; SELECT 1").await.unwrap();
                    assert!(estimate(&editor, kind, "SELECT dbdeck_missing_column").await.is_err());
                    assert!(editor.execute("SELECT 2").await.is_ok());
                    assert!(estimate(&editor, kind, "((SELECT 1) UNION ALL (SELECT 2))").await.is_ok());
                    editor.execute("ROLLBACK").await.unwrap();
                } else {
                    editor.execute("LOCK TABLES sample_records WRITE").await.unwrap();
                    assert!(estimate(&editor, kind, "SELECT 1").await.is_ok());
                    assert!(editor.execute("SELECT COUNT(*) FROM sample_records AS unlocked_alias").await.is_err());
                    editor.execute("UNLOCK TABLES").await.unwrap();

                    editor.execute("CREATE TEMPORARY TABLE dbdeck_estimate (id INT PRIMARY KEY)").await.unwrap();
                    editor.execute("SET SESSION completion_type=1").await.unwrap();
                    editor.execute("START TRANSACTION; COMMIT; INSERT INTO dbdeck_estimate VALUES (1)").await.unwrap();
                    assert!(estimate(&editor, kind, "SELECT 1").await.is_ok());
                    editor.execute("ROLLBACK AND NO CHAIN NO RELEASE").await.unwrap();
                    assert!(editor.execute("SELECT id FROM dbdeck_estimate").await.unwrap().data.rows.is_empty());
                }
                editor.close().await;
            }
        });
    }

    #[test]
    #[ignore = "requires Compose DBs; connection-local TEMP objects only"]
    fn readonly_and_production_policy_all_engines() {
        tokio::runtime::Runtime::new().unwrap().block_on(async{
            for (kind,port,key) in [(DatabaseKind::PostgreSql,55432,"DBDECK_PG_PASSWORD"),(DatabaseKind::MariaDb,53306,"DBDECK_MARIADB_PASSWORD"),(DatabaseKind::MySql,53307,"DBDECK_MYSQL_PASSWORD")] {
                let mut profile:ConnectionProfile=serde_json::from_value(serde_json::json!({"id":"policy","name":"policy","kind":kind,"host":"127.0.0.1","port":port,"database":"dbdeck_test","username":"dbdeck_test","environment":"local","color":"#ffffff","read_only":false,"tls_mode":"disabled"})).unwrap();
                let pool=DbPool::connect(&profile,&std::env::var(key).unwrap()).await.unwrap();
                let bootstrap=SqlSession::new(pool.clone());
                bootstrap.execute("CREATE TEMPORARY TABLE dbdeck_policy_test (id INT PRIMARY KEY, value TEXT)").await.unwrap();
                bootstrap.execute("INSERT INTO dbdeck_policy_test VALUES(1,'original')").await.unwrap();
                if matches!(kind,DatabaseKind::PostgreSql){bootstrap.execute("CREATE FUNCTION pg_temp.dbdeck_temp_mutation() RETURNS text LANGUAGE plpgsql AS $$BEGIN UPDATE pg_temp.dbdeck_policy_test SET value='hidden'; RETURN 'hidden'; END$$").await.unwrap();}
                profile.read_only=true;
                let readonly=SqlSession::with_profile(pool.clone(),&profile);
                readonly.inner.lock().await.connection=bootstrap.inner.lock().await.connection.take();
                let values=readonly.execute("SELECT id, value, CAST(123.45 AS DECIMAL(10,2)) AS amount, CAST('2025-01-02' AS DATE) AS day FROM dbdeck_policy_test").await.unwrap();
                assert_eq!(values.data.rows[0],vec![CellValue::Text("1".into()),CellValue::Text("original".into()),CellValue::Text("123.45".into()),CellValue::Text("2025-01-02".into())]);
                assert_eq!(readonly.execute("SELECT value FROM dbdeck_policy_test").await.unwrap().data.rows[0][0],CellValue::Text("original".into()));
                for sql in ["UPDATE dbdeck_policy_test SET value='bad'", "WITH changed AS (DELETE FROM dbdeck_policy_test RETURNING *) SELECT * FROM changed", "COMMIT", "SET autocommit=0", "SELECT 1; SELECT 2"] {
                    assert!(readonly.execute_authorized(sql,5,true).await.unwrap_err().message.contains("Только чтение"),"{kind:?}: {sql}");
                }
                if matches!(kind,DatabaseKind::PostgreSql){
                    assert_eq!(readonly.execute("SELECT current_setting('transaction_read_only')").await.unwrap().data.rows[0][0],CellValue::Text("on".into()));
                    readonly.execute("SELECT pg_temp.dbdeck_temp_mutation()").await.unwrap();
                    assert_eq!(readonly.execute("SELECT value FROM dbdeck_policy_test").await.unwrap().data.rows[0][0],CellValue::Text("original".into()),"auto readonly rolls back hidden temp side effects");
                }
                readonly.transaction(TransactionAction::Manual).await.unwrap();readonly.execute("SELECT 1").await.unwrap();assert!(readonly.state().await.read_only);
                readonly.transaction(TransactionAction::Rollback).await.unwrap();readonly.transaction(TransactionAction::AutoCommit).await.unwrap();
                assert!(readonly.update_cells_authorized(&crate::database::DbObject{schema:"unused".into(),name:"unused".into(),kind:crate::database::DbObjectKind::Table},&[],true).await.is_err());
                profile.read_only=false;profile.environment=EnvironmentTag::Prod;
                let prod=SqlSession::with_profile(pool.clone(),&profile);prod.inner.lock().await.connection=readonly.inner.lock().await.connection.take();
                assert!(prod.execute("UPDATE dbdeck_policy_test SET value='bad'").await.unwrap_err().message.contains("PROD"));
                prod.execute_authorized("UPDATE dbdeck_policy_test SET value='approved'",5,true).await.unwrap();
                assert_eq!(prod.execute("SELECT value FROM dbdeck_policy_test").await.unwrap().data.rows[0][0],CellValue::Text("approved".into()));
                prod.transaction(TransactionAction::Manual).await.unwrap();prod.execute("SELECT 1").await.unwrap();assert!(prod.state().await.read_only);
                assert!(prod.execute_authorized("UPDATE dbdeck_policy_test SET value='bad'",5,true).await.unwrap_err().message.contains("транзакция"));
                prod.transaction(TransactionAction::Rollback).await.unwrap();
                prod.execute_authorized("UPDATE dbdeck_policy_test SET value='pending'",5,true).await.unwrap();
                assert!(!prod.state().await.read_only);
                assert!(prod.execute("SELECT 1").await.unwrap_err().message.contains("PROD"));
                prod.transaction(TransactionAction::Rollback).await.unwrap();prod.transaction(TransactionAction::AutoCommit).await.unwrap();
                assert_eq!(prod.execute("SELECT value FROM dbdeck_policy_test").await.unwrap().data.rows[0][0],CellValue::Text("approved".into()));
                for sql in ["SELECT GET_LOCK('dbdeck-policy',0)", "SELECT pg_try_advisory_lock(123)","SELECT set_config('work_mem','1GB',false)"] {
                    assert!(readonly.execute(sql).await.is_err(),"read-only accepted {sql}");
                    assert!(prod.execute(sql).await.is_err(),"PROD accepted {sql} without approval");
                }
                prod.transaction(TransactionAction::Manual).await.unwrap();
                assert!(prod.execute_authorized("SET application_name='unsafe-manual'",5,true).await.is_err());
                assert!(!prod.state().await.pending,"rejected manual command started hidden transaction");
                prod.execute_authorized("UPDATE dbdeck_policy_test SET value='tx-before'",5,true).await.unwrap();
                let stale=prod.revision();
                prod.transaction(TransactionAction::Rollback).await.unwrap();
                prod.execute_authorized("UPDATE dbdeck_policy_test SET value='tx-after'",5,true).await.unwrap();
                assert!(prod.transaction_with_approval(TransactionAction::Commit,Some(stale)).await.is_err());
                assert!(prod.transaction(TransactionAction::Commit).await.is_err());
                prod.transaction_with_approval(TransactionAction::Commit,Some(prod.revision())).await.unwrap();
                prod.transaction(TransactionAction::AutoCommit).await.unwrap();
                assert_eq!(prod.execute("SELECT value FROM dbdeck_policy_test").await.unwrap().data.rows[0][0],CellValue::Text("tx-after".into()));
                prod.close().await;
            }
        });
    }

    #[test]
    #[ignore = "requires Compose PostgreSQL; connection-local TEMP tables only"]
    fn optimistic_updates_reject_stale_null_deleted_and_unreadable_cells() {
        use crate::database::TableCellEdit;
        tokio::runtime::Runtime::new().unwrap().block_on(async {
            let options = sqlx::postgres::PgConnectOptions::new().host("127.0.0.1").port(55432)
                .database("dbdeck_test").username("dbdeck_test")
                .password(&std::env::var("DBDECK_PG_PASSWORD").expect("test password"));
            let pool = sqlx::postgres::PgPoolOptions::new().connect_with(options).await.unwrap();
            let session = SqlSession::new(DbPool::PostgreSql(pool));
            session.execute("CREATE TEMP TABLE dbdeck_conflict_test (id INT PRIMARY KEY, value TEXT, binary_value BYTEA, computed TEXT GENERATED ALWAYS AS (upper(value)) STORED); INSERT INTO dbdeck_conflict_test (id,value) VALUES (1,NULL),(2,'second')").await.unwrap();
            let object = session.execute("SELECT * FROM dbdeck_conflict_test").await.unwrap().editable_object.unwrap();
            let edit = |id: &str, original_value, value: Option<&str>| TableCellEdit {
                primary_key_values: vec![CellValue::Text(id.into())], column: "value".into(),
                original_value, value: value.map(str::to_owned),
            };
            session.update_cells(&object, &[edit("1",CellValue::Null,Some("first"))]).await.unwrap();
            assert!(session.update_cells(&object, &[edit("1",CellValue::Null,Some("stale"))]).await.unwrap_err().contains("Конфликт"));
            session.update_cells(&object, &[edit("1",CellValue::Text("first".into()),None)]).await.unwrap();
            assert!(session.update_cells(&object, &[edit("1",CellValue::Text("first".into()),Some("stale"))]).await.is_err());
            session.execute("DELETE FROM dbdeck_conflict_test WHERE id=2").await.unwrap();
            assert!(session.update_cells(&object, &[edit("1",CellValue::Null,Some("must-rollback")),edit("2",CellValue::Text("second".into()),Some("missing"))]).await.is_err());
            assert_eq!(session.execute("SELECT value FROM dbdeck_conflict_test WHERE id=1").await.unwrap().data.rows[0][0], CellValue::Null);
            for original in [CellValue::Truncated("preview".into()), CellValue::DecodeError("decode".into())] {
                assert!(session.update_cells(&object,&[edit("1",original,Some("bad"))]).await.is_err());
            }
            for column in ["binary_value", "computed"] {
                let mut change = edit("1",CellValue::Null,Some("bad"));
                change.column = column.into();
                assert!(session.update_cells(&object,&[change]).await.is_err());
            }
            session.close().await;
        });
    }

    #[test]
    #[ignore = "requires local test databases and passwords from .env; TEMP tables only"]
    fn sql_sessions_transactions_and_isolation() {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async {
            for (kind, port, variable) in [(DatabaseKind::PostgreSql, 55432, "DBDECK_PG_PASSWORD"), (DatabaseKind::MySql, 53306, "DBDECK_MARIADB_PASSWORD"), (DatabaseKind::MySql, 53307, "DBDECK_MYSQL_PASSWORD")] {
                let profile = ConnectionProfile { id: "sql-session-test".into(), name: "test".into(), kind, host: "127.0.0.1".into(), port, database: "dbdeck_test".into(), username: "dbdeck_test".into(), environment: EnvironmentTag::Local, color: "#000000".into(), read_only: false, tls_mode: TlsMode::Disabled, password_saved: false, gopass_entry: None, tls_files: Default::default(), native_tools_dir: None, transport: Default::default() };
                let pool = DbPool::connect(&profile, &std::env::var(variable).expect("set test database password")).await.unwrap();
                pool.execute_sql("SELECT 1").await.unwrap();
                let first = SqlSession::new(pool.clone());
                let second = SqlSession::new(pool.clone());
                first.execute("CREATE TEMPORARY TABLE dbdeck_session_test (id INT PRIMARY KEY, value TEXT)").await.unwrap();
                first.execute("INSERT INTO dbdeck_session_test VALUES (1, 'original')").await.unwrap();
                assert!(second.execute("SELECT * FROM dbdeck_session_test").await.is_err(), "TEMP objects must not leak between SQL tabs");
                assert_eq!(first.execute("SELECT value FROM dbdeck_session_test").await.unwrap().data.rows[0][0], CellValue::Text("original".into()));
                first.transaction(TransactionAction::Manual).await.unwrap();
                first.execute("UPDATE dbdeck_session_test SET value='rolled back' WHERE id=1").await.unwrap();
                assert!(first.has_pending_transaction());
                assert!(first.transaction(TransactionAction::AutoCommit).await.is_err());
                assert!(first.execute("COMMIT").await.is_err(), "UI owns managed transaction control");
                assert!(first.execute("CREATE TABLE dbdeck_must_not_exist (id INT)").await.is_err());
                first.transaction(TransactionAction::Rollback).await.unwrap();
                assert_eq!(first.execute("SELECT value FROM dbdeck_session_test").await.unwrap().data.rows[0][0], CellValue::Text("original".into()));
                first.execute("UPDATE dbdeck_session_test SET value='committed' WHERE id=1").await.unwrap();
                first.transaction(TransactionAction::Commit).await.unwrap();
                first.transaction(TransactionAction::AutoCommit).await.unwrap();
                assert_eq!(first.execute("SELECT value FROM dbdeck_session_test").await.unwrap().data.rows[0][0], CellValue::Text("committed".into()));
                first.execute("BEGIN").await.unwrap();
                assert!(first.has_pending_transaction());
                first.execute("UPDATE dbdeck_session_test SET value='raw rollback' WHERE id=1").await.unwrap();
                first.execute("ROLLBACK").await.unwrap();
                assert!(!first.has_pending_transaction());
                assert_eq!(first.execute("SELECT value FROM dbdeck_session_test").await.unwrap().data.rows[0][0], CellValue::Text("committed".into()));
                first.transaction(TransactionAction::Manual).await.unwrap();
                assert!(first.execute("SELECT missing_field FROM dbdeck_session_test").await.is_err());
                assert!(first.state().await.failed);
                assert!(first.transaction(TransactionAction::Commit).await.is_err());
                first.transaction(TransactionAction::Rollback).await.unwrap();
                first.transaction(TransactionAction::AutoCommit).await.unwrap();
                // More tabs than the shared pool size must still work.
                let mut tabs = Vec::new();
                for _ in 0..5 {
                    let tab = SqlSession::new(pool.clone());
                    tab.execute("SELECT 1").await.unwrap();
                    tabs.push(tab);
                }
                for tab in tabs { tab.close().await; }
                let sleep = if matches!(kind, DatabaseKind::PostgreSql) { "SELECT pg_sleep(20)" } else { "SELECT SLEEP(20)" };
                let started = std::time::Instant::now();
                let error = first.execute_with_timeout(sleep, 1).await.unwrap_err();
                assert!(error.message.contains("Превышено время"), "{error}");
                assert!(started.elapsed() < Duration::from_secs(10));
                assert!(first.execute("SELECT value FROM dbdeck_session_test").await.is_ok(), "cancel preserves session/TEMP state");
                first.transaction(TransactionAction::Manual).await.unwrap();
                let error = first.execute_with_timeout(sleep, 1).await.unwrap_err();
                assert!(error.message.contains("Превышено время"));
                assert!(first.state().await.pending && first.state().await.failed);
                assert!(first.transaction(TransactionAction::Commit).await.is_err());
                first.transaction(TransactionAction::Rollback).await.unwrap();
                first.transaction(TransactionAction::AutoCommit).await.unwrap();
                let cancelling = first.clone();
                let stopper = tokio::spawn(async move { tokio::time::sleep(Duration::from_millis(200)).await; cancelling.request_cancel(); });
                let cancel_started = std::time::Instant::now();
                let error = first.execute_with_timeout(sleep, 0).await.unwrap_err();
                stopper.await.unwrap();
                assert!(error.message.contains("пользователем"), "{error}");
                assert!(cancel_started.elapsed() < Duration::from_secs(5));
                assert!(!first.state().await.pending && !first.state().await.failed);
                if matches!(kind, DatabaseKind::MySql) {
                    first.execute("SET SESSION autocommit=0").await.unwrap();
                    assert!(first.state().await.manual && first.has_pending_transaction());
                    first.execute("UPDATE dbdeck_session_test SET value='must-rollback' WHERE id=1").await.unwrap();
                    assert!(first.transaction(TransactionAction::AutoCommit).await.is_err());
                    first.transaction(TransactionAction::Rollback).await.unwrap();
                    first.transaction(TransactionAction::AutoCommit).await.unwrap();
                    let value = first.execute("SELECT value FROM dbdeck_session_test").await.unwrap();
                    assert_eq!(value.data.rows[0][0], CellValue::Text("committed".into()));
                    assert!(first.execute("SET @@session.autocommit=0; SELECT missing_field FROM dbdeck_session_test").await.is_err());
                    assert!(first.state().await.manual && first.has_pending_transaction());
                    first.transaction(TransactionAction::Rollback).await.unwrap();
                    first.transaction(TransactionAction::AutoCommit).await.unwrap();
                    first.execute("UPDATE dbdeck_session_test SET value='after-auto' WHERE id=1").await.unwrap();
                    assert!(!first.has_pending_transaction());
                }
                first.execute("SELECT 1").await.unwrap();
                first.close().await;
                assert!(first.execute("SELECT 1").await.is_err());
                assert!(second.execute("SELECT 1").await.is_ok());
                second.close().await;
            }
        });
    }
}

#[cfg(test)]
#[path = "../tests/support/postgres_idle_eof.rs"]
pub(crate) mod postgres_idle_eof;
