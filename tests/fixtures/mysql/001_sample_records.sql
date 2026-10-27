SET NAMES utf8mb4;

CREATE TABLE IF NOT EXISTS sample_records (
    id BIGINT NOT NULL AUTO_INCREMENT PRIMARY KEY,
    name VARCHAR(255) NOT NULL,
    note TEXT NULL,
    created_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_unicode_ci;

INSERT INTO sample_records (name, note, created_at) SELECT 'Алиса', 'Обычная запись', '2025-01-01 09:15:00' WHERE NOT EXISTS (SELECT 1 FROM sample_records WHERE name = 'Алиса');
INSERT INTO sample_records (name, note, created_at) SELECT 'Боб', 'Запятая, кавычки "и" Unicode: café', '2025-01-02 10:20:30' WHERE NOT EXISTS (SELECT 1 FROM sample_records WHERE name = 'Боб');
INSERT INTO sample_records (name, note, created_at) SELECT 'Чарли', NULL, '2025-01-03 11:25:45' WHERE NOT EXISTS (SELECT 1 FROM sample_records WHERE name = 'Чарли');
INSERT INTO sample_records (name, note, created_at) SELECT 'Дина', 'Пробелы в начале и в конце   ', '2025-02-10 08:00:00' WHERE NOT EXISTS (SELECT 1 FROM sample_records WHERE name = 'Дина');
INSERT INTO sample_records (name, note, created_at) SELECT 'Егор', 'Перевод строки: первая строка; вторая строка', '2025-02-11 08:05:00' WHERE NOT EXISTS (SELECT 1 FROM sample_records WHERE name = 'Егор');
INSERT INTO sample_records (name, note, created_at) SELECT 'Жанна', 'Эмодзи и Unicode: 🌌 🐈‍⬛ Привет', '2025-02-12 08:10:00' WHERE NOT EXISTS (SELECT 1 FROM sample_records WHERE name = 'Жанна');
INSERT INTO sample_records (name, note, created_at) SELECT 'Иван', '', '2025-03-01 12:00:00' WHERE NOT EXISTS (SELECT 1 FROM sample_records WHERE name = 'Иван');
INSERT INTO sample_records (name, note, created_at) SELECT 'Кира', 'SQL: SELECT * FROM sample_records WHERE id > 3;', '2025-03-02 12:05:00' WHERE NOT EXISTS (SELECT 1 FROM sample_records WHERE name = 'Кира');
INSERT INTO sample_records (name, note, created_at) SELECT 'Лео', 'Кавычки: ''одинарные'' и "двойные"', '2025-03-03 12:10:00' WHERE NOT EXISTS (SELECT 1 FROM sample_records WHERE name = 'Лео');
INSERT INTO sample_records (name, note, created_at) SELECT 'Мария', 'Путь: /var/lib/dbdeck/data.json', '2025-04-04 13:14:15' WHERE NOT EXISTS (SELECT 1 FROM sample_records WHERE name = 'Мария');
INSERT INTO sample_records (name, note, created_at) SELECT 'Никита', 'Табуляция и спецсимволы: [x] {y} <z> &', '2025-04-05 13:15:15' WHERE NOT EXISTS (SELECT 1 FROM sample_records WHERE name = 'Никита');
INSERT INTO sample_records (name, note, created_at) SELECT 'Ольга', CONCAT('Длинная заметка для проверки: ', REPEAT('текст ', 40)), '2025-05-06 14:16:16' WHERE NOT EXISTS (SELECT 1 FROM sample_records WHERE name = 'Ольга');
INSERT INTO sample_records (name, note, created_at) SELECT 'Пётр', 'Ноль: 0; отрицательное число: -42; дробь: 3.14159', '2025-05-07 14:17:17' WHERE NOT EXISTS (SELECT 1 FROM sample_records WHERE name = 'Пётр');
INSERT INTO sample_records (name, note, created_at) SELECT 'Роман', 'Смешанные языки: Русский / English / 日本語', '2025-06-08 15:18:18' WHERE NOT EXISTS (SELECT 1 FROM sample_records WHERE name = 'Роман');
INSERT INTO sample_records (name, note, created_at) SELECT 'Света', 'CSV-проверка: значение, содержащее запятую', '2025-06-09 15:19:19' WHERE NOT EXISTS (SELECT 1 FROM sample_records WHERE name = 'Света');
INSERT INTO sample_records (name, note, created_at) SELECT 'Тимур', 'Последняя запись первой партии', '2025-07-10 16:20:20' WHERE NOT EXISTS (SELECT 1 FROM sample_records WHERE name = 'Тимур');

CREATE TABLE IF NOT EXISTS typed_values (
    id BIGINT NOT NULL PRIMARY KEY,
    label VARCHAR(255) NOT NULL,
    date_value DATE NULL,
    timestamp_value DATETIME(6) NULL,
    amount DECIMAL(12, 2) NULL,
    enabled BOOLEAN NULL,
    metadata JSON NULL,
    payload VARBINARY(32) NULL,
    optional_text TEXT NULL
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_unicode_ci;

INSERT INTO typed_values (id, label, date_value, timestamp_value, amount, enabled, metadata, payload, optional_text) SELECT 1, 'json-object', '2024-02-29', '2024-02-29 23:59:59.123456', 123456.78, TRUE, '{"kind":"nested","tags":["json","café"],"valid":true,"count":3}', UNHEX('00ff10a5'), 'обычный текст' WHERE NOT EXISTS (SELECT 1 FROM typed_values WHERE id = 1);
INSERT INTO typed_values (id, label, date_value, timestamp_value, amount, enabled, metadata, payload, optional_text) SELECT 2, 'json-null', NULL, NULL, NULL, FALSE, 'null', NULL, NULL WHERE NOT EXISTS (SELECT 1 FROM typed_values WHERE id = 2);
INSERT INTO typed_values (id, label, date_value, timestamp_value, amount, enabled, metadata, payload, optional_text) SELECT 3, 'empty-values', '2000-01-01', '2000-01-01 00:00:00.000000', 0.00, FALSE, '{}', UNHEX(''), '' WHERE NOT EXISTS (SELECT 1 FROM typed_values WHERE id = 3);
INSERT INTO typed_values (id, label, date_value, timestamp_value, amount, enabled, metadata, payload, optional_text) SELECT 4, 'large-text', '2038-01-19', '2038-01-19 03:14:07.000000', -9876.50, TRUE, '{"description":"long text"}', UNHEX('deadbeef'), REPEAT('длинный-текст-', 120) WHERE NOT EXISTS (SELECT 1 FROM typed_values WHERE id = 4);
INSERT INTO typed_values (id, label, date_value, timestamp_value, amount, enabled, metadata, payload, optional_text) SELECT 5, 'unicode', '1999-12-31', '1999-12-31 23:30:00.000000', 0.01, TRUE, '{"город":"Токио","эмодзи":"🛰️","массив":[1,2,3]}', UNHEX('cafe'), 'Кириллица, العربية, 日本語' WHERE NOT EXISTS (SELECT 1 FROM typed_values WHERE id = 5);
INSERT INTO typed_values (id, label, date_value, timestamp_value, amount, enabled, metadata, payload, optional_text) SELECT 6, 'precision', '2026-10-16', '2026-10-16 16:42:08.002001', 99999999.99, FALSE, '[1,"two",null,true]', UNHEX('0123456789abcdef'), 'точность времени и суммы' WHERE NOT EXISTS (SELECT 1 FROM typed_values WHERE id = 6);
INSERT INTO typed_values (id, label, date_value, timestamp_value, amount, enabled, metadata, payload, optional_text) SELECT 7, 'negative', '1970-01-01', '1970-01-01 00:00:00.000000', -0.01, NULL, '{"negative":-42,"fraction":3.14159}', UNHEX('ff'), 'NULL отдельно в bool' WHERE NOT EXISTS (SELECT 1 FROM typed_values WHERE id = 7);
INSERT INTO typed_values (id, label, date_value, timestamp_value, amount, enabled, metadata, payload, optional_text) SELECT 8, 'leap-day', '2020-02-29', '2020-02-29 12:34:56.789000', 42.42, TRUE, '{"nested":{"key":"value"},"spaces":"a b c"}', UNHEX('0100ff'), 'високосная дата' WHERE NOT EXISTS (SELECT 1 FROM typed_values WHERE id = 8);
