DO $$
DECLARE
    invalid_id BIGINT;
    invalid_label TEXT := 'incompatible';
BEGIN
    SELECT max(id) INTO invalid_id
    FROM public.records
    WHERE compatible IS NOT DISTINCT FROM false;
    IF invalid_id IS NOT NULL THEN
        RAISE EXCEPTION 'record % is %', invalid_id, invalid_label USING HINT = 'set compatible before retrying';
    END IF;
END;
$$;

ALTER TRIGGER normalize_before_write ON public.records RENAME TO normalize_record_before_write;
DROP TRIGGER IF EXISTS normalize_record_before_write ON public.records;
DROP FUNCTION IF EXISTS public.normalize_record();
