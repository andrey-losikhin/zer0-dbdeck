SET NAMES utf8mb4;

CREATE TABLE IF NOT EXISTS inventory_items (
    id BIGINT NOT NULL PRIMARY KEY,
    sku VARCHAR(40) NOT NULL UNIQUE,
    item_name VARCHAR(200) NOT NULL,
    quantity INT NOT NULL,
    unit_price DECIMAL(10, 2) NOT NULL,
    stocked_on DATE,
    attributes JSON,
    available BOOLEAN NOT NULL DEFAULT TRUE
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_unicode_ci;

INSERT INTO inventory_items (id, sku, item_name, quantity, unit_price, stocked_on, attributes, available)
SELECT n, CONCAT('SKU-', LPAD(n, 4, '0')), CONCAT('Товар ', n), n * 3,
       n * 17.25, DATE_ADD('2025-01-01', INTERVAL n DAY),
       JSON_OBJECT('category', IF(MOD(n, 2) = 0, 'hardware', 'office'), 'fragile', MOD(n, 3) = 0),
       MOD(n, 4) <> 0
FROM (SELECT 1 AS n UNION ALL SELECT 2 UNION ALL SELECT 3 UNION ALL SELECT 4 UNION ALL SELECT 5
      UNION ALL SELECT 6 UNION ALL SELECT 7 UNION ALL SELECT 8 UNION ALL SELECT 9 UNION ALL SELECT 10) AS rows_to_add
WHERE NOT EXISTS (SELECT 1 FROM inventory_items WHERE id = n);

CREATE TABLE IF NOT EXISTS support_tickets (
    id BIGINT NOT NULL PRIMARY KEY,
    subject VARCHAR(255) NOT NULL,
    details TEXT,
    priority ENUM('low', 'normal', 'high', 'urgent') NOT NULL,
    opened_at DATETIME(6) NOT NULL,
    closed_at DATETIME(6),
    metadata JSON
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_unicode_ci;

INSERT INTO support_tickets (id, subject, details, priority, opened_at, closed_at, metadata)
SELECT n, CONCAT('Обращение ', n), IF(MOD(n, 4) = 0, NULL, CONCAT('Описание с Unicode: запрос №', n, ' — проверка')),
       ELT(MOD(n - 1, 4) + 1, 'low', 'normal', 'high', 'urgent'),
       TIMESTAMP('2025-03-01 09:00:00') + INTERVAL n HOUR,
       IF(MOD(n, 3) = 0, TIMESTAMP('2025-03-02 12:00:00') + INTERVAL n HOUR, NULL),
       JSON_OBJECT('channel', IF(MOD(n, 2) = 0, 'email', 'web'), 'attempt', n)
FROM (SELECT 1 AS n UNION ALL SELECT 2 UNION ALL SELECT 3 UNION ALL SELECT 4 UNION ALL SELECT 5
      UNION ALL SELECT 6 UNION ALL SELECT 7 UNION ALL SELECT 8 UNION ALL SELECT 9 UNION ALL SELECT 10) AS rows_to_add
WHERE NOT EXISTS (SELECT 1 FROM support_tickets WHERE id = n);
