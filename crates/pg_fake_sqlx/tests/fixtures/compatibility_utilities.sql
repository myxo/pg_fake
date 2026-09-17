SELECT pg_is_in_recovery(), pg_catalog.pg_is_in_recovery();
SELECT '2B/1757980'::pg_lsn::text,
       '0/16AE7F8'::pg_lsn - '0/16AE7F7'::pg_lsn,
       ('0/16AE7F7'::pg_lsn + 16::numeric)::text;

CREATE TABLE phase3_catalog_item (
    id SERIAL PRIMARY KEY,
    label VARCHAR(9) NOT NULL
);
SELECT a.attname,
       format_type(a.atttypid, a.atttypmod),
       a.attnotnull
FROM pg_catalog.pg_attribute AS a
WHERE a.attrelid = 'phase3_catalog_item'::regclass
  AND a.attnum > 0
ORDER BY a.attnum;

CREATE TABLE phase3_truncate_item (id SERIAL PRIMARY KEY);
INSERT INTO phase3_truncate_item DEFAULT VALUES;
TRUNCATE phase3_truncate_item RESTART IDENTITY;
