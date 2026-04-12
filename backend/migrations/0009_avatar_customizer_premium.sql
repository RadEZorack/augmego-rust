CREATE TABLE IF NOT EXISTS avatar_generation_preferences (
  user_id UUID PRIMARY KEY REFERENCES users(id) ON DELETE CASCADE,
  style_options JSONB NOT NULL,
  updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE TABLE IF NOT EXISTS account_entitlements (
  user_id UUID PRIMARY KEY REFERENCES users(id) ON DELETE CASCADE,
  avatar_customizer_premium BOOLEAN NOT NULL DEFAULT FALSE,
  updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

ALTER TABLE player_avatar_generation_tasks
  ADD COLUMN IF NOT EXISTS style_options JSONB;

ALTER TABLE player_avatar_generation_tasks
  ADD COLUMN IF NOT EXISTS effective_prompt TEXT;
