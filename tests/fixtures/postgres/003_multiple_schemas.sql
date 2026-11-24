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

CREATE TABLE IF NOT EXISTS crm.comparison_index_options (value integer);
CREATE TABLE IF NOT EXISTS audit.comparison_index_options (value integer);
CREATE TABLE IF NOT EXISTS audit.comparison_empty ();
CREATE INDEX IF NOT EXISTS comparison_brin ON crm.comparison_index_options
    USING brin (value int4_minmax_multi_ops(values_per_range = 16));
CREATE INDEX IF NOT EXISTS comparison_brin ON audit.comparison_index_options
    USING brin (value int4_minmax_multi_ops(values_per_range = 32));
