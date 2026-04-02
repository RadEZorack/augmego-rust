CREATE TABLE IF NOT EXISTS users (
  id UUID PRIMARY KEY,
  email TEXT UNIQUE,
  name TEXT,
  avatar_url TEXT,
  created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
  updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE TABLE IF NOT EXISTS auth_identities (
  id UUID PRIMARY KEY,
  user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  provider TEXT NOT NULL,
  subject TEXT NOT NULL,
  email TEXT,
  created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
  UNIQUE(provider, subject)
);

CREATE TABLE IF NOT EXISTS sessions (
  id UUID PRIMARY KEY,
  user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
  expires_at TIMESTAMPTZ NOT NULL,
  revoked_at TIMESTAMPTZ
);

CREATE INDEX IF NOT EXISTS sessions_lookup_idx
  ON sessions (id, expires_at)
  WHERE revoked_at IS NULL;

CREATE TABLE IF NOT EXISTS avatar_slots (
  user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  slot TEXT NOT NULL,
  model_url TEXT,
  storage_key TEXT,
  created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
  updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
  PRIMARY KEY (user_id, slot),
  CONSTRAINT avatar_slots_slot_check CHECK (slot IN ('IDLE', 'RUN', 'DANCE'))
);

CREATE TABLE IF NOT EXISTS pets (
  id UUID PRIMARY KEY,
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
  captured_at TIMESTAMPTZ,
  captured_by_user_id UUID REFERENCES users(id) ON DELETE SET NULL,
  created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
  updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
  CONSTRAINT pets_status_check CHECK (status IN ('QUEUED', 'GENERATING', 'READY', 'SPAWNED', 'CAPTURED', 'FAILED'))
);

CREATE INDEX IF NOT EXISTS pets_status_idx ON pets (status, updated_at);
CREATE INDEX IF NOT EXISTS pets_captured_idx ON pets (captured_by_user_id, captured_at);
