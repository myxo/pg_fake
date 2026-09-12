CREATE SEQUENCE public.entry_number_seq;
CREATE SEQUENCE public.discarded_sequence;

ALTER TABLE public.accounts ADD COLUMN normalized_name TEXT;
UPDATE public.accounts SET normalized_name = btrim(display_name);
ALTER TABLE public.accounts ALTER COLUMN normalized_name SET NOT NULL;
ALTER TABLE public.accounts ALTER COLUMN normalized_name SET DEFAULT '';
ALTER TABLE public.accounts ALTER COLUMN normalized_name DROP DEFAULT;
ALTER TABLE public.accounts RENAME COLUMN display_name TO original_name;
ALTER TABLE public.accounts ADD COLUMN discarded_column INTEGER DEFAULT 1;
ALTER TABLE public.accounts DROP COLUMN discarded_column;

ALTER TABLE public.entries ALTER COLUMN legacy_position TYPE BIGINT USING legacy_position * 10;
ALTER TABLE public.entries RENAME COLUMN legacy_position TO imported_position;
ALTER TABLE public.entries ADD COLUMN entry_number BIGINT;

WITH numbered AS MATERIALIZED (
    SELECT id, row_number() OVER (ORDER BY id) AS number
    FROM public.entries
)
UPDATE public.entries AS target
SET entry_number = numbered.number
FROM numbered
WHERE target.id = numbered.id;

SELECT setval('public.entry_number_seq', (SELECT max(entry_number) FROM public.entries));
ALTER TABLE public.entries ALTER COLUMN entry_number SET DEFAULT nextval('public.entry_number_seq');
ALTER TABLE public.entries ALTER COLUMN entry_number SET NOT NULL;

ALTER TABLE public.entries
    ADD CONSTRAINT entries_account_fk
    FOREIGN KEY (account_id) REFERENCES public.accounts(id) NOT VALID;
ALTER TABLE public.entries VALIDATE CONSTRAINT entries_account_fk;
ALTER TABLE public.entries ADD CONSTRAINT entries_position_positive CHECK (imported_position > 0);
ALTER TABLE public.entries ADD CONSTRAINT entries_label_unique UNIQUE (label);
ALTER TABLE public.entries DROP CONSTRAINT entries_label_unique;

CREATE UNIQUE INDEX entries_active_number_idx
    ON public.entries (entry_number ASC)
    INCLUDE (label)
    WHERE active;
CREATE INDEX entries_account_position_idx
    ON public.entries (account_id ASC, imported_position DESC)
    INCLUDE (label)
    WHERE active IS NOT NULL AND imported_position IN (100, 200);
ALTER INDEX IF EXISTS public.entries_account_position_idx
    RENAME TO entries_account_position_v2_idx;
CREATE INDEX entries_discarded_idx ON public.entries (label);
DROP INDEX IF EXISTS public.entries_discarded_idx;

CREATE VIEW public.active_entries (entry_id, account_id, entry_number, label) AS
SELECT id, account_id, entry_number, label
FROM public.entries
WHERE active;
CREATE OR REPLACE VIEW public.active_entries (entry_id, account_id, entry_number, label) AS
SELECT id, account_id, entry_number, label
FROM public.entries
WHERE active IS NOT DISTINCT FROM true;
COMMENT ON VIEW public.active_entries IS 'active entry compatibility';
CREATE VIEW public.discarded_view AS SELECT id FROM public.entries;
DROP VIEW IF EXISTS public.discarded_view;

CREATE TABLE public.discarded_table (id INTEGER);
DROP TABLE public.discarded_table;

SELECT
    id::text,
    balance::text::numeric::bigint,
    balance * 2::bigint > 0::numeric,
    extract(epoch FROM updated_at)::bigint,
    CURRENT_TIMESTAMP + INTERVAL '7 days'
FROM public.accounts
ORDER BY id
LIMIT 1;

DROP SEQUENCE public.discarded_sequence;
ALTER TABLE public.entries RENAME TO account_entries;

INSERT INTO public.account_entries (id, account_id, imported_position, label, active)
VALUES (
    '10000000-0000-0000-0000-000000000003',
    '00000000-0000-0000-0000-000000000001',
    300,
    'third',
    true
);
