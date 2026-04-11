CREATE TABLE IF NOT EXISTS player_block_inventory_slots (
  user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  slot_index SMALLINT NOT NULL,
  block_id SMALLINT NOT NULL,
  stack_count SMALLINT NOT NULL,
  created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
  updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
  PRIMARY KEY (user_id, slot_index),
  CONSTRAINT player_block_inventory_slots_slot_range CHECK (slot_index BETWEEN 0 AND 26),
  CONSTRAINT player_block_inventory_slots_stack_range CHECK (stack_count BETWEEN 1 AND 64)
);
