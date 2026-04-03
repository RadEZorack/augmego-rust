CREATE INDEX IF NOT EXISTS pets_ready_reserve_idx
  ON pets (updated_at, created_at)
  WHERE status = 'READY' AND model_storage_key IS NOT NULL;
