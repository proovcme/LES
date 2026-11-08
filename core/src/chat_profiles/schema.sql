CREATE TABLE IF NOT EXISTS les_prompt_revisions (
            revision_id TEXT PRIMARY KEY,
            name TEXT NOT NULL,
            revision_no INTEGER NOT NULL,
            text_value TEXT NOT NULL,
            sha256 TEXT NOT NULL,
            is_factory INTEGER NOT NULL DEFAULT 0,
            source_revision_id TEXT,
            created_at TEXT NOT NULL,
            deleted_at TEXT
        );
        CREATE TABLE IF NOT EXISTS les_skill_revisions (
            revision_id TEXT PRIMARY KEY,
            name TEXT NOT NULL,
            revision_no INTEGER NOT NULL,
            text_value TEXT NOT NULL,
            sha256 TEXT NOT NULL,
            is_factory INTEGER NOT NULL DEFAULT 0,
            source_revision_id TEXT,
            created_at TEXT NOT NULL,
            deleted_at TEXT
        );
        CREATE TABLE IF NOT EXISTS les_profile_revisions (
            revision_id TEXT PRIMARY KEY,
            mode TEXT NOT NULL,
            name TEXT NOT NULL,
            revision_no INTEGER NOT NULL,
            prompt_revision_id TEXT NOT NULL,
            skill_revision_id TEXT NOT NULL,
            tools_json TEXT NOT NULL,
            model_policy_json TEXT NOT NULL,
            rag_policy_json TEXT NOT NULL,
            snapshot_json TEXT NOT NULL,
            is_factory INTEGER NOT NULL DEFAULT 0,
            source_revision_id TEXT,
            created_at TEXT NOT NULL,
            deleted_at TEXT
        );
        CREATE TABLE IF NOT EXISTS les_active_profiles (
            mode TEXT PRIMARY KEY,
            profile_revision_id TEXT NOT NULL,
            updated_at TEXT NOT NULL
        );
        CREATE TABLE IF NOT EXISTS les_chat_profile_bindings (
            session_id TEXT PRIMARY KEY,
            mode TEXT NOT NULL,
            profile_revision_id TEXT NOT NULL,
            snapshot_json TEXT NOT NULL,
            bound_at TEXT NOT NULL,
            updated_at TEXT NOT NULL
        );
