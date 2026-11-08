CREATE TABLE IF NOT EXISTS les_model_connection_revisions (
                revision_id TEXT PRIMARY KEY,
                connection_id TEXT NOT NULL,
                revision_no INTEGER NOT NULL,
                display_name TEXT NOT NULL,
                protocol TEXT NOT NULL,
                base_url TEXT NOT NULL,
                model_id TEXT NOT NULL,
                locality TEXT NOT NULL,
                requested_context_tokens INTEGER,
                secret_ref TEXT,
                extension_type TEXT,
                enabled INTEGER NOT NULL,
                created_at TEXT NOT NULL,
                created_by TEXT NOT NULL,
                UNIQUE(connection_id, revision_no)
            );
            CREATE TABLE IF NOT EXISTS les_model_connection_heads (
                connection_id TEXT PRIMARY KEY,
                revision_id TEXT NOT NULL REFERENCES les_model_connection_revisions(revision_id)
            );
            CREATE TABLE IF NOT EXISTS les_model_capability_snapshots (
                snapshot_id TEXT PRIMARY KEY,
                connection_revision_id TEXT NOT NULL REFERENCES les_model_connection_revisions(revision_id),
                observations_json TEXT NOT NULL,
                observed_at TEXT NOT NULL,
                expires_at TEXT NOT NULL,
                transport_options_json TEXT NOT NULL,
                created_by TEXT NOT NULL
            );
            CREATE TABLE IF NOT EXISTS les_model_role_bindings (
                role TEXT NOT NULL,
                binding_revision INTEGER NOT NULL,
                connection_revision_id TEXT NOT NULL REFERENCES les_model_connection_revisions(revision_id),
                bound_at TEXT NOT NULL,
                bound_by TEXT NOT NULL,
                PRIMARY KEY(role, binding_revision)
            );
            CREATE TABLE IF NOT EXISTS les_model_connection_audit (
                event_id TEXT PRIMARY KEY,
                action TEXT NOT NULL,
                connection_id TEXT,
                revision_id TEXT,
                actor TEXT NOT NULL,
                created_at TEXT NOT NULL,
                details_json TEXT NOT NULL
            );
