use serde::{Deserialize, Serialize};
use thiserror::Error;

pub const CHUNK_WIDTH: i32 = 32;
pub const CHUNK_HEIGHT: i32 = 256;
pub const CHUNK_DEPTH: i32 = 32;
pub const SECTION_HEIGHT: i32 = 16;
pub const SECTION_COUNT: usize = (CHUNK_HEIGHT / SECTION_HEIGHT) as usize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct WorldPos {
    pub x: i64,
    pub y: i32,
    pub z: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ChunkPos {
    pub x: i32,
    pub z: i32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct LocalVoxelPos {
    pub x: u8,
    pub y: u8,
    pub z: u8,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Direction {
    Up,
    Down,
    North,
    South,
    East,
    West,
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum MathError {
    #[error("world y {0} is outside the chunk bounds")]
    WorldYOutOfBounds(i32),
}

impl ChunkPos {
    pub fn from_world(pos: WorldPos) -> Self {
        Self {
            x: div_floor_i64(pos.x, i64::from(CHUNK_WIDTH)) as i32,
            z: div_floor_i64(pos.z, i64::from(CHUNK_DEPTH)) as i32,
        }
    }

    pub fn min_world_block(self) -> WorldPos {
        WorldPos {
            x: i64::from(self.x) * i64::from(CHUNK_WIDTH),
            y: 0,
            z: i64::from(self.z) * i64::from(CHUNK_DEPTH),
        }
    }
}

impl LocalVoxelPos {
    pub fn from_world(pos: WorldPos) -> Result<Self, MathError> {
        if !(0..CHUNK_HEIGHT).contains(&pos.y) {
            return Err(MathError::WorldYOutOfBounds(pos.y));
        }

        Ok(Self {
            x: mod_floor_i64(pos.x, i64::from(CHUNK_WIDTH)) as u8,
            y: pos.y as u8,
            z: mod_floor_i64(pos.z, i64::from(CHUNK_DEPTH)) as u8,
        })
    }
}

impl WorldPos {
    pub fn to_chunk_local(self) -> Result<(ChunkPos, LocalVoxelPos), MathError> {
        Ok((ChunkPos::from_world(self), LocalVoxelPos::from_world(self)?))
    }

    pub fn offset(self, dx: i64, dy: i32, dz: i64) -> Self {
        Self {
            x: self.x + dx,
            y: self.y + dy,
            z: self.z + dz,
        }
    }
}

pub fn voxel_index(local: LocalVoxelPos) -> usize {
    usize::from(local.y) * (CHUNK_WIDTH as usize) * (CHUNK_DEPTH as usize)
        + usize::from(local.z) * (CHUNK_WIDTH as usize)
        + usize::from(local.x)
}

pub fn section_index(y: u8) -> usize {
    usize::from(y) / (SECTION_HEIGHT as usize)
}

pub fn raycast_grid(origin: [f32; 3], direction: [f32; 3], max_distance: f32) -> Vec<WorldPos> {
    let mut results = Vec::new();
    let length = (direction[0] * direction[0] + direction[1] * direction[1] + direction[2] * direction[2]).sqrt();
    if length <= f32::EPSILON {
        return results;
    }

    let norm = [direction[0] / length, direction[1] / length, direction[2] / length];
    let steps = (max_distance * 8.0).ceil() as i32;
    let mut previous = None;

    for step in 0..=steps {
        let t = (step as f32 / steps.max(1) as f32) * max_distance;
        let candidate = WorldPos {
            x: (origin[0] + norm[0] * t).floor() as i64,
            y: (origin[1] + norm[1] * t).floor() as i32,
            z: (origin[2] + norm[2] * t).floor() as i64,
        };

        if previous != Some(candidate) {
            results.push(candidate);
            previous = Some(candidate);
        }
    }

    results
}

fn div_floor_i64(value: i64, divisor: i64) -> i64 {
    value.div_euclid(divisor)
}

fn mod_floor_i64(value: i64, modulus: i64) -> i64 {
    value.rem_euclid(modulus)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn converts_negative_world_positions_to_chunk_space() {
        let pos = WorldPos { x: -1, y: 42, z: -33 };
        let (chunk, local) = pos.to_chunk_local().expect("valid position");

        assert_eq!(chunk, ChunkPos { x: -1, z: -2 });
        assert_eq!(local, LocalVoxelPos { x: 31, y: 42, z: 31 });
    }

    #[test]
    fn computes_voxel_index_in_linear_storage() {
        let local = LocalVoxelPos { x: 2, y: 3, z: 4 };
        assert_eq!(voxel_index(local), 3 * 32 * 32 + 4 * 32 + 2);
    }

    #[test]
    fn raycast_grid_returns_monotonic_cells() {
        let visited = raycast_grid([0.2, 65.8, 0.2], [1.0, -0.2, 0.0], 6.0);
        assert!(visited.len() > 6);
        assert_eq!(visited.first(), Some(&WorldPos { x: 0, y: 65, z: 0 }));
    }
}
