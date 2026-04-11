use anyhow::{Context, Result, anyhow};
use shared_protocol::{
    INVENTORY_MAX_STACK_SIZE, INVENTORY_SLOT_COUNT, InventorySnapshot, InventoryStack,
};
use shared_world::BlockId;
use sqlx::{PgPool, Row};
use uuid::Uuid;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlayerBlockInventory {
    pub slots: Vec<Option<InventoryStack>>,
}

impl PlayerBlockInventory {
    pub fn empty() -> Self {
        Self {
            slots: vec![None; INVENTORY_SLOT_COUNT],
        }
    }

    pub fn starter() -> Self {
        let mut inventory = Self::empty();
        inventory.slots[0] = Some(InventoryStack {
            block: BlockId::Grass,
            count: 32,
        });
        inventory.slots[1] = Some(InventoryStack {
            block: BlockId::Stone,
            count: 32,
        });
        inventory.slots[2] = Some(InventoryStack {
            block: BlockId::Sand,
            count: 32,
        });
        inventory.slots[3] = Some(InventoryStack {
            block: BlockId::Log,
            count: 16,
        });
        inventory
    }

    pub fn snapshot(&self) -> InventorySnapshot {
        InventorySnapshot {
            slots: self.slots.clone(),
        }
    }

    pub fn can_add_block(&self, block: BlockId) -> bool {
        if !block.is_collectible() {
            return false;
        }

        self.slots.iter().any(|slot| match slot {
            Some(stack) => stack.block == block && stack.count < INVENTORY_MAX_STACK_SIZE,
            None => true,
        })
    }

    pub fn add_block(&mut self, block: BlockId) -> bool {
        if !self.can_add_block(block) {
            return false;
        }

        if let Some(Some(stack)) = self
            .slots
            .iter_mut()
            .find(|slot| matches!(slot, Some(stack) if stack.block == block && stack.count < INVENTORY_MAX_STACK_SIZE))
        {
            stack.count += 1;
            return true;
        }

        if let Some(empty_slot) = self.slots.iter_mut().find(|slot| slot.is_none()) {
            *empty_slot = Some(InventoryStack { block, count: 1 });
            return true;
        }

        false
    }

    pub fn has_block(&self, block: BlockId) -> bool {
        self.slots
            .iter()
            .flatten()
            .any(|stack| stack.block == block && stack.count > 0)
    }

    pub fn remove_block(&mut self, block: BlockId) -> bool {
        for slot in &mut self.slots {
            let Some(stack) = slot.as_mut() else {
                continue;
            };
            if stack.block != block {
                continue;
            }

            if stack.count > 1 {
                stack.count -= 1;
            } else {
                *slot = None;
            }
            return true;
        }

        false
    }

    pub fn swap_slots(&mut self, from: usize, to: usize) -> bool {
        if from >= INVENTORY_SLOT_COUNT || to >= INVENTORY_SLOT_COUNT {
            return false;
        }

        self.slots.swap(from, to);
        true
    }

    fn from_slots(slots: Vec<Option<InventoryStack>>) -> Self {
        let mut normalized = slots;
        normalized.truncate(INVENTORY_SLOT_COUNT);
        normalized.resize(INVENTORY_SLOT_COUNT, None);
        Self { slots: normalized }
    }
}

#[derive(Clone)]
pub struct BlockInventoryService {
    pool: PgPool,
}

impl BlockInventoryService {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    pub async fn load_or_seed_user_inventory(&self, user_id: &str) -> Result<PlayerBlockInventory> {
        let user_id = parse_user_id(user_id)?;
        if let Some(inventory) = self.load_existing_user_inventory(user_id).await? {
            return Ok(inventory);
        }

        let inventory = PlayerBlockInventory::starter();
        self.save_user_inventory_by_uuid(user_id, &inventory)
            .await?;
        Ok(inventory)
    }

    pub async fn save_user_inventory(
        &self,
        user_id: &str,
        inventory: &PlayerBlockInventory,
    ) -> Result<()> {
        self.save_user_inventory_by_uuid(parse_user_id(user_id)?, inventory)
            .await
    }

    async fn load_existing_user_inventory(
        &self,
        user_id: Uuid,
    ) -> Result<Option<PlayerBlockInventory>> {
        let rows = sqlx::query(
            "SELECT slot_index, block_id, stack_count
             FROM player_block_inventory_slots
             WHERE user_id = $1
             ORDER BY slot_index ASC",
        )
        .bind(user_id)
        .fetch_all(&self.pool)
        .await
        .context("load player block inventory")?;

        if rows.is_empty() {
            return Ok(None);
        }

        let mut slots = vec![None; INVENTORY_SLOT_COUNT];
        for row in rows {
            let slot_index: i16 = row.get("slot_index");
            let block_id: i16 = row.get("block_id");
            let stack_count: i16 = row.get("stack_count");
            let slot_index =
                usize::try_from(slot_index).context("convert inventory slot index to usize")?;
            if slot_index >= INVENTORY_SLOT_COUNT {
                return Err(anyhow!("inventory slot {slot_index} is out of range"));
            }
            let block = BlockId::from_raw(
                u16::try_from(block_id).context("convert inventory block id to u16")?,
            )
            .ok_or_else(|| anyhow!("unknown persisted block id {block_id}"))?;
            let count = u16::try_from(stack_count).context("convert stack count to u16")?;
            if count == 0 || count > INVENTORY_MAX_STACK_SIZE {
                return Err(anyhow!("invalid persisted stack count {count}"));
            }
            slots[slot_index] = Some(InventoryStack { block, count });
        }

        Ok(Some(PlayerBlockInventory::from_slots(slots)))
    }

