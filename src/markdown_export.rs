//! Streaming Markdown report; database values never become Markdown syntax.
use crate::database::{CellValue, QueryResult};
use std::io::{self, Write};

pub fn fenced(out: &mut impl Write, language: &str, text: &str) -> io::Result<()> {
    let longest = text.split(|c| c != '`').map(str::len).max().unwrap_or(0);
    let fence = "`".repeat(3.max(longest + 1));
    writeln!(out, "{fence}{language}\n{text}\n{fence}\n")
}

fn cell(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('\\', "&#92;")
        .replace('|', "&#124;")
        .replace('`', "&#96;")
        .replace('*', "&#42;")
        .replace('_', "&#95;")
        .replace('[', "&#91;")
        .replace(']', "&#93;")
        .replace('\r', "&#13;")
        .replace('\n', "<br>")
}

pub fn query(
    out: &mut impl Write,
    index: usize,
    sql: &str,
    result: &Result<QueryResult, String>,
    elapsed_ms: u128,
) -> io::Result<()> {
    writeln!(
        out,
        "## {index}. {}\n",
        if result.is_ok() {
            "Успешно"
        } else {
            "Ошибка"
        }
    )?;
    fenced(out, "sql", sql)?;
    match result {
        Err(error) => fenced(out, "text", error)?,
        Ok(result) => {
            let data = &result.data;
            if !data.columns.is_empty() {
                writeln!(
                    out,
                    "| {} |",
                    data.columns
                        .iter()
                        .map(|s| cell(s))
                        .collect::<Vec<_>>()
                        .join(" | ")
                )?;
                writeln!(out, "| {} |", vec!["---"; data.columns.len()].join(" | "))?;
                for row in &data.rows {
                    let values = row
                        .iter()
                        .map(|value| match value {
                            CellValue::Null => "**NULL**".to_owned(),
                            CellValue::Text(value) => {
                                if value.is_empty() {
                                    "*(пустая строка)*".to_owned()
                                } else {
                                    cell(value)
                                }
                            }
                            CellValue::Truncated(value) => {
                                format!("{} **[обрезано]**", cell(value))
                            }
                            CellValue::DecodeError(error) => {
                                format!("**[ошибка декодирования]** {}", cell(error))
                            }
                        })
                        .collect::<Vec<_>>();
                    writeln!(out, "| {} |", values.join(" | "))?;
                }
            }
            writeln!(
                out,
                "\nСтрок: {}. Сохранено: {}.",
                result.row_count,
                data.rows.len()
            )?;
            if data.truncated {
                writeln!(
                    out,
                    "Результат обрезан: лимит 1000 строк или лимит объёма предпросмотра."
                )?;
            }
            if data.truncated_columns {
                writeln!(
                    out,
                    "Столбцы обрезаны: сохранены первые {}.",
                    data.columns.len()
                )?;
            }
        }
    }
    writeln!(out, "\nВремя: {elapsed_ms} мс.\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn markdown_escapes_values_and_fences() {
        assert_eq!(
            cell("<b>|`*_[x]\\\r\n"),
            "&lt;b&gt;&#124;&#96;&#42;&#95;&#91;x&#93;&#92;&#13;<br>"
        );
        let mut out = Vec::new();
        fenced(&mut out, "sql", "SELECT '```';").unwrap();
        assert!(String::from_utf8(out).unwrap().starts_with("````sql\n"));
    }
}
