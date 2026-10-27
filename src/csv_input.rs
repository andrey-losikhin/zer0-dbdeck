//! Streaming UTF-8 CSV reader, bounded independently of the total file size.
use std::io::{BufRead, BufReader};
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;

pub const MAX_RECORD_BYTES: usize = 1024 * 1024;
const MAX_FIELDS: usize = 1024;
#[derive(Clone, Debug)]
pub struct Field {
    pub value: String,
    pub quoted: bool,
}
#[derive(Clone, Debug)]
pub struct Options {
    pub delimiter: u8,
    pub header: bool,
    pub null: String,
    pub expected_headers: Option<Vec<String>>,
}
impl Default for Options {
    fn default() -> Self {
        Self {
            delimiter: b',',
            header: true,
            null: "\\N".into(),
            expected_headers: None,
        }
    }
}
pub struct Reader<R> {
    input: R,
    delimiter: u8,
    first: bool,
    pending: std::collections::VecDeque<u8>,
    pub record: u64,
}
impl<R: BufRead> Reader<R> {
    pub fn new(input: R, delimiter: u8) -> Result<Self, String> {
        if ![b',', b';', b'\t', b'|'].contains(&delimiter) {
            return Err("Неподдерживаемый разделитель CSV".into());
        }
        Ok(Self {
            input,
            delimiter,
            first: true,
            pending: Default::default(),
            record: 0,
        })
    }
    fn byte(&mut self) -> Result<Option<u8>, String> {
        if let Some(byte) = self.pending.pop_front() {
            return Ok(Some(byte));
        }
        let byte = self
            .input
            .fill_buf()
            .map_err(|e| format!("Ошибка чтения CSV: {e}"))?
            .first()
            .copied();
        if byte.is_some() {
            self.input.consume(1);
        }
        Ok(byte)
    }
    pub fn next(&mut self) -> Result<Option<Vec<Field>>, String> {
        // 0: unquoted; 1: inside quotes; 2: quote closed (or doubled).
        let mut state = 0;
        let mut quoted = false;
        let mut field = Vec::new();
        let mut fields = Vec::new();
        let mut bytes = 0;
        let mut started = false;
        if self.first {
            self.first = false;
            let mut prefix = Vec::new();
            for _ in 0..3 {
                if let Some(byte) = self.byte()? {
                    prefix.push(byte);
                } else {
                    break;
                }
            }
            if prefix != [0xef, 0xbb, 0xbf] {
                self.pending = prefix.into();
            }
        }
        loop {
            let byte = self.byte()?;
            if byte.is_none() && !started {
                return Ok(None);
            }
            if let Some(byte) = byte {
                started = true;
                bytes += 1;
                if bytes > MAX_RECORD_BYTES {
                    return Err(format!("CSV: запись {} превышает 1 МиБ", self.record + 1));
                }
                if state == 1 {
                    if byte == b'"' {
                        state = 2;
                    } else {
                        field.push(byte);
                    }
                    continue;
                }
                if state == 2 && byte == b'"' {
                    field.push(b'"');
                    state = 1;
                    continue;
                }
                if state == 0 && byte == b'"' {
                    if !field.is_empty() {
                        return Err(format!(
                            "CSV: кавычка внутри поля, запись {}",
                            self.record + 1
                        ));
                    }
                    state = 1;
                    quoted = true;
                    continue;
                }
                if byte != self.delimiter && byte != b'\n' && byte != b'\r' {
                    if state == 2 {
                        return Err(format!(
                            "CSV: символ после закрывающей кавычки, запись {}",
                            self.record + 1
                        ));
                    }
                    field.push(byte);
                    continue;
                }
            } else if state == 1 {
                return Err(format!(
                    "CSV: незакрытая кавычка, запись {}",
                    self.record + 1
                ));
            }
            let value = String::from_utf8(std::mem::take(&mut field))
                .map_err(|_| "CSV должен быть в UTF-8".to_owned())?;
            fields.push(Field { value, quoted });
            if fields.len() > MAX_FIELDS {
                return Err("CSV: больше 1024 столбцов".into());
            }
            quoted = false;
            state = 0;
            if byte != Some(self.delimiter) {
                if byte == Some(b'\r') {
                    let next = match self.pending.front() {
                        Some(byte) => Some(*byte),
                        None => self
                            .input
                            .fill_buf()
                            .map_err(|e| e.to_string())?
                            .first()
                            .copied(),
                    };
                    if next == Some(b'\n') {
                        self.byte()?;
                    }
                }
                self.record += 1;
                return Ok(Some(fields));
            }
        }
    }
}
pub fn open(path: &Path, delimiter: u8) -> Result<Reader<BufReader<std::fs::File>>, String> {
    let file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NONBLOCK)
        .open(path)
        .map_err(|e| e.to_string())?;
    if !file.metadata().map_err(|e| e.to_string())?.is_file() {
        return Err("Выберите обычный CSV-файл".into());
    }
    Reader::new(BufReader::new(file), delimiter)
}
pub fn value(field: &Field, null: &str) -> Option<String> {
    if !field.quoted && field.value == null {
        None
    } else {
        Some(field.value.clone())
    }
}
#[derive(Clone, Debug)]
pub struct Preview {
    pub headers: Vec<String>,
    pub rows: Vec<Vec<Field>>,
}
pub fn preview(path: &Path, options: &Options) -> Result<Preview, String> {
    let mut reader = open(path, options.delimiter)?;
    let first = reader.next()?.ok_or("CSV-файл пуст")?;
    let headers: Vec<String> = if options.header {
        first.iter().map(|f| f.value.clone()).collect()
    } else {
        (1..=first.len()).map(|i| format!("Столбец {i}")).collect()
    };
    let mut rows = if options.header {
        Vec::new()
    } else {
        vec![first]
    };
    let mut bytes = 0;
    while rows.len() < 10 && bytes < 128 * 1024 {
        let Some(row) = reader.next()? else {
            break;
        };
        if row.len() != headers.len() {
            return Err(format!(
                "CSV: в записи {} другое число полей",
                reader.record
            ));
        }
        bytes += row.iter().map(|f| f.value.len()).sum::<usize>();
        rows.push(row);
    }
    if rows.is_empty() {
        return Err("CSV не содержит строк данных".into());
    }
    Ok(Preview { headers, rows })
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn quoted_multiline_and_unicode() {
        let mut r = Reader::new(
            std::io::Cursor::new(
                "\u{feff}id,name,note\r\n1,\"Привет, мир\",\"строка\n\"\"вторая\"\"\"\r\n2,,\\N",
            ),
            b',',
        )
        .unwrap();
        assert_eq!(r.next().unwrap().unwrap()[0].value, "id");
        let row = r.next().unwrap().unwrap();
        assert_eq!(row[1].value, "Привет, мир");
        assert_eq!(row[2].value, "строка\n\"вторая\"");
        let row = r.next().unwrap().unwrap();
        assert_eq!(value(&row[1], "\\N"), Some(String::new()));
        assert_eq!(value(&row[2], "\\N"), None);
        assert!(r.next().unwrap().is_none());
        let tiny = std::io::BufReader::with_capacity(1, std::io::Cursor::new("\u{feff}a,b\n"));
        assert_eq!(
            Reader::new(tiny, b',').unwrap().next().unwrap().unwrap()[0].value,
            "a"
        );
    }
    #[test]
    fn short_crlf_records_survive_prefix_buffering() {
        for input in ["a\r\nb\r\n", "a\rb\r", "a\nb\n"] {
            let mut reader = Reader::new(
                std::io::BufReader::with_capacity(1, std::io::Cursor::new(input)),
                b',',
            )
            .unwrap();
            assert_eq!(reader.next().unwrap().unwrap()[0].value, "a");
            assert_eq!(reader.next().unwrap().unwrap()[0].value, "b");
            assert!(reader.next().unwrap().is_none());
        }
    }
    #[test]
    fn malformed_and_record_limit() {
        for input in ["\"unclosed", "a\"b", "\"a\"x", "\"a\" \n"] {
            assert!(
                Reader::new(std::io::Cursor::new(input), b',')
                    .unwrap()
                    .next()
                    .is_err()
            );
        }
        let input = vec![b'x'; MAX_RECORD_BYTES + 1];
        assert!(
            Reader::new(std::io::Cursor::new(input), b',')
                .unwrap()
                .next()
                .is_err()
        );
        let row = Reader::new(std::io::Cursor::new("\"\\N\";\\N;"), b';')
            .unwrap()
            .next()
            .unwrap()
            .unwrap();
        assert_eq!(value(&row[0], "\\N"), Some("\\N".into()));
        assert_eq!(value(&row[1], "\\N"), None);
        assert_eq!(row[2].value, "");
    }
}
