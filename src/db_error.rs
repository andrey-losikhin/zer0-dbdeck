//! Database errors that tell the UI whether the connection itself broke.
//! The kind is derived from the original `sqlx::Error`, never from message text:
//! texts are rewritten for users and localized by servers.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DbErrorKind {
    /// The connection or pool is gone (network, server restart, idle kill).
    Disconnected,
    /// The pool could not hand out a connection in time. Ambiguous: a busy pool and a
    /// dead server look the same, so the caller must confirm with `DbPool::is_connection_lost`.
    PoolTimedOut,
    Other,
}

#[derive(Clone, Debug)]
pub struct DbError {
    pub kind: DbErrorKind,
    /// Text shown to the user; identical to the former `String` error.
    pub message: String,
}

impl std::fmt::Display for DbError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for DbError {}

impl From<DbError> for String {
    fn from(error: DbError) -> Self {
        error.message
    }
}

impl From<String> for DbError {
    fn from(message: String) -> Self {
        Self {
            kind: DbErrorKind::Other,
            message,
        }
    }
}

impl From<&str> for DbError {
    fn from(message: &str) -> Self {
        message.to_owned().into()
    }
}

pub fn classify(error: &sqlx::Error) -> DbErrorKind {
    match error {
        sqlx::Error::Io(_)
        | sqlx::Error::Tls(_)
        | sqlx::Error::PoolClosed
        | sqlx::Error::WorkerCrashed => DbErrorKind::Disconnected,
        sqlx::Error::PoolTimedOut => DbErrorKind::PoolTimedOut,
        sqlx::Error::Database(database) => {
            // MySQL `code()` is a SQLSTATE; the client-side loss codes are error numbers.
            let number = database
                .try_downcast_ref::<sqlx::mysql::MySqlDatabaseError>()
                .map(|mysql| mysql.number());
            classify_code(database.code().as_deref(), number)
        }
        _ => DbErrorKind::Other,
    }
}

/// PostgreSQL: 57P01-57P03 (server shutdown), 57P05, 25P03 and 25P04 (session-ending
/// timeouts), class 08. MySQL: 2006 (server gone away), 2013 (lost connection).
/// 25P02 (aborted transaction) is deliberately not here: the connection is alive.
pub fn classify_code(sqlstate: Option<&str>, mysql_number: Option<u16>) -> DbErrorKind {
    let by_state = sqlstate.is_some_and(|state| {
        matches!(
            state,
            "57P01" | "57P02" | "57P03" | "57P05" | "25P03" | "25P04"
        ) || state.starts_with("08")
    });
    if by_state || matches!(mysql_number, Some(2006 | 2013)) {
        DbErrorKind::Disconnected
    } else {
        DbErrorKind::Other
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn io_error_is_disconnect() {
        let error = sqlx::Error::Io(std::io::Error::from(std::io::ErrorKind::ConnectionReset));
        assert_eq!(classify(&error), DbErrorKind::Disconnected);
    }

    #[test]
    fn closed_pool_is_disconnect() {
        assert_eq!(
            classify(&sqlx::Error::PoolClosed),
            DbErrorKind::Disconnected
        );
    }

    #[test]
    fn tls_error_is_disconnect() {
        let error = sqlx::Error::Tls("handshake eof".into());
        assert_eq!(classify(&error), DbErrorKind::Disconnected);
    }

    #[test]
    fn pool_timeout_is_not_disconnect_by_itself() {
        assert_eq!(
            classify(&sqlx::Error::PoolTimedOut),
            DbErrorKind::PoolTimedOut
        );
    }

    #[test]
    fn unrelated_errors_are_other() {
        assert_eq!(classify(&sqlx::Error::RowNotFound), DbErrorKind::Other);
        let protocol = sqlx::Error::Protocol("unexpected".into());
        assert_eq!(classify(&protocol), DbErrorKind::Other);
    }

    #[test]
    fn disconnect_codes() {
        for state in [
            "57P01", "57P02", "57P03", "57P05", "08006", "08S01", "25P03", "25P04",
        ] {
            let kind = classify_code(Some(state), None);
            assert_eq!(kind, DbErrorKind::Disconnected, "{state}");
        }
        for number in [2006, 2013] {
            let kind = classify_code(Some("HY000"), Some(number));
            assert_eq!(kind, DbErrorKind::Disconnected, "{number}");
        }
    }

    #[test]
    fn query_errors_are_not_disconnect() {
        for state in ["42601", "25P02", "23505", "40001"] {
            assert_eq!(classify_code(Some(state), None), DbErrorKind::Other);
        }
        assert_eq!(classify_code(None, None), DbErrorKind::Other);
        assert_eq!(classify_code(Some("42000"), Some(1064)), DbErrorKind::Other);
    }

    #[test]
    fn message_text_is_preserved_in_conversions() {
        let error = DbError::from("Ошибка запроса");
        assert_eq!(error.kind, DbErrorKind::Other);
        assert_eq!(error.to_string(), "Ошибка запроса");
        assert_eq!(String::from(error), "Ошибка запроса");
    }
}
