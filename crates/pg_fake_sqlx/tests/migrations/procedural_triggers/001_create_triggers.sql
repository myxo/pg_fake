CREATE TABLE public.records (
    id BIGINT PRIMARY KEY,
    value BIGINT NOT NULL,
    compatible BOOLEAN,
    inserted_by_trigger BOOLEAN NOT NULL DEFAULT false,
    created_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP
);

CREATE OR REPLACE FUNCTION public.touch_record() RETURNS TRIGGER AS $$
BEGIN
    NEW.updated_at := CURRENT_TIMESTAMP;
    RETURN NEW;
END;
$$ LANGUAGE plpgsql;

CREATE FUNCTION public.mark_inserted_record() RETURNS TRIGGER AS $$
BEGIN
    NEW.inserted_by_trigger := true;
    RETURN NEW;
END;
$$ LANGUAGE plpgsql;

CREATE FUNCTION public.normalize_record() RETURNS TRIGGER AS $$
BEGIN
    IF NEW.compatible IS NULL THEN
        NEW.compatible := true;
    ELSIF NEW.value IS NULL THEN
        RETURN NULL;
    ELSE
        NEW.value = NEW.value + 1;
    END IF;
    RETURN NEW;
END;
$$ LANGUAGE plpgsql;

CREATE TRIGGER touch_before_update
    BEFORE UPDATE ON public.records
    FOR EACH ROW EXECUTE FUNCTION public.touch_record();
CREATE TRIGGER mark_before_insert
    BEFORE INSERT ON public.records
    FOR EACH ROW EXECUTE FUNCTION public.mark_inserted_record();
CREATE TRIGGER normalize_before_write
    BEFORE INSERT OR UPDATE ON public.records
    FOR EACH ROW EXECUTE FUNCTION public.normalize_record();
