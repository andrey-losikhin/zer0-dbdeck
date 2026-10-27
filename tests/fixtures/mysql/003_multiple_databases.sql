SET NAMES utf8mb4;

CREATE DATABASE IF NOT EXISTS dbdeck_analytics CHARACTER SET utf8mb4;
GRANT ALL PRIVILEGES ON dbdeck_analytics.* TO 'dbdeck_test'@'%';

CREATE TABLE IF NOT EXISTS dbdeck_analytics.sample_records (
    id BIGINT NOT NULL PRIMARY KEY,
    name VARCHAR(150) NOT NULL,
    note TEXT,
    created_at DATETIME NOT NULL
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;

INSERT INTO dbdeck_analytics.sample_records (id, name, note, created_at)
SELECT 1, 'Аналитика', 'Отдельная БД на том же сервере', '2025-01-01 09:00:00'
WHERE NOT EXISTS (SELECT 1 FROM dbdeck_analytics.sample_records WHERE id = 1);

CREATE TABLE IF NOT EXISTS dbdeck_analytics.reports (
    id BIGINT NOT NULL PRIMARY KEY,
    title VARCHAR(150) NOT NULL
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;

INSERT INTO dbdeck_analytics.reports (id, title)
SELECT 1, 'Сводный отчёт'
WHERE NOT EXISTS (SELECT 1 FROM dbdeck_analytics.reports WHERE id = 1);
