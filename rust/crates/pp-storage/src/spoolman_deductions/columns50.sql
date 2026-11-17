ALTER TABLE spoolman_deductions ADD COLUMN failure TEXT CHECK (failure IS NULL OR (failure IN ('unreachable', 'refused', 'reply_lost', 'interrupted', 'integration_missing', 'integration_disabled') AND state IN ('failed', 'unknown', 'dismissed')));
-- A CHECK passes when its expression is NULL, so a comparison with failure cannot be the only guard.
ALTER TABLE spoolman_deductions ADD COLUMN failure_status INTEGER CHECK (CASE WHEN failure_status IS NULL THEN 1 WHEN failure = 'refused' AND failure_status >= 100 AND failure_status <= 599 THEN 1 ELSE 0 END);
ALTER TABLE spoolman_deductions ADD COLUMN spool_grams_before REAL;
