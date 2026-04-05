ALTER TABLE pets
  ADD COLUMN IF NOT EXISTS equipped_weapon_id UUID REFERENCES weapons(id) ON DELETE SET NULL;

CREATE UNIQUE INDEX IF NOT EXISTS pets_equipped_weapon_idx
  ON pets (equipped_weapon_id)
  WHERE equipped_weapon_id IS NOT NULL;
