CREATE TABLE public.identities (
    id BIGINT PRIMARY KEY,
    external_code TEXT NOT NULL UNIQUE
);

CREATE TABLE public.imported_records (
    id BIGINT PRIMARY KEY,
    external_code TEXT NOT NULL,
    payload JSONB NOT NULL,
    identity_id BIGINT,
    amount BIGINT,
    match_state TEXT NOT NULL DEFAULT 'pending'
);

CREATE TABLE public.reconciliation_log (
    imported_id BIGINT PRIMARY KEY,
    message TEXT NOT NULL
);

INSERT INTO public.identities VALUES (1, 'alpha'), (2, 'beta');
INSERT INTO public.imported_records VALUES
    (10, 'alpha', '{"amount":{"currency":"USD","value":"12.5"}}', NULL, NULL, 'pending'),
    (11, 'beta', '{"amount":{"currency":"USD","value":9}}', NULL, NULL, 'pending'),
    (12, 'missing', '{"amount":{"currency":"USD"}}', NULL, NULL, 'pending'),
    (13, 'alpha', '{"amount":null}', NULL, NULL, 'pending'),
    (14, 'alpha', '{"amount":{"currency":"USD","value":"3"}}', NULL, NULL, 'pending');
