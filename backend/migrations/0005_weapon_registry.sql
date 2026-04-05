CREATE TABLE IF NOT EXISTS weapons (
  id UUID PRIMARY KEY,
  kind TEXT NOT NULL,
  display_name TEXT NOT NULL,
  base_prompt TEXT NOT NULL,
  effective_prompt TEXT NOT NULL,
  variation_key TEXT NOT NULL UNIQUE,
  status TEXT NOT NULL,
  meshy_task_id TEXT UNIQUE,
  meshy_status TEXT,
  model_storage_key TEXT UNIQUE,
  model_url TEXT,
  model_sha256 TEXT UNIQUE,
  attempts INTEGER NOT NULL DEFAULT 0,
  failure_reason TEXT,
  spawned_at TIMESTAMPTZ,
  collected_at TIMESTAMPTZ,
  collected_by_user_id UUID REFERENCES users(id) ON DELETE SET NULL,
  created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
  updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
  CONSTRAINT weapons_status_check CHECK (status IN ('QUEUED', 'GENERATING', 'READY', 'SPAWNED', 'COLLECTED', 'FAILED'))
);

CREATE INDEX IF NOT EXISTS weapons_status_idx ON weapons (status, updated_at);
CREATE INDEX IF NOT EXISTS weapons_collected_idx ON weapons (collected_by_user_id, collected_at);
CREATE INDEX IF NOT EXISTS weapons_ready_reserve_idx
  ON weapons (updated_at, created_at)
  WHERE status = 'READY' AND model_storage_key IS NOT NULL;
