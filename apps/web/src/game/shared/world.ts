import { Packr, Unpackr } from "msgpackr";
import {
  CHUNK_DEPTH,
  CHUNK_HEIGHT,
  CHUNK_WIDTH,
  SECTION_COUNT,
  SECTION_HEIGHT,
  LocalVoxelPos,
  WorldPos,
  ChunkPos,
  clamp,
  minWorldBlock,
  sectionIndex,
  toChunkLocal,
  voxelIndex,
} from "@/src/game/shared/math";
import { BlockId, blockIsTransparent } from "@/src/game/shared/content";

export enum BiomeId {
  Plains = "Plains",
  Forest = "Forest",
  Desert = "Desert",
  Alpine = "Alpine",
}

export type Voxel = {
  block: BlockId;
};

export type PaletteSection = {
  palette: BlockId[];
  indices: number[];
};

export type ChunkData = {
  position: ChunkPos;
  biome: BiomeId;
  sections: PaletteSection[];
  revision: number;
};

export const SECTION_VOLUME = CHUNK_WIDTH * SECTION_HEIGHT * CHUNK_DEPTH;
export const CHUNK_VOLUME = CHUNK_WIDTH * CHUNK_HEIGHT * CHUNK_DEPTH;

const packr = new Packr({
  structuredClone: true,
});
const unpackr = new Unpackr();
const U64_MASK = (1n << 64n) - 1n;
const U64_MAX = (1n << 64n) - 1n;

export function defaultVoxel(): Voxel {
  return { block: BlockId.Air };
}

export function createEmptyChunk(position: ChunkPos, biome: BiomeId): ChunkData {
  return {
    position,
    biome,
    sections: Array.from({ length: SECTION_COUNT }, () => ({
      palette: [BlockId.Air],
      indices: new Array(SECTION_VOLUME).fill(0),
    })),
    revision: 0,
  };
}

export function paletteSectionFromVoxels(voxels: Voxel[]): PaletteSection {
  const palette: BlockId[] = [];
  const indices: number[] = [];

  for (const voxel of voxels) {
    let paletteIndex = palette.indexOf(voxel.block);
    if (paletteIndex === -1) {
      palette.push(voxel.block);
      paletteIndex = palette.length - 1;
    }
    indices.push(paletteIndex);
  }

  return { palette, indices };
}

export function expandSection(section: PaletteSection): Voxel[] {
  return section.indices.map((index) => ({
    block: section.palette[index] ?? BlockId.Air,
  }));
}

export function chunkFromVoxels(position: ChunkPos, biome: BiomeId, voxels: Voxel[]): ChunkData {
  if (voxels.length !== CHUNK_VOLUME) {
    throw new Error(`Expected ${CHUNK_VOLUME} voxels, received ${voxels.length}`);
  }

  const sections: PaletteSection[] = [];
  for (let section = 0; section < SECTION_COUNT; section += 1) {
    const start = section * SECTION_VOLUME;
    const end = start + SECTION_VOLUME;
    sections.push(paletteSectionFromVoxels(voxels.slice(start, end)));
  }

  return {
    position,
    biome,
    sections,
    revision: 0,
  };
}

export function cloneChunkData(chunk: ChunkData): ChunkData {
  return {
    position: { ...chunk.position },
    biome: chunk.biome,
    revision: chunk.revision,
    sections: chunk.sections.map((section) => ({
      palette: [...section.palette],
      indices: [...section.indices],
    })),
  };
}

export function chunkVoxel(chunk: ChunkData, local: LocalVoxelPos): Voxel {
  const currentSection = sectionIndex(local.y);
  const yWithinSection = local.y % SECTION_HEIGHT;
  const index = voxelIndex({
    x: local.x,
    y: yWithinSection,
    z: local.z,
  }) % SECTION_VOLUME;
  const paletteSection = chunk.sections[currentSection];
  const paletteIndex = paletteSection.indices[index] ?? 0;
  return {
    block: paletteSection.palette[paletteIndex] ?? BlockId.Air,
  };
}

