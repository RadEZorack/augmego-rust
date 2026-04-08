CREATE TABLE IF NOT EXISTS player_avatar_generation_tasks (
  id UUID PRIMARY KEY,
  user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  status TEXT NOT NULL,
  phase TEXT NOT NULL,
  progress_percent INTEGER NOT NULL DEFAULT 0,
  provider_progress INTEGER,
  status_message TEXT,
  failure_reason TEXT,
  openai_response_id TEXT,
  meshy_model_task_id TEXT,
  meshy_rigging_task_id TEXT,
  meshy_idle_animation_task_id TEXT,
  meshy_dance_animation_task_id TEXT,
  selfie_storage_key TEXT UNIQUE,
  selfie_content_type TEXT,
  portrait_storage_key TEXT UNIQUE,
  portrait_content_type TEXT,
  raw_model_storage_key TEXT UNIQUE,
  rigged_model_storage_key TEXT UNIQUE,
  idle_model_storage_key TEXT UNIQUE,
  run_model_storage_key TEXT UNIQUE,
  dance_model_storage_key TEXT UNIQUE,
  attempts INTEGER NOT NULL DEFAULT 0,
  started_at TIMESTAMPTZ,
  completed_at TIMESTAMPTZ,
  created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
  updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
  CONSTRAINT player_avatar_generation_tasks_status_check CHECK (
    status IN ('QUEUED', 'PROCESSING', 'READY', 'FAILED')
  )
);

CREATE INDEX IF NOT EXISTS player_avatar_generation_tasks_user_created_idx
  ON player_avatar_generation_tasks (user_id, created_at DESC);

CREATE INDEX IF NOT EXISTS player_avatar_generation_tasks_status_updated_idx
  ON player_avatar_generation_tasks (status, updated_at);
