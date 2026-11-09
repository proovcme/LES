CREATE TABLE IF NOT EXISTS mail_accounts (
    id TEXT PRIMARY KEY,
    kind TEXT NOT NULL,
    label TEXT NOT NULL,
    dataset_id TEXT NOT NULL DEFAULT '',
    dataset_name TEXT NOT NULL DEFAULT '',
    native_account_id TEXT NOT NULL DEFAULT '',
    config_json TEXT NOT NULL DEFAULT '{}',
    enabled INTEGER NOT NULL DEFAULT 1,
    sync_state TEXT NOT NULL DEFAULT 'idle',
    last_sync TEXT NOT NULL DEFAULT '',
    last_error TEXT NOT NULL DEFAULT '',
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);
CREATE UNIQUE INDEX IF NOT EXISTS uq_mail_account_native
    ON mail_accounts(kind, native_account_id)
    WHERE native_account_id <> '';

CREATE TABLE IF NOT EXISTS mail_folders (
    account_id TEXT NOT NULL,
    native_id TEXT NOT NULL,
    path TEXT NOT NULL,
    special_use TEXT NOT NULL DEFAULT '',
    enabled INTEGER NOT NULL DEFAULT 1,
    uid_validity TEXT NOT NULL DEFAULT '',
    last_uid INTEGER NOT NULL DEFAULT 0,
    backfill_complete INTEGER NOT NULL DEFAULT 0,
    updated_at TEXT NOT NULL,
    PRIMARY KEY(account_id, native_id),
    FOREIGN KEY(account_id) REFERENCES mail_accounts(id) ON DELETE CASCADE
);

CREATE TABLE IF NOT EXISTS mail_messages (
    id TEXT PRIMARY KEY,
    account_id TEXT NOT NULL,
    native_id TEXT NOT NULL DEFAULT '',
    internet_message_id TEXT NOT NULL DEFAULT '',
    content_sha256 TEXT NOT NULL,
    subject TEXT NOT NULL DEFAULT '',
    sender TEXT NOT NULL DEFAULT '',
    recipients_json TEXT NOT NULL DEFAULT '[]',
    received_at TEXT NOT NULL DEFAULT '',
    sent_at TEXT NOT NULL DEFAULT '',
    thread_key TEXT NOT NULL DEFAULT '',
    raw_path TEXT NOT NULL,
    relative_path TEXT NOT NULL,
    source_kind TEXT NOT NULL,
    outlook_store_id TEXT NOT NULL DEFAULT '',
    outlook_entry_id TEXT NOT NULL DEFAULT '',
    index_status TEXT NOT NULL DEFAULT 'pending',
    rag_doc_id TEXT NOT NULL DEFAULT '',
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    FOREIGN KEY(account_id) REFERENCES mail_accounts(id) ON DELETE CASCADE
);
CREATE INDEX IF NOT EXISTS ix_mail_messages_account_date
    ON mail_messages(account_id, received_at DESC);
CREATE INDEX IF NOT EXISTS ix_mail_messages_thread
    ON mail_messages(thread_key);
CREATE INDEX IF NOT EXISTS ix_mail_messages_hash
    ON mail_messages(content_sha256);

CREATE TABLE IF NOT EXISTS mail_message_locations (
    message_id TEXT NOT NULL,
    folder_native_id TEXT NOT NULL,
    folder_path TEXT NOT NULL,
    first_seen TEXT NOT NULL,
    last_seen TEXT NOT NULL,
    is_current INTEGER NOT NULL DEFAULT 1,
    PRIMARY KEY(message_id, folder_native_id),
    FOREIGN KEY(message_id) REFERENCES mail_messages(id) ON DELETE CASCADE
);

CREATE TABLE IF NOT EXISTS mail_attachment_provenance (
    account_id TEXT NOT NULL,
    attachment_sha256 TEXT NOT NULL,
    message_id TEXT NOT NULL,
    attachment_id TEXT NOT NULL,
    first_seen TEXT NOT NULL,
    PRIMARY KEY(account_id,attachment_sha256,message_id,attachment_id),
    FOREIGN KEY(account_id) REFERENCES mail_accounts(id) ON DELETE CASCADE,
    FOREIGN KEY(message_id) REFERENCES mail_messages(id) ON DELETE CASCADE
);