export function setChunkVoxel(chunk: ChunkData, local: LocalVoxelPos, voxel: Voxel) {
  const currentSection = sectionIndex(local.y);
  const voxels = expandSection(chunk.sections[currentSection]);
  const yWithinSection = local.y % SECTION_HEIGHT;
  const index = voxelIndex({
    x: local.x,
    y: yWithinSection,
    z: local.z,
  }) % SECTION_VOLUME;
  voxels[index] = voxel;
  chunk.sections[currentSection] = paletteSectionFromVoxels(voxels);
  chunk.revision += 1;
}

export function serializeChunk(chunk: ChunkData) {
  return packr.pack(chunk);
}

export function deserializeChunk(bytes: Uint8Array | Buffer) {
  return unpackr.unpack(bytes) as ChunkData;
}

export function worldBlockIsSolidFromChunk(chunk: ChunkData, local: LocalVoxelPos) {
  return !blockIsTransparent(chunkVoxel(chunk, local).block);
}

export class TerrainGenerator {
  constructor(private readonly worldSeed: number) {}

  generateChunk(position: ChunkPos): ChunkData {
    const base = minWorldBlock(position);
    const centerX = base.x + Math.floor(CHUNK_WIDTH / 2);
    const centerZ = base.z + Math.floor(CHUNK_DEPTH / 2);
    const biome = this.biomeAtWorld(centerX, centerZ);
    const voxels = Array.from({ length: CHUNK_VOLUME }, defaultVoxel);

    for (let x = 0; x < CHUNK_WIDTH; x += 1) {
      for (let z = 0; z < CHUNK_DEPTH; z += 1) {
        const worldX = base.x + x;
        const worldZ = base.z + z;
        const columnBiome = this.biomeAtWorld(worldX, worldZ);
        const surface = this.heightAt(worldX, worldZ, columnBiome);

        for (let y = 0; y <= Math.min(surface, CHUNK_HEIGHT - 1); y += 1) {
          const local = { x, y, z };
          const block =
            y === surface
              ? columnBiome === BiomeId.Desert
                ? BlockId.Sand
                : columnBiome === BiomeId.Alpine
                  ? BlockId.Stone
                  : BlockId.Grass
              : y > surface - 4
                ? columnBiome === BiomeId.Desert
                  ? BlockId.Sand
                  : BlockId.Dirt
                : BlockId.Stone;
          voxels[voxelIndex(local)] = { block };
        }

        if (columnBiome === BiomeId.Forest && this.hash(worldX, worldZ, 99) % 23n === 0n) {
          this.placeTree(voxels, x, clamp(surface + 1, 0, CHUNK_HEIGHT - 1), z);
        }
      }
    }

    return chunkFromVoxels(position, biome, voxels);
  }

  surfaceHeight(x: number, z: number) {
    const biome = this.biomeAtWorld(x, z);
    return this.heightAt(x, z, biome);
  }

  biomeAtWorld(x: number, z: number): BiomeId {
    const temperature = this.valueNoise(x, z, 144, 7);
    const moisture = this.valueNoise(x, z, 144, 17);
    const elevation = this.valueNoise(x, z, 220, 23);

    if (elevation > 0.7) {
      return BiomeId.Alpine;
    }
    if (temperature > 0.58 && moisture < 0.42) {
      return BiomeId.Desert;
    }
    if (moisture > 0.57) {
      return BiomeId.Forest;
    }
    return BiomeId.Plains;
  }

  private heightAt(x: number, z: number, biome: BiomeId) {
    const broad = this.valueNoise(x, z, 96, 13);
    const rolling = this.valueNoise(x, z, 42, 29);
    const detail = this.valueNoise(x, z, 18, 41);

    const biomeOffset =
      biome === BiomeId.Plains ? 60
      : biome === BiomeId.Forest ? 63
      : biome === BiomeId.Desert ? 58
      : 70;
    const biomeScale =
      biome === BiomeId.Plains ? 7
      : biome === BiomeId.Forest ? 9
      : biome === BiomeId.Desert ? 6
      : 14;

    return Math.round(biomeOffset + broad * biomeScale + rolling * 5 + detail * 2);
  }

