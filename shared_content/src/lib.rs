use serde::{Deserialize, Serialize};
use shared_world::BlockId;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BlockDefinition {
    pub id: BlockId,
    pub name: &'static str,
    pub solid: bool,
    pub transparent: bool,
    pub hardness: f32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Recipe {
    pub output: BlockId,
    pub output_count: u16,
    pub ingredients: Vec<(BlockId, u16)>,
}

pub fn block_definitions() -> Vec<BlockDefinition> {
    vec![
        BlockDefinition {
            id: BlockId::Air,
            name: "Air",
            solid: false,
            transparent: true,
            hardness: 0.0,
        },
        BlockDefinition {
            id: BlockId::Grass,
            name: "Grass",
            solid: true,
            transparent: false,
            hardness: 0.8,
        },
        BlockDefinition {
            id: BlockId::Dirt,
            name: "Dirt",
            solid: true,
            transparent: false,
            hardness: 0.7,
        },
        BlockDefinition {
            id: BlockId::Stone,
            name: "Stone",
            solid: true,
            transparent: false,
            hardness: 1.6,
        },
        BlockDefinition {
            id: BlockId::Sand,
            name: "Sand",
            solid: true,
            transparent: false,
            hardness: 0.6,
        },
        BlockDefinition {
            id: BlockId::Water,
            name: "Water",
            solid: false,
            transparent: true,
            hardness: 0.0,
        },
        BlockDefinition {
            id: BlockId::Log,
            name: "Log",
            solid: true,
            transparent: false,
            hardness: 1.2,
        },
        BlockDefinition {
            id: BlockId::Leaves,
            name: "Leaves",
            solid: true,
            transparent: true,
            hardness: 0.2,
        },
        BlockDefinition {
            id: BlockId::Planks,
            name: "Planks",
            solid: true,
            transparent: false,
            hardness: 1.1,
        },
        BlockDefinition {
            id: BlockId::Glass,
            name: "Glass",
            solid: true,
            transparent: true,
            hardness: 0.3,
        },
        BlockDefinition {
            id: BlockId::Lantern,
            name: "Lantern",
            solid: true,
            transparent: true,
            hardness: 0.3,
        },
        BlockDefinition {
            id: BlockId::Storage,
            name: "Storage Crate",
            solid: true,
            transparent: false,
            hardness: 1.5,
        },
        BlockDefinition {
            id: BlockId::GoldOre,
            name: "Gold Ore",
            solid: true,
            transparent: false,
            hardness: 3.0,
        },
        BlockDefinition {
            id: BlockId::CoalOre,
            name: "Coal Ore",
            solid: true,
            transparent: false,
            hardness: 2.4,
        },
        BlockDefinition {
            id: BlockId::IronOre,
            name: "Iron Ore",
            solid: true,
            transparent: false,
            hardness: 3.0,
        },
        BlockDefinition {
            id: BlockId::Sandstone,
            name: "Sandstone",
            solid: true,
            transparent: false,
            hardness: 0.9,
        },
    ]
}

pub fn starter_recipes() -> Vec<Recipe> {
    vec![
        Recipe {
            output: BlockId::Planks,
            output_count: 4,
            ingredients: vec![(BlockId::Log, 1)],
        },
        Recipe {
            output: BlockId::Storage,
            output_count: 1,
            ingredients: vec![(BlockId::Planks, 8)],
        },
        Recipe {
            output: BlockId::Glass,
            output_count: 2,
            ingredients: vec![(BlockId::Sand, 2)],
        },
    ]
}
