-- A poison step must stop after three failed executions, across cron slots and restarts.
ALTER TABLE flow_runs
    ADD COLUMN attempts smallint NOT NULL DEFAULT 0 CHECK (attempts BETWEEN 0 AND 3),
    ADD COLUMN last_error text CHECK (last_error ~ '^[a-z_]{1,40}$');
