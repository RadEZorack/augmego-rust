ALTER TABLE pets
  ADD COLUMN IF NOT EXISTS party_active BOOLEAN NOT NULL DEFAULT FALSE;

WITH ranked_captured_pets AS (
  SELECT
    id,
    ROW_NUMBER() OVER (
      PARTITION BY captured_by_user_id
      ORDER BY captured_at DESC NULLS LAST, created_at DESC
    ) AS capture_rank
  FROM pets
  WHERE status = 'CAPTURED'
    AND captured_by_user_id IS NOT NULL
)
UPDATE pets
SET party_active = ranked_captured_pets.capture_rank <= 6
FROM ranked_captured_pets
WHERE pets.id = ranked_captured_pets.id;
