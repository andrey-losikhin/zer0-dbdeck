use crate::database::CellValue;

pub(crate) fn write_csv_header(
    file: &mut impl std::io::Write,
    columns: &[String],
) -> Result<(), String> {
    let header = columns
        .iter()
        .map(|value| csv_field(value))
        .collect::<Vec<_>>();
    writeln!(file, "{}", header.join(",")).map_err(|error| format!("Ошибка записи CSV: {error}"))
}

pub(crate) fn write_csv_row(
    file: &mut impl std::io::Write,
    row: &[CellValue],
) -> Result<(), String> {
    let fields = row.iter().map(csv_cell).collect::<Vec<_>>();
    writeln!(file, "{}", fields.join(",")).map_err(|error| format!("Ошибка записи CSV: {error}"))
}

pub(crate) fn csv_cell(value: &CellValue) -> String {
    match value {
        CellValue::Text(value) => {
            if value.starts_with("\\N") {
                csv_field(&format!("\\{value}"))
            } else {
                csv_field(value)
            }
        }
        CellValue::Truncated(value) => csv_field(&format!("{value}… [обрезано до 1024 символов]")),
        CellValue::DecodeError(value) => csv_field(&format!("Ошибка декодирования: {value}")),
        CellValue::Null => "\\N".to_owned(),
    }
}

pub(crate) fn csv_field(value: &str) -> String {
    let first_non_space = value
        .trim_start_matches([' ', '\t', '\r', '\n'])
        .chars()
        .next();
    let unsafe_leading_control = value
        .chars()
        .next()
        .is_some_and(|character| matches!(character, '\t' | '\r' | '\n'));
    let safe_value =
        if unsafe_leading_control || matches!(first_non_space, Some('=' | '+' | '-' | '@')) {
            format!("'{value}")
        } else {
            value.to_owned()
        };
    format!("\"{}\"", safe_value.replace('"', "\"\""))
}
