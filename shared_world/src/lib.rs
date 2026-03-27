use serde::{Deserialize, Serialize};
use shared_math::{
    CHUNK_DEPTH, CHUNK_HEIGHT, CHUNK_WIDTH, ChunkPos, LocalVoxelPos, SECTION_COUNT,
    SECTION_HEIGHT, section_index, voxel_index,
};
use thiserror::Error;

pub const SECTION_VOLUME: usize = (CHUNK_WIDTH as usize) * (SECTION_HEIGHT as usize) * (CHUNK_DEPTH as usize);
pub const CHUNK_VOLUME: usize = (CHUNK_WIDTH as usize) * (CHUNK_HEIGHT as usize) * (CHUNK_DEPTH as usize);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[repr(u16)]
pub enum BlockId {
    Air = 0,
    Grass = 1,
    Dirt = 2,
    Stone = 3,
    Sand = 4,
    Water = 5,
    Log = 6,
    Leaves = 7,
    Planks = 8,
    Glass = 9,
    Lantern = 10,
    Storage = 11,
    GoldOre = 12,
}

impl BlockId {
    pub fn is_transparent(self) -> bool {
        matches!(self, Self::Air | Self::Water | Self::Leaves | Self::Glass)
    }

    pub fn is_empty(self) -> bool {
        matches!(self, Self::Air)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum BiomeId {
    Plains,
    Forest,
    Desert,
    Alpine,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Voxel {
    pub block: BlockId,
}

impl Default for Voxel {
    fn default() -> Self {
        Self { block: BlockId::Air }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PaletteSection {
    pub palette: Vec<BlockId>,
    pub indices: Vec<u16>,
}

impl PaletteSection {
    pub fn from_voxels(voxels: &[Voxel]) -> Self {
        let mut palette = Vec::<BlockId>::new();
        let mut indices = Vec::<u16>::with_capacity(voxels.len());

        for voxel in voxels {
            let palette_index = palette
                .iter()
                .position(|block| *block == voxel.block)
                .unwrap_or_else(|| {
                    palette.push(voxel.block);
                    palette.len() - 1
                });

            indices.push(palette_index as u16);
        }

        Self { palette, indices }
    }

    pub fn expand(&self) -> Vec<Voxel> {
        self.indices
            .iter()
            .map(|index| Voxel {
                block: self.palette[usize::from(*index)],
            })
            .collect()
    }

    pub fn voxel(&self, index: usize) -> Voxel {
        let palette_index = self.indices[index];
        Voxel {
            block: self.palette[usize::from(palette_index)],
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChunkData {
    pub position: ChunkPos,
    pub biome: BiomeId,
    pub sections: Vec<PaletteSection>,
    pub revision: u64,
}

impl ChunkData {
    pub fn new(position: ChunkPos, biome: BiomeId) -> Self {
        Self {
            position,
            biome,
            sections: vec![
                PaletteSection {
                    palette: vec![BlockId::Air],
                    indices: vec![0; SECTION_VOLUME],
                };
                SECTION_COUNT
            ],
            revision: 0,
        }
    }

    pub fn from_voxels(position: ChunkPos, biome: BiomeId, voxels: Vec<Voxel>) -> Self {
        debug_assert_eq!(voxels.len(), CHUNK_VOLUME);

        let mut sections = Vec::with_capacity(SECTION_COUNT);
        for section in 0..SECTION_COUNT {
            let start = section * SECTION_VOLUME;
            let end = start + SECTION_VOLUME;
            sections.push(PaletteSection::from_voxels(&voxels[start..end]));
        }

        Self {
            position,
            biome,
            sections,
            revision: 0,
        }
    }

    pub fn voxel(&self, local: LocalVoxelPos) -> Voxel {
        let section = section_index(local.y);
        let index = voxel_index(LocalVoxelPos { y: local.y % (SECTION_HEIGHT as u8), ..local }) % SECTION_VOLUME;
        self.sections[section].voxel(index)
    }

    pub fn set_voxel(&mut self, local: LocalVoxelPos, voxel: Voxel) {
        let section = section_index(local.y);
        let mut voxels = self.sections[section].expand();
        let section_local = LocalVoxelPos {
            x: local.x,
            y: local.y % (SECTION_HEIGHT as u8),
            z: local.z,
        };
        let index = voxel_index(section_local) % SECTION_VOLUME;
        voxels[index] = voxel;
        self.sections[section] = PaletteSection::from_voxels(&voxels);
        self.revision += 1;
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChunkDelta {
    pub position: ChunkPos,
    pub revision: u64,
    pub edits: Vec<(LocalVoxelPos, Voxel)>,
}

#[derive(Debug, Error)]
pub enum WorldError {
    #[error("failed to serialize chunk")]
    Serialize(#[from] Box<bincode::ErrorKind>),
}

pub fn serialize_chunk(chunk: &ChunkData) -> Result<Vec<u8>, WorldError> {
    Ok(bincode::serialize(chunk)?)
}

pub fn deserialize_chunk(bytes: &[u8]) -> Result<ChunkData, WorldError> {
    Ok(bincode::deserialize(bytes)?)
}

#[derive(Debug, Clone)]
pub struct TerrainGenerator {
    world_seed: u64,
}

impl TerrainGenerator {
    pub fn new(world_seed: u64) -> Self {
        Self { world_seed }
    }

    pub fn generate_chunk(&self, position: ChunkPos) -> ChunkData {
        let base = position.min_world_block();
        let center_x = base.x + i64::from(CHUNK_WIDTH / 2);
        let center_z = base.z + i64::from(CHUNK_DEPTH / 2);
        let biome = self.biome_at_world(center_x, center_z);
        let mut voxels = vec![Voxel::default(); CHUNK_VOLUME];

        for x in 0..CHUNK_WIDTH {
            for z in 0..CHUNK_DEPTH {
                let world_x = base.x + i64::from(x);
                let world_z = base.z + i64::from(z);
                let column_biome = self.biome_at_world(world_x, world_z);
                let surface = self.height_at(world_x, world_z, column_biome);

                for y in 0..=surface.min(CHUNK_HEIGHT - 1) {
                    let local = LocalVoxelPos { x: x as u8, y: y as u8, z: z as u8 };
                    let block = if y == surface {
                        match column_biome {
                            BiomeId::Desert => BlockId::Sand,
                            BiomeId::Alpine => BlockId::Stone,
                            _ => BlockId::Grass,
                        }
                    } else if y > surface - 4 {
                        match column_biome {
                            BiomeId::Desert => BlockId::Sand,
                            _ => BlockId::Dirt,
                        }
                    } else {
                        BlockId::Stone
                    };
                    let index = linear_index(local);
                    voxels[index] = Voxel { block };
                }

                if matches!(column_biome, BiomeId::Forest) && self.hash(world_x, world_z, 99) % 23 == 0 {
                    self.place_tree(&mut voxels, x as u8, (surface + 1) as u8, z as u8);
                }
            }
        }

        ChunkData::from_voxels(position, biome, voxels)
    }

    pub fn surface_height(&self, x: i64, z: i64) -> i32 {
        let biome = self.biome_at_world(x, z);
        self.height_at(x, z, biome)
    }

    fn biome_at_world(&self, x: i64, z: i64) -> BiomeId {
        let temperature = self.value_noise(x as f32, z as f32, 144.0, 7);
        let moisture = self.value_noise(x as f32, z as f32, 144.0, 17);
        let elevation = self.value_noise(x as f32, z as f32, 220.0, 23);

        if elevation > 0.7 {
            BiomeId::Alpine
        } else if temperature > 0.58 && moisture < 0.42 {
            BiomeId::Desert
        } else if moisture > 0.57 {
            BiomeId::Forest
        } else {
            BiomeId::Plains
        }
    }

    fn height_at(&self, x: i64, z: i64, biome: BiomeId) -> i32 {
        let broad = self.value_noise(x as f32, z as f32, 96.0, 13);
        let rolling = self.value_noise(x as f32, z as f32, 42.0, 29);
        let detail = self.value_noise(x as f32, z as f32, 18.0, 41);
        let biome_offset = match biome {
            BiomeId::Plains => 60.0,
            BiomeId::Forest => 63.0,
            BiomeId::Desert => 58.0,
            BiomeId::Alpine => 70.0,
        };
        let biome_scale = match biome {
            BiomeId::Plains => 7.0,
            BiomeId::Forest => 9.0,
            BiomeId::Desert => 6.0,
            BiomeId::Alpine => 14.0,
        };

        (biome_offset + broad * biome_scale + rolling * 5.0 + detail * 2.0).round() as i32
    }

    fn value_noise(&self, x: f32, z: f32, scale: f32, salt: u64) -> f32 {
        let gx = (x / scale).floor();
        let gz = (z / scale).floor();
        let fx = smoothstep((x / scale) - gx);
        let fz = smoothstep((z / scale) - gz);

        let x0 = gx as i64;
        let z0 = gz as i64;
        let x1 = x0 + 1;
        let z1 = z0 + 1;

        let n00 = self.noise_value(x0, z0, salt);
        let n10 = self.noise_value(x1, z0, salt);
        let n01 = self.noise_value(x0, z1, salt);
        let n11 = self.noise_value(x1, z1, salt);
        let nx0 = lerp(n00, n10, fx);
        let nx1 = lerp(n01, n11, fx);
        lerp(nx0, nx1, fz)
    }

    fn noise_value(&self, x: i64, z: i64, salt: u64) -> f32 {
        (self.hash(x, z, salt) as f64 / u64::MAX as f64) as f32
    }

    fn place_tree(&self, voxels: &mut [Voxel], x: u8, y: u8, z: u8) {
        if usize::from(y) + 6 >= CHUNK_HEIGHT as usize {
            return;
        }

        for trunk in y..(y + 4) {
            voxels[linear_index(LocalVoxelPos { x, y: trunk, z })] = Voxel { block: BlockId::Log };
        }

        for dy in 3..=5 {
            for dx in -2_i32..=2 {
                for dz in -2_i32..=2 {
                    if dx.abs() + dz.abs() > 3 {
                        continue;
                    }

                    let leaf_x = x as i32 + dx;
                    let leaf_z = z as i32 + dz;
                    if !(0..CHUNK_WIDTH).contains(&leaf_x) || !(0..CHUNK_DEPTH).contains(&leaf_z) {
                        continue;
                    }

                    voxels[linear_index(LocalVoxelPos {
                        x: leaf_x as u8,
                        y: y + dy,
                        z: leaf_z as u8,
                    })] = Voxel { block: BlockId::Leaves };
                }
            }
        }
    }

    fn hash(&self, x: i64, z: i64, salt: u64) -> u64 {
        let mut value = self.world_seed ^ salt;
        value ^= (x as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15);
        value = value.rotate_left(17);
        value ^= (z as u64).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        value ^= value >> 31;
        value = value.wrapping_mul(0x94D0_49BB_1331_11EB);
        value ^ (value >> 30)
    }
}

fn lerp(a: f32, b: f32, t: f32) -> f32 {
    a + (b - a) * t
}

fn smoothstep(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

fn linear_index(local: LocalVoxelPos) -> usize {
    usize::from(local.y) * (CHUNK_WIDTH as usize) * (CHUNK_DEPTH as usize)
        + usize::from(local.z) * (CHUNK_WIDTH as usize)
        + usize::from(local.x)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn palette_round_trips_voxel_data() {
        let voxels = vec![
            Voxel { block: BlockId::Air },
            Voxel { block: BlockId::Grass },
            Voxel { block: BlockId::Grass },
            Voxel { block: BlockId::Stone },
        ];

        let palette = PaletteSection::from_voxels(&voxels);
        assert_eq!(palette.expand(), voxels);
    }

    #[test]
    fn terrain_is_deterministic_for_seed() {
        let generator = TerrainGenerator::new(42);
        let a = generator.generate_chunk(ChunkPos { x: 3, z: -7 });
        let b = generator.generate_chunk(ChunkPos { x: 3, z: -7 });

        assert_eq!(serialize_chunk(&a).unwrap(), serialize_chunk(&b).unwrap());
    }
}
