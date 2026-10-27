SET NAMES utf8mb4;

CREATE TABLE IF NOT EXISTS audit_events (
    id BIGINT NOT NULL PRIMARY KEY,
    actor VARCHAR(120) NOT NULL,
    action_name VARCHAR(120) NOT NULL,
    occurred_at TIMESTAMP(6) NOT NULL,
    context JSON NOT NULL,
    duration_ms INT UNSIGNED,
    succeeded BOOLEAN NOT NULL
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_0900_ai_ci;

INSERT INTO audit_events (id, actor, action_name, occurred_at, context, duration_ms, succeeded)
SELECT n, CONCAT('user-', n), ELT(MOD(n - 1, 4) + 1, 'login', 'select', 'update', 'export'),
       TIMESTAMP('2025-04-01 08:00:00') + INTERVAL n MINUTE,
       JSON_OBJECT('ip', CONCAT('192.0.2.', n), 'tags', JSON_ARRAY('demo', 'audit', n)),
       n * 37, MOD(n, 5) <> 0
FROM (SELECT 1 AS n UNION ALL SELECT 2 UNION ALL SELECT 3 UNION ALL SELECT 4 UNION ALL SELECT 5
      UNION ALL SELECT 6 UNION ALL SELECT 7 UNION ALL SELECT 8 UNION ALL SELECT 9 UNION ALL SELECT 10) AS rows_to_add
WHERE NOT EXISTS (SELECT 1 FROM audit_events WHERE id = n);

CREATE TABLE IF NOT EXISTS demo_orders (
    id BIGINT NOT NULL PRIMARY KEY,
    customer_name VARCHAR(150) NOT NULL,
    created_at DATETIME(6) NOT NULL,
    total DECIMAL(12, 2) NOT NULL,
    currency CHAR(3) NOT NULL,
    status ENUM('new', 'paid', 'shipped', 'cancelled') NOT NULL,
    comment TEXT
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_0900_ai_ci;

INSERT INTO demo_orders (id, customer_name, created_at, total, currency, status, comment)
SELECT n, CONCAT('Покупатель ', n), TIMESTAMP('2025-05-01 10:00:00') + INTERVAL n DAY,
       n * 48.75, IF(MOD(n, 2) = 0, 'EUR', 'USD'),
       ELT(MOD(n - 1, 4) + 1, 'new', 'paid', 'shipped', 'cancelled'),
       IF(MOD(n, 4) = 0, NULL, CONCAT('Заказ №', n, ': тест текста'))
FROM (SELECT 1 AS n UNION ALL SELECT 2 UNION ALL SELECT 3 UNION ALL SELECT 4 UNION ALL SELECT 5
      UNION ALL SELECT 6 UNION ALL SELECT 7 UNION ALL SELECT 8 UNION ALL SELECT 9 UNION ALL SELECT 10) AS rows_to_add
WHERE NOT EXISTS (SELECT 1 FROM demo_orders WHERE id = n);

CREATE TABLE IF NOT EXISTS feature_flags (
    flag_key VARCHAR(120) NOT NULL PRIMARY KEY,
    description VARCHAR(255),
    enabled BOOLEAN NOT NULL,
    rollout_percent TINYINT UNSIGNED NOT NULL,
    rules JSON,
    updated_at TIMESTAMP(6) NOT NULL
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_0900_ai_ci;

INSERT INTO feature_flags (flag_key, description, enabled, rollout_percent, rules, updated_at)
VALUES
    ('new-dashboard', 'Новая панель', TRUE, 25, JSON_OBJECT('groups', JSON_ARRAY('internal', 'beta')), '2025-06-01 12:00:00.123456'),
    ('compact-grid', 'Компактная таблица', FALSE, 0, JSON_OBJECT('platforms', JSON_ARRAY('linux', 'macos')), '2025-06-02 12:00:00.654321'),
    ('json-cell-editor', 'Редактор JSON', TRUE, 100, JSON_OBJECT('min_version', '0.2.0'), '2025-06-03 12:00:00.000001')
ON DUPLICATE KEY UPDATE flag_key = VALUES(flag_key);
