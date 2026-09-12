SET LOCAL lock_timeout = '5s';
SET LOCAL statement_timeout = '30min';
LOCK TABLE public.identities, public.imported_records IN EXCLUSIVE MODE;

CREATE TEMP TABLE pg_temp.record_matches (
    imported_id BIGINT PRIMARY KEY,
    identity_id BIGINT NOT NULL,
    amount BIGINT,
    candidate_count BIGINT NOT NULL,
    ordinal BIGINT NOT NULL
) ON COMMIT DROP;

WITH candidates AS MATERIALIZED (
    SELECT
        imported.id AS imported_id,
        identity.id AS identity_id,
        (imported.payload #>> '{amount,value}')::numeric::bigint AS amount,
        count(*) OVER (PARTITION BY imported.external_code) AS candidate_count,
        row_number() OVER (ORDER BY imported.id) AS ordinal
    FROM public.imported_records AS imported
    INNER JOIN public.identities AS identity
        ON identity.external_code = imported.external_code
    WHERE jsonb_typeof(imported.payload) = 'object'
      AND jsonb_typeof(imported.payload #> '{amount}') = 'object'
      AND imported.payload #>> '{amount,currency}' = 'USD'
      AND (
          imported.payload #>> '{amount,value}' IS NULL
          OR imported.payload #>> '{amount,value}' ~ '^-?[0-9]{1,9}([.][0-9]{1,2})?$'
      )
)
INSERT INTO pg_temp.record_matches
SELECT imported_id, identity_id, amount, candidate_count, ordinal
FROM candidates
WHERE EXISTS (
    SELECT 1 FROM public.identities
    WHERE identities.id = candidates.identity_id
)
ORDER BY ordinal
ON CONFLICT (imported_id) DO NOTHING;

UPDATE public.imported_records AS imported
SET identity_id = matched.identity_id,
    amount = matched.amount,
    match_state = CASE
        WHEN matched.candidate_count = 1 THEN 'matched'
        ELSE 'ambiguous'
    END
FROM (
    SELECT imported_id, identity_id, amount, candidate_count
    FROM pg_temp.record_matches
) AS matched
WHERE imported.id = matched.imported_id;

UPDATE public.imported_records AS imported
SET amount = matched.amount
FROM pg_temp.record_matches AS matched
WHERE imported.id = matched.imported_id;

UPDATE public.imported_records AS imported
SET amount = (
    SELECT candidate.amount
    FROM public.imported_records AS candidate
    WHERE candidate.id = imported.id
    ORDER BY candidate.id
    LIMIT 1
);

UPDATE public.imported_records AS imported
SET match_state = 'unmatched'
WHERE identity_id IS NULL;

WITH log_rows AS (
    SELECT imported.id,
           coalesce(imported.match_state, 'missing') AS message
    FROM public.imported_records AS imported
    LEFT JOIN pg_temp.record_matches AS matched ON matched.imported_id = imported.id
    WHERE EXISTS (
        SELECT 1 FROM public.imported_records AS source
        WHERE source.id = imported.id
    ) OR NOT EXISTS (
        SELECT 1 FROM pg_temp.record_matches AS candidate
        WHERE candidate.imported_id = imported.id
    )
)
INSERT INTO public.reconciliation_log
SELECT id, message FROM log_rows ORDER BY id;

DO $$
DECLARE
    affected BIGINT;
    total BIGINT;
    maximum BIGINT;
    states TEXT;
    unsupported_currency TEXT;
BEGIN
    UPDATE public.reconciliation_log SET message = message;
    GET DIAGNOSTICS affected = ROW_COUNT;
    SELECT count(*), max(id), string_agg(match_state, ',' ORDER BY id)
    INTO total, maximum, states
    FROM public.imported_records;
    SELECT imported.payload #>> '{amount,currency}'
    INTO unsupported_currency
    FROM public.imported_records AS imported
    WHERE imported.payload #>> '{amount,currency}' IS NOT NULL
      AND imported.payload #>> '{amount,currency}' IS DISTINCT FROM 'USD'
    ORDER BY imported.id
    LIMIT 1;
    IF unsupported_currency IS NOT NULL THEN
        RAISE EXCEPTION 'unsupported currency %', unsupported_currency USING HINT = 'normalize legacy currency and retry';
    ELSIF affected IS DISTINCT FROM total OR maximum IS NULL OR states IS NULL THEN
        RAISE EXCEPTION 'unexpected reconciliation % %', maximum, states;
    END IF;
END;
$$;
