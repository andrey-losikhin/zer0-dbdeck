//! Build an estimated plan for a single statement, preserving the original SQL.
use crate::settings::DatabaseKind;
use sqlparser::{
    dialect::{Dialect, MySqlDialect, PostgreSqlDialect},
    tokenizer::{Token, Tokenizer},
};

#[derive(Debug)]
pub struct EstimatedPlan(String);

impl EstimatedPlan {
    pub fn new(kind: DatabaseKind, sql: &str) -> Result<Self, String> {
        let postgres = matches!(kind, DatabaseKind::PostgreSql);
        let dialect: &dyn Dialect = if postgres {
            &PostgreSqlDialect {}
        } else {
            &MySqlDialect {}
        };
        if !postgres && (sql.contains("/*!") || sql.to_ascii_uppercase().contains("/*M!")) {
            return Err("EXPLAIN не поддерживает исполняемые комментарии MySQL/MariaDB.".into());
        }
        let tokens = Tokenizer::new(dialect, sql)
            .tokenize()
            .map_err(|error| error.to_string())?;
        let mut statements = tokens.split(|token| matches!(token, Token::SemiColon));
        let mut command = statements
            .next()
            .unwrap_or_default()
            .iter()
            .filter(|token| !matches!(token, Token::Whitespace(_)));
        let first = command.next();
        if first.is_none()
            || statements
                .flatten()
                .any(|token| !matches!(token, Token::Whitespace(_)))
        {
            return Err("Выделите один SQL-оператор для EXPLAIN.".into());
        }
        let parenthesized = postgres && matches!(first, Some(Token::LParen));
        let first = if parenthesized {
            command.find(|token| !matches!(token, Token::LParen))
        } else {
            first
        };
        let supported = match first {
            Some(Token::Word(word)) if word.quote_style.is_none() => {
                match word.value.to_ascii_uppercase().as_str() {
                    "SELECT" | "WITH" => true,
                    "TABLE" | "VALUES" if parenthesized => true,
                    _ if parenthesized => false,
                    "UPDATE" | "DELETE" => true,
                    "INSERT" | "TABLE" => !matches!(kind, DatabaseKind::MariaDb),
                    "VALUES" | "MERGE" => postgres,
                    "REPLACE" => matches!(kind, DatabaseKind::MySql),
                    _ => false,
                }
            }
            _ => false,
        };
        if !supported {
            return Err(format!(
                "Оператор не поддерживается командой EXPLAIN в {}.",
                kind.label()
            ));
        }
        let prefix = match kind {
            DatabaseKind::PostgreSql => "EXPLAIN (ANALYZE FALSE, FORMAT TEXT)",
            DatabaseKind::MySql => "EXPLAIN FORMAT=TRADITIONAL",
            DatabaseKind::MariaDb => "EXPLAIN",
        };
        Ok(Self(format!("{prefix}\n{sql}")))
    }

    pub fn sql(&self) -> &str {
        &self.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use DatabaseKind::*;

    #[test]
    fn estimated_plan_preserves_sql_and_uses_engine_syntax() {
        for (kind, prefix) in [
            (PostgreSql, "EXPLAIN (ANALYZE FALSE, FORMAT TEXT)"),
            (MySql, "EXPLAIN FORMAT=TRADITIONAL"),
            (MariaDb, "EXPLAIN"),
        ] {
            let sql = "-- comment;\nWITH t AS (SELECT ';') SELECT * FROM t; -- tail";
            assert_eq!(
                EstimatedPlan::new(kind, sql).unwrap().sql(),
                format!("{prefix}\n{sql}")
            );
            assert!(EstimatedPlan::new(kind, "UPDATE t SET value = 'ANALYZE'").is_ok());
            assert!(EstimatedPlan::new(kind, "DELETE FROM t WHERE id=1").is_ok());
            for invalid in [
                "-- comment only",
                "SELECT 1; DELETE FROM t",
                "; SELECT 1",
                "SELECT 'unclosed",
                "ANALYZE SELECT 1",
                "EXPLAIN ANALYZE SELECT 1",
                "EXPLAIN FOR CONNECTION 1",
                "SET autocommit=0",
            ] {
                assert!(
                    EstimatedPlan::new(kind, invalid).is_err(),
                    "{kind:?}: {invalid}"
                );
            }
        }
        assert!(EstimatedPlan::new(PostgreSql, "SELECT $$;$$").is_ok());
        assert!(EstimatedPlan::new(PostgreSql, "((SELECT 1) UNION ALL (SELECT 2))").is_ok());
        assert!(EstimatedPlan::new(MySql, "SELECT 1 /*!; SELECT 2 */").is_err());
        assert!(EstimatedPlan::new(MariaDb, "/*M!100000 ANALYZE */ SELECT 1").is_err());
    }
}
