CREATE TABLE IF NOT EXISTS tool_approval_receipts (
 receipt_id TEXT PRIMARY KEY,status TEXT NOT NULL,proposal_revision TEXT NOT NULL,
 tool_name TEXT NOT NULL,argument_sha256 TEXT NOT NULL,actor_id TEXT NOT NULL,expires_at_epoch REAL NOT NULL);
CREATE TABLE IF NOT EXISTS tool_idempotency_ledger (
 actor_id TEXT NOT NULL,tool_name TEXT NOT NULL,idempotency_key TEXT NOT NULL,
 fingerprint TEXT NOT NULL,state TEXT NOT NULL,envelope_json TEXT NOT NULL DEFAULT '',updated_at_epoch REAL NOT NULL,
 PRIMARY KEY(actor_id,tool_name,idempotency_key));
CREATE TABLE IF NOT EXISTS tool_result_cursors (
 cursor TEXT PRIMARY KEY,actor_id TEXT NOT NULL,expires_at_epoch REAL NOT NULL,result_json TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS tool_approval_uses (
 receipt_id TEXT PRIMARY KEY,actor_id TEXT NOT NULL,tool_name TEXT NOT NULL,idempotency_key TEXT NOT NULL,
 fingerprint TEXT NOT NULL,bound_at_epoch REAL NOT NULL);
