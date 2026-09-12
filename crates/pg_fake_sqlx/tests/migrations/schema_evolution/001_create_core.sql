CREATE TABLE public.accounts (
    id UUID PRIMARY KEY,
    code CHAR(3) NOT NULL,
    display_name TEXT NOT NULL,
    active BOOLEAN NOT NULL DEFAULT true,
    priority SMALLINT NOT NULL DEFAULT 0,
    revision INTEGER NOT NULL DEFAULT 0,
    balance NUMERIC NOT NULL DEFAULT 0,
    payload BYTEA,
    opened_on DATE,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    retention INTERVAL,
    metadata JSONB NOT NULL DEFAULT '{}'
);

CREATE TABLE public.entries (
    id UUID PRIMARY KEY,
    account_id UUID,
    legacy_position INTEGER,
    label TEXT,
    active BOOLEAN NOT NULL DEFAULT true
);

INSERT INTO public.accounts (
    id, code, display_name, priority, revision, balance, payload,
    opened_on, updated_at, retention, metadata
) VALUES (
    '00000000-0000-0000-0000-000000000001', 'USD', ' Primary ', 1, 2,
    12.50, '\x0102', '2024-01-02', '2024-01-02 03:04:05+00',
    INTERVAL '7 days', '{"amount":{"currency":"USD","value":"12.50"}}'
);

INSERT INTO public.entries VALUES
    ('10000000-0000-0000-0000-000000000002', '00000000-0000-0000-0000-000000000001', 20, 'second', true),
    ('10000000-0000-0000-0000-000000000001', '00000000-0000-0000-0000-000000000001', 10, 'first', true),
    ('10000000-0000-0000-0000-000000000009', '00000000-0000-0000-0000-000000000001', 90, 'inactive', false);
