ALTER TABLE public.imported_records
    ADD CONSTRAINT imported_identity_fk
    FOREIGN KEY (identity_id) REFERENCES public.identities(id) NOT VALID;
ALTER TABLE public.imported_records VALIDATE CONSTRAINT imported_identity_fk;
LOCK TABLE public.imported_records IN ACCESS EXCLUSIVE MODE;
