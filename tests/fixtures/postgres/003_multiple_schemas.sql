CREATE SCHEMA IF NOT EXISTS crm;
CREATE SCHEMA IF NOT EXISTS audit;

CREATE TABLE IF NOT EXISTS crm.events (
    id BIGINT PRIMARY KEY,
    description TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS audit.events (
    id BIGINT PRIMARY KEY,
    description TEXT NOT NULL
);

INSERT INTO crm.events (id, description) VALUES
    (1, 'CRM: создан клиент'),
    (2, 'CRM: назначена встреча')
ON CONFLICT (id) DO NOTHING;

INSERT INTO audit.events (id, description) VALUES
    (1, 'AUDIT: пользователь вошёл'),
    (2, 'AUDIT: экспортирован отчёт')
ON CONFLICT (id) DO NOTHING;
