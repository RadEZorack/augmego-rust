CREATE TABLE IF NOT EXISTS world_chunk_overrides (
  world_seed BIGINT NOT NULL,
  chunk_x INTEGER NOT NULL,
  chunk_z INTEGER NOT NULL,
  revision BIGINT NOT NULL,
  format_version SMALLINT NOT NULL DEFAULT 1,
  override_count INTEGER NOT NULL,
  payload BYTEA NOT NULL,
  created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
  updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
  PRIMARY KEY (world_seed, chunk_x, chunk_z)
);