    async fn save_user_inventory_by_uuid(
        &self,
        user_id: Uuid,
        inventory: &PlayerBlockInventory,
    ) -> Result<()> {
        let mut tx = self
            .pool
            .begin()
            .await
            .context("begin inventory transaction")?;
        sqlx::query("DELETE FROM player_block_inventory_slots WHERE user_id = $1")
            .bind(user_id)
            .execute(&mut *tx)
            .await
            .context("clear player block inventory")?;

        for (slot_index, slot) in inventory.slots.iter().enumerate() {
            let Some(stack) = slot else {
                continue;
            };
            sqlx::query(
                "INSERT INTO player_block_inventory_slots (user_id, slot_index, block_id, stack_count)
                 VALUES ($1, $2, $3, $4)",
            )
            .bind(user_id)
            .bind(i16::try_from(slot_index).context("convert slot index to i16")?)
            .bind(i16::try_from(stack.block.raw()).context("convert block id to i16")?)
            .bind(i16::try_from(stack.count).context("convert stack count to i16")?)
            .execute(&mut *tx)
            .await
            .context("insert player block inventory slot")?;
        }

        tx.commit().await.context("commit inventory transaction")?;
        Ok(())
    }
}

fn parse_user_id(user_id: &str) -> Result<Uuid> {
    Uuid::parse_str(user_id).with_context(|| format!("parse user id {user_id}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db;

    async fn inventory_test_service() -> (BlockInventoryService, String, String) {
        let base_database_url = std::env::var("DATABASE_URL")
            .unwrap_or_else(|_| "postgresql://postgres:postgres@127.0.0.1:5432/augmego".into());
        let (pool, schema_name) = db::connect_isolated_test_pool(&base_database_url)
            .await
            .expect("create isolated schema");
        (
            BlockInventoryService::new(pool),
            base_database_url,
            schema_name,
        )
    }

    async fn insert_test_user(pool: &PgPool, user_id: Uuid) {
        sqlx::query("INSERT INTO users (id, email) VALUES ($1, $2)")
            .bind(user_id)
            .bind(format!("{user_id}@example.test"))
            .execute(pool)
            .await
            .expect("insert test user");
    }

    #[test]
    fn starter_inventory_uses_expected_seed_slots() {
        let inventory = PlayerBlockInventory::starter();
        assert_eq!(
            inventory.slots[0],
            Some(InventoryStack {
                block: BlockId::Grass,
                count: 32,
            })
        );
        assert_eq!(
            inventory.slots[1],
            Some(InventoryStack {
                block: BlockId::Stone,
                count: 32,
            })
        );
        assert_eq!(
            inventory.slots[2],
            Some(InventoryStack {
                block: BlockId::Sand,
                count: 32,
            })
        );
        assert_eq!(
            inventory.slots[3],
            Some(InventoryStack {
                block: BlockId::Log,
                count: 16,
            })
        );
        assert!(inventory.slots[4..].iter().all(Option::is_none));
    }

    #[tokio::test]
    async fn load_or_seed_user_inventory_returns_starter_slots_for_new_user() {
        let (service, base_database_url, schema_name) = inventory_test_service().await;
        let user_id = Uuid::new_v4();
        insert_test_user(&service.pool, user_id).await;

        let inventory = service
            .load_or_seed_user_inventory(&user_id.to_string())
            .await
            .expect("seed user inventory");

        assert_eq!(inventory, PlayerBlockInventory::starter());

        db::cleanup_isolated_test_schema(&base_database_url, &schema_name)
            .await
            .expect("cleanup isolated schema");
    }

    #[tokio::test]
    async fn saved_user_inventory_persists_across_service_reloads() {
        let (service, base_database_url, schema_name) = inventory_test_service().await;
        let user_id = Uuid::new_v4();
        insert_test_user(&service.pool, user_id).await;
        let mut inventory = service
            .load_or_seed_user_inventory(&user_id.to_string())
            .await
            .expect("seed user inventory");
        assert!(inventory.remove_block(BlockId::Grass));
        assert!(inventory.add_block(BlockId::CoalOre));
        assert!(inventory.swap_slots(0, 10));
        service
            .save_user_inventory(&user_id.to_string(), &inventory)
            .await
            .expect("save user inventory");

        let reloaded = service
            .load_or_seed_user_inventory(&user_id.to_string())
            .await
            .expect("reload user inventory");

        assert_eq!(reloaded, inventory);

        db::cleanup_isolated_test_schema(&base_database_url, &schema_name)
            .await
            .expect("cleanup isolated schema");
    }
}
