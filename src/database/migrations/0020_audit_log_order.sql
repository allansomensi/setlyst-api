-- The audit log is listed newest first with the id (UUIDv7) breaking ties
-- between entries logged in the same instant, so pages are stable.
DROP INDEX IF EXISTS idx_audit_logs_created_at;
CREATE INDEX idx_audit_logs_created_at_id ON audit_logs (created_at DESC, id DESC);