  private valueNoise(x: number, z: number, scale: number, salt: number) {
    const gx = Math.floor(x / scale);
    const gz = Math.floor(z / scale);
    const fx = smoothstep(x / scale - gx);
    const fz = smoothstep(z / scale - gz);

    const x0 = gx;
    const z0 = gz;
    const x1 = x0 + 1;
    const z1 = z0 + 1;

    const n00 = this.noiseValue(x0, z0, salt);
    const n10 = this.noiseValue(x1, z0, salt);
    const n01 = this.noiseValue(x0, z1, salt);
    const n11 = this.noiseValue(x1, z1, salt);

    const nx0 = lerp(n00, n10, fx);
    const nx1 = lerp(n01, n11, fx);
    return lerp(nx0, nx1, fz);
  }

  private noiseValue(x: number, z: number, salt: number) {
    const value = this.hash(x, z, salt);
    const top53 = value >> 11n;
    return Number(top53) / Number((1n << 53n) - 1n);
  }

  private placeTree(voxels: Voxel[], x: number, y: number, z: number) {
    if (y + 6 >= CHUNK_HEIGHT) {
      return;
    }

    for (let trunk = y; trunk < y + 4; trunk += 1) {
      voxels[voxelIndex({ x, y: trunk, z })] = { block: BlockId.Log };
    }

    for (let dy = 3; dy <= 5; dy += 1) {
      for (let dx = -2; dx <= 2; dx += 1) {
        for (let dz = -2; dz <= 2; dz += 1) {
          if (Math.abs(dx) + Math.abs(dz) > 3) {
            continue;
          }

          const leafX = x + dx;
          const leafZ = z + dz;
          if (leafX < 0 || leafX >= CHUNK_WIDTH || leafZ < 0 || leafZ >= CHUNK_DEPTH) {
            continue;
          }

          voxels[voxelIndex({ x: leafX, y: y + dy, z: leafZ })] = { block: BlockId.Leaves };
        }
      }
    }
  }

  private hash(x: number, z: number, salt: number) {
    let value = BigInt.asUintN(64, BigInt(this.worldSeed >>> 0) ^ BigInt(salt >>> 0));
    value = BigInt.asUintN(
      64,
      value ^ BigInt.asUintN(64, BigInt(x) * 0x9e3779b97f4a7c15n),
    );
    value = rotateLeft64(value, 17n);
    value = BigInt.asUintN(
      64,
      value ^ BigInt.asUintN(64, BigInt(z) * 0xbf58476d1ce4e5b9n),
    );
    value = BigInt.asUintN(64, value ^ (value >> 31n));
    value = BigInt.asUintN(64, value * 0x94d049bb133111ebn);
    return BigInt.asUintN(64, value ^ (value >> 30n));
  }
}

export function withinReach(playerPosition: [number, number, number], target: WorldPos) {
  const origin: [number, number, number] = [playerPosition[0], playerPosition[1] + 1.6, playerPosition[2]];
  const dx = target.x + 0.5 - origin[0];
  const dy = target.y + 0.5 - origin[1];
  const dz = target.z + 0.5 - origin[2];
  return dx * dx + dy * dy + dz * dz <= 8 ** 2;
}

export function worldPositionToChunkBounds(position: WorldPos) {
  return toChunkLocal(position);
}

export function iterateChunkBlocks(chunk: ChunkData, visit: (block: BlockId, local: LocalVoxelPos) => void) {
  for (let y = 0; y < CHUNK_HEIGHT; y += 1) {
    for (let z = 0; z < CHUNK_DEPTH; z += 1) {
      for (let x = 0; x < CHUNK_WIDTH; x += 1) {
        const local = { x, y, z };
        visit(chunkVoxel(chunk, local).block, local);
      }
    }
  }
}

export function worldBlockAt(getChunk: (position: ChunkPos) => ChunkData, position: WorldPos) {
  const { chunk, local } = toChunkLocal(position);
  return chunkVoxel(getChunk(chunk), local).block;
}

function lerp(a: number, b: number, t: number) {
  return a + (b - a) * t;
}

function smoothstep(t: number) {
  const value = clamp(t, 0, 1);
  return value * value * (3 - 2 * value);
}

function rotateLeft64(value: bigint, shift: bigint) {
  return BigInt.asUintN(64, ((value << shift) & U64_MASK) | (value >> (64n - shift)));
}
