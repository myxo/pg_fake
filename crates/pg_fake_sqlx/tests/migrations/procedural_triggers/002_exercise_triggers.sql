INSERT INTO public.records (id, value, compatible, created_at, updated_at) VALUES
    (1, 10, NULL, '2000-01-01 00:00:00+00', '2000-01-01 00:00:00+00'),
    (2, 20, true, '2000-01-01 00:00:00+00', '2000-01-01 00:00:00+00');

UPDATE public.records SET value = value + 5 WHERE id = 1;

DO $$
DECLARE
    affected BIGINT;
    total BIGINT;
    labels TEXT;
BEGIN
    UPDATE public.records SET value = value WHERE id = 2;
    GET DIAGNOSTICS affected = ROW_COUNT;
    SELECT count(*), string_agg(id::text, ',' ORDER BY id)
    INTO total, labels
    FROM public.records;
    IF affected IS DISTINCT FROM 1 OR total IS DISTINCT FROM 2 OR labels IS DISTINCT FROM '1,2' THEN
        RAISE EXCEPTION 'unexpected trigger state % %', total, labels USING HINT = 'trigger migration rolled back';
    END IF;
END;
$$;
