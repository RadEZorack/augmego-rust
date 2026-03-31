export const CHUNK_WIDTH = 32;
export const CHUNK_HEIGHT = 256;
export const CHUNK_DEPTH = 32;
export const SECTION_HEIGHT = 16;
export const SECTION_COUNT = CHUNK_HEIGHT / SECTION_HEIGHT;

export type WorldPos = {
  x: number;
  y: number;
  z: number;
};

export type ChunkPos = {
  x: number;
  z: number;
};

export type LocalVoxelPos = {
  x: number;
  y: number;
  z: number;
};

export type Vec3Tuple = [number, number, number];

export function clamp(value: number, min: number, max: number) {
  return Math.min(max, Math.max(min, value));
}

export function chunkKey(position: ChunkPos) {
  return `${position.x},${position.z}`;
}

export function parseChunkKey(key: string): ChunkPos {
  const [x, z] = key.split(",").map((value) => Number.parseInt(value, 10));
  return {
    x: Number.isFinite(x) ? x : 0,
    z: Number.isFinite(z) ? z : 0,
  };
}

export function divFloor(value: number, divisor: number) {
  return Math.floor(value / divisor);
}

export function modFloor(value: number, divisor: number) {
  return ((value % divisor) + divisor) % divisor;
}

export function chunkPosFromWorld(position: WorldPos): ChunkPos {
  return {
    x: divFloor(position.x, CHUNK_WIDTH),
    z: divFloor(position.z, CHUNK_DEPTH),
  };
}

export function minWorldBlock(position: ChunkPos): WorldPos {
  return {
    x: position.x * CHUNK_WIDTH,
    y: 0,
    z: position.z * CHUNK_DEPTH,
  };
}

export function localVoxelFromWorld(position: WorldPos): LocalVoxelPos {
  if (position.y < 0 || position.y >= CHUNK_HEIGHT) {
    throw new Error(`World Y ${position.y} is outside chunk bounds`);
  }

  return {
    x: modFloor(position.x, CHUNK_WIDTH),
    y: position.y,
    z: modFloor(position.z, CHUNK_DEPTH),
  };
}

export function toChunkLocal(position: WorldPos) {
  return {
    chunk: chunkPosFromWorld(position),
    local: localVoxelFromWorld(position),
  };
}

export function voxelIndex(local: LocalVoxelPos) {
  return local.y * CHUNK_WIDTH * CHUNK_DEPTH + local.z * CHUNK_WIDTH + local.x;
}

export function sectionIndex(y: number) {
  return Math.floor(y / SECTION_HEIGHT);
}

export function worldPosOffset(position: WorldPos, dx: number, dy: number, dz: number): WorldPos {
  return {
    x: position.x + dx,
    y: position.y + dy,
    z: position.z + dz,
  };
}

export function orderedChunkPositions(center: ChunkPos, radius: number) {
  const positions: ChunkPos[] = [center];

  for (let ring = 1; ring <= radius; ring += 1) {
    for (let dz = -ring; dz <= ring; dz += 1) {
      for (let dx = -ring; dx <= ring; dx += 1) {
        if (Math.max(Math.abs(dx), Math.abs(dz)) !== ring) {
          continue;
        }

        positions.push({
          x: center.x + dx,
          z: center.z + dz,
        });
      }
    }
  }

  return positions;
}

export function desiredChunkSet(center: ChunkPos, radius: number) {
  return new Set(orderedChunkPositions(center, radius).map(chunkKey));
}

export function raycastGrid(origin: Vec3Tuple, direction: Vec3Tuple, maxDistance: number) {
  const length = Math.hypot(direction[0], direction[1], direction[2]);
  if (length <= Number.EPSILON) {
    return [] as WorldPos[];
  }

  const normalized: Vec3Tuple = [
    direction[0] / length,
    direction[1] / length,
    direction[2] / length,
  ];
  const steps = Math.ceil(maxDistance * 8);
  const results: WorldPos[] = [];
  let previous: string | null = null;

  for (let step = 0; step <= steps; step += 1) {
    const t = (step / Math.max(steps, 1)) * maxDistance;
    const candidate: WorldPos = {
      x: Math.floor(origin[0] + normalized[0] * t),
      y: Math.floor(origin[1] + normalized[1] * t),
      z: Math.floor(origin[2] + normalized[2] * t),
    };
    const key = `${candidate.x}:${candidate.y}:${candidate.z}`;
    if (key !== previous) {
      results.push(candidate);
      previous = key;
    }
  }

  return results;
}
