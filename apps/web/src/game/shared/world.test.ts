import { describe, expect, it } from "vitest";
import { BlockId, currentAvatarUrl, normalizeAvatarSelection } from "@/src/game/shared/content";
import { ChunkPos } from "@/src/game/shared/math";
import {
  BiomeId,
  TerrainGenerator,
  chunkFromVoxels,
  cloneChunkData,
  createEmptyChunk,
  deserializeChunk,
  serializeChunk,
  setChunkVoxel,
  withinReach,
} from "@/src/game/shared/world";

describe("world helpers", () => {
  it("round-trips chunk data through messagepack storage", () => {
    const chunk = createEmptyChunk({ x: 1, z: -2 }, BiomeId.Forest);
    setChunkVoxel(chunk, { x: 4, y: 72, z: 9 }, { block: BlockId.GoldOre });

    const restored = deserializeChunk(serializeChunk(chunk));

    expect(restored.position).toEqual(chunk.position);
    expect(restored.revision).toBe(chunk.revision);
    expect(restored.sections[4]?.palette).toContain(BlockId.GoldOre);
  });

  it("keeps terrain generation deterministic for a given seed", () => {
    const generator = new TerrainGenerator(42);
    const a = generator.generateChunk({ x: 3, z: -7 });
    const b = generator.generateChunk({ x: 3, z: -7 });

    expect(serializeChunk(a)).toEqual(serializeChunk(b));
  });

  it("preserves palette-based chunk edits when cloning", () => {
    const source = createEmptyChunk({ x: 0, z: 0 }, BiomeId.Plains);
    setChunkVoxel(source, { x: 3, y: 60, z: 5 }, { block: BlockId.Stone });

    const copy = cloneChunkData(source);
    setChunkVoxel(copy, { x: 4, y: 60, z: 5 }, { block: BlockId.Glass });

    expect(source.revision).toBe(1);
    expect(copy.revision).toBe(2);
  });

  it("builds chunks from explicit voxel arrays", () => {
    const voxels = new Array(32 * 256 * 32).fill({ block: BlockId.Air });
    voxels[0] = { block: BlockId.Grass };
    const chunk = chunkFromVoxels({ x: 0, z: 0 }, BiomeId.Plains, voxels);

    expect(chunk.position).toEqual<ChunkPos>({ x: 0, z: 0 });
    expect(chunk.sections[0]?.palette).toContain(BlockId.Grass);
  });

  it("normalizes avatar selection and chooses current avatar URLs", () => {
    const selection = normalizeAvatarSelection({
      stationaryModelUrl: " https://example.com/idle.glb ",
      moveModelUrl: "https://example.com/run.glb",
      specialModelUrl: "https://example.com/dance.glb",
    });

    expect(selection.stationaryModelUrl).toBe("https://example.com/idle.glb");
    expect(currentAvatarUrl(selection, 0.4, 0)).toBe("https://example.com/run.glb");
    expect(currentAvatarUrl(selection, 0, 9)).toBe("https://example.com/dance.glb");
  });

  it("keeps block reach checks aligned with the gameplay rules", () => {
    expect(withinReach([2, 91, -3], { x: 2, y: 91, z: -3 })).toBe(true);
    expect(withinReach([2, 91, -3], { x: 20, y: 91, z: 0 })).toBe(false);
  });
});
