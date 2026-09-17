CREATE TABLE public.app_sessions (
    id UUID PRIMARY KEY,
    identity_id BIGINT NOT NULL REFERENCES public.identities(id),
    permissions BIGINT[] NOT NULL,
    related_ids UUID[] NOT NULL,
    context JSONB NOT NULL,
    expires_at TIMESTAMPTZ NOT NULL,
    active BOOLEAN NOT NULL DEFAULT true
);

CREATE UNIQUE INDEX app_sessions_one_active_identity
    ON public.app_sessions (identity_id)
    WHERE active;

CREATE TABLE public.app_work_items (
    id BIGINT PRIMARY KEY,
    priority INTEGER NOT NULL,
    state TEXT NOT NULL DEFAULT 'ready',
    claim_key TEXT NOT NULL,
    owner_id UUID,
    tags UUID[] NOT NULL,
    payload JSONB NOT NULL,
    created_at TIMESTAMPTZ NOT NULL,
    updated_at TIMESTAMPTZ NOT NULL
);

CREATE INDEX app_work_items_ready
    ON public.app_work_items (priority DESC, id ASC)
    INCLUDE (claim_key)
    WHERE state = 'ready';

CREATE VIEW public.app_ready_work AS
SELECT id, priority, claim_key
FROM public.app_work_items
WHERE state = 'ready';

CREATE TABLE public.app_work_accounting (
    work_id BIGINT PRIMARY KEY REFERENCES public.app_work_items(id),
    owner_id UUID NOT NULL,
    charged BIGINT NOT NULL,
    recorded_at TIMESTAMPTZ NOT NULL
);

CREATE TABLE public.app_promotions (
    code TEXT PRIMARY KEY,
    remaining BIGINT NOT NULL,
    metadata JSONB NOT NULL
);

CREATE TABLE public.app_payments (
    id UUID PRIMARY KEY,
    identity_id BIGINT NOT NULL REFERENCES public.identities(id),
    promotion_code TEXT REFERENCES public.app_promotions(code),
    amount NUMERIC NOT NULL,
    state TEXT NOT NULL,
    metadata JSONB NOT NULL,
    created_at TIMESTAMPTZ NOT NULL
);

CREATE TABLE public.app_request_events (
    request_key TEXT NOT NULL,
    issued_at BIGINT NOT NULL,
    accepted BOOLEAN NOT NULL,
    PRIMARY KEY (request_key, issued_at)
);

CREATE TABLE public.app_threads (
    id UUID PRIMARY KEY,
    identity_id BIGINT NOT NULL REFERENCES public.identities(id),
    subject TEXT NOT NULL,
    metadata JSONB NOT NULL
);

CREATE TABLE public.app_executions (
    id UUID PRIMARY KEY,
    thread_id UUID NOT NULL REFERENCES public.app_threads(id),
    state TEXT NOT NULL,
    started_at TIMESTAMPTZ NOT NULL,
    finished_at TIMESTAMPTZ,
    payload JSONB NOT NULL
);

CREATE SEQUENCE public.app_log_id_seq START 1;

CREATE TABLE public.app_logs (
    id BIGINT PRIMARY KEY DEFAULT nextval('public.app_log_id_seq'),
    execution_id UUID NOT NULL REFERENCES public.app_executions(id),
    ordinal BIGINT NOT NULL,
    payload JSONB NOT NULL,
    created_at TIMESTAMPTZ NOT NULL,
    UNIQUE (execution_id, ordinal)
);

CREATE TABLE public.app_maintenance (
    id SERIAL PRIMARY KEY,
    value TEXT NOT NULL
);
