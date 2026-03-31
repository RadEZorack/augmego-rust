import { describe, expect, it } from "vitest";
import {
  ChunkPos,
  WorldPos,
  chunkPosFromWorld,
  desiredChunkSet,
  orderedChunkPositions,
  raycastGrid,
  toChunkLocal,
  voxelIndex,
} from "@/src/game/shared/math";

describe("math helpers", () => {
  it("converts negative world positions to chunk-local space", () => {
    const position: WorldPos = { x: -1, y: 42, z: -33 };
    const { chunk, local } = toChunkLocal(position);

    expect(chunk).toEqual<ChunkPos>({ x: -1, z: -2 });
    expect(local).toEqual({ x: 31, y: 42, z: 31 });
  });

  it("orders chunk positions center-first and then by rings", () => {
    const ordered = orderedChunkPositions({ x: 10, z: -4 }, 2);

    expect(ordered[0]).toEqual({ x: 10, z: -4 });
    expect(ordered).toHaveLength(25);
    expect(ordered.slice(0, 9)).toContainEqual({ x: 11, z: -4 });
    expect(ordered.slice(0, 9)).toContainEqual({ x: 9, z: -5 });
  });

  it("builds the desired chunk set as a square area", () => {
    const set = desiredChunkSet({ x: 0, z: 0 }, 3);

    expect(set.size).toBe(49);
    expect(set.has("-3,2")).toBe(true);
    expect(set.has("3,-3")).toBe(true);
  });

  it("computes linear voxel indexes", () => {
    expect(voxelIndex({ x: 2, y: 3, z: 4 })).toBe(3 * 32 * 32 + 4 * 32 + 2);
  });

  it("raycasts through monotonically changing grid cells", () => {
    const visited = raycastGrid([0.2, 65.8, 0.2], [1, -0.2, 0], 6);

    expect(visited[0]).toEqual({ x: 0, y: 65, z: 0 });
    expect(visited.length).toBeGreaterThan(6);
    expect(chunkPosFromWorld(visited[visited.length - 1]!)).toEqual({ x: 0, z: 0 });
  });
});
