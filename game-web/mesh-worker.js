const CHUNK_WIDTH = 32;
const CHUNK_HEIGHT = 256;
const CHUNK_DEPTH = 32;
const CHUNK_WORLD_RADIUS = CHUNK_WIDTH * 0.5;

self.onmessage = (event) => {
  const data = event.data;
  if (!data || data.kind !== "build") {
    return;
  }

  const { x, z } = data;

  try {
    const mesh = buildChunkMesh(x, z);
    self.postMessage(
      {
        kind: "mesh",
        x,
        z,
        vertices: mesh.vertices.buffer,
        indices: mesh.indices.buffer,
      },
      [mesh.vertices.buffer, mesh.indices.buffer],
    );
  } catch (error) {
    self.postMessage({
      kind: "error",
      x,
      z,
      message: error instanceof Error ? error.message : String(error),
    });
  }
};

function buildChunkMesh(chunkX, chunkZ) {
  const voxels = generateChunk(chunkX, chunkZ);
  const vertices = [];
  const indices = [];
  const originX = chunkX * CHUNK_WIDTH;
  const originZ = chunkZ * CHUNK_DEPTH;

  for (let y = 0; y < CHUNK_HEIGHT; y++) {
    for (let z = 0; z < CHUNK_DEPTH; z++) {
      for (let x = 0; x < CHUNK_WIDTH; x++) {
        const block = voxels[linearIndex(x, y, z)];
        if (block === 0 || block === 5) {
          continue;
        }

        emitBlockFaces(voxels, vertices, indices, [originX + x, y, originZ + z], x, y, z, block);
      }
    }
  }

  return {
    vertices: new Float32Array(vertices),
    indices: new Uint32Array(indices),
  };
}

function generateChunk(chunkX, chunkZ) {
  const voxels = new Uint16Array(CHUNK_WIDTH * CHUNK_HEIGHT * CHUNK_DEPTH);
  const baseX = chunkX * CHUNK_WIDTH;
  const baseZ = chunkZ * CHUNK_DEPTH;
  const biome = biomeAt(chunkX, chunkZ);

  for (let x = 0; x < CHUNK_WIDTH; x++) {
    for (let z = 0; z < CHUNK_DEPTH; z++) {
      const worldX = baseX + x;
      const worldZ = baseZ + z;
      const surface = heightAt(worldX, worldZ, biome);

      for (let y = 0; y <= Math.min(surface, CHUNK_HEIGHT - 1); y++) {
        let block;
        if (y === surface) {
          block = biome === 2 ? 4 : biome === 3 ? 3 : 1;
        } else if (y > surface - 4) {
          block = biome === 2 ? 4 : 2;
        } else {
          block = 3;
        }

        voxels[linearIndex(x, y, z)] = block;
      }

      if (biome === 1 && hash(worldX, worldZ, 99) % 19n === 0n) {
        placeTree(voxels, x, surface + 1, z);
      }
    }
  }

  return voxels;
}

function biomeAt(chunkX, chunkZ) {
  return Number(hash(chunkX, chunkZ, 7) % 4n);
}

function heightAt(x, z, biome) {
  const coarse = Number(hash(Math.floor(x / 8), Math.floor(z / 8), 13) % 16n);
  const fine = Number(hash(x, z, 29) % 7n);
  const biomeOffset = biome === 0 ? 58 : biome === 1 ? 62 : biome === 2 ? 54 : 78;
  return biomeOffset + coarse + fine;
}

function placeTree(voxels, x, y, z) {
  if (y + 6 >= CHUNK_HEIGHT) {
    return;
  }

  for (let trunk = y; trunk < y + 4; trunk++) {
    voxels[linearIndex(x, trunk, z)] = 6;
  }

  for (let dy = 3; dy <= 5; dy++) {
    for (let dx = -2; dx <= 2; dx++) {
      for (let dz = -2; dz <= 2; dz++) {
        if (Math.abs(dx) + Math.abs(dz) > 3) {
          continue;
        }

        const leafX = x + dx;
        const leafZ = z + dz;
        if (leafX < 0 || leafX >= CHUNK_WIDTH || leafZ < 0 || leafZ >= CHUNK_DEPTH) {
          continue;
        }

        voxels[linearIndex(leafX, y + dy, leafZ)] = 7;
      }
    }
  }
}

function emitBlockFaces(voxels, vertices, indices, world, x, y, z, block) {
  const baseColor = [1, 1, 1];
  const faces = [
    { offset: [0, 0, -1], vertices: faceVertices(world, "north", baseColor, tileUvs(block, "north")) },
    { offset: [0, 0, 1], vertices: faceVertices(world, "south", baseColor, tileUvs(block, "south")) },
    { offset: [-1, 0, 0], vertices: faceVertices(world, "west", baseColor, tileUvs(block, "west")) },
    { offset: [1, 0, 0], vertices: faceVertices(world, "east", baseColor, tileUvs(block, "east")) },
    { offset: [0, 1, 0], vertices: faceVertices(world, "up", brighten(baseColor, 0.08), tileUvs(block, "up")) },
    { offset: [0, -1, 0], vertices: faceVertices(world, "down", darken(baseColor, 0.16), tileUvs(block, "down")) },
  ];

  for (const face of faces) {
    const neighbor = sampleVoxel(voxels, x + face.offset[0], y + face.offset[1], z + face.offset[2]);
    if (neighbor === null || isTransparent(neighbor)) {
      const base = vertices.length / 8;
      vertices.push(...face.vertices.flat());
      indices.push(base, base + 1, base + 2, base, base + 2, base + 3);
    }
  }
}

function sampleVoxel(voxels, x, y, z) {
  if (x < 0 || x >= CHUNK_WIDTH || y < 0 || y >= CHUNK_HEIGHT || z < 0 || z >= CHUNK_DEPTH) {
    return null;
  }
  return voxels[linearIndex(x, y, z)];
}

function isTransparent(block) {
  return block === 0 || block === 5 || block === 7 || block === 9;
}

function faceVertices(origin, face, color, uvs) {
  const [x, y, z] = origin;
  const make = (px, py, pz, uv) => [px, py, pz, color[0], color[1], color[2], uv[0], uv[1]];
  switch (face) {
    case "north":
      return [make(x, y + 1, z, uvs[0]), make(x + 1, y + 1, z, uvs[1]), make(x + 1, y, z, uvs[2]), make(x, y, z, uvs[3])];
    case "south":
      return [make(x + 1, y + 1, z + 1, uvs[0]), make(x, y + 1, z + 1, uvs[1]), make(x, y, z + 1, uvs[2]), make(x + 1, y, z + 1, uvs[3])];
    case "east":
      return [make(x + 1, y + 1, z, uvs[0]), make(x + 1, y + 1, z + 1, uvs[1]), make(x + 1, y, z + 1, uvs[2]), make(x + 1, y, z, uvs[3])];
    case "west":
      return [make(x, y + 1, z + 1, uvs[0]), make(x, y + 1, z, uvs[1]), make(x, y, z, uvs[2]), make(x, y, z + 1, uvs[3])];
    case "up":
      return [make(x, y + 1, z, uvs[0]), make(x, y + 1, z + 1, uvs[1]), make(x + 1, y + 1, z + 1, uvs[2]), make(x + 1, y + 1, z, uvs[3])];
    default:
      return [make(x, y, z, uvs[0]), make(x + 1, y, z, uvs[1]), make(x + 1, y, z + 1, uvs[2]), make(x, y, z + 1, uvs[3])];
  }
}

function tileUvs(block, face) {
  return atlasQuad(tileFor(block, face));
}

function tileFor(block, face) {
  switch (block) {
    case 1:
      return face === "up" ? [1, 0] : face === "down" ? [0, 0] : [1, 1];
    case 2:
      return [0, 0];
    case 3:
      return [2, 0];
    case 4:
      return [3, 0];
    case 5:
      return [2, 1];
    case 6:
      return face === "up" || face === "down" ? [3, 1] : [0, 1];
    case 7:
      return [1, 1];
    case 8:
      return [3, 1];
    case 9:
      return [2, 1];
    case 10:
      return [3, 1];
    case 11:
      return [0, 1];
    default:
      return [0, 0];
  }
}

function atlasQuad(tile) {
  const tileCount = 4;
  const eps = 0.001;
  const minU = tile[0] / tileCount + eps;
  const maxU = (tile[0] + 1) / tileCount - eps;
  const minV = tile[1] / tileCount + eps;
  const maxV = (tile[1] + 1) / tileCount - eps;
  return [[minU, minV], [maxU, minV], [maxU, maxV], [minU, maxV]];
}

function darken(color, amount) {
  return color.map((value) => value * (1 - amount));
}

function brighten(color, amount) {
  return color.map((value) => Math.min(1, value + amount));
}

function linearIndex(x, y, z) {
  return y * CHUNK_WIDTH * CHUNK_DEPTH + z * CHUNK_WIDTH + x;
}

function hash(x, z, salt) {
  const seed = 0xA66DE601n;
  let value = seed ^ BigInt(salt);
  value ^= BigInt.asUintN(64, BigInt(x)) * 0x9E3779B97F4A7C15n;
  value = rotateLeft64(value, 17n);
  value ^= BigInt.asUintN(64, BigInt(z)) * 0xBF58476D1CE4E5B9n;
  value ^= value >> 31n;
  value = BigInt.asUintN(64, value * 0x94D049BB133111EBn);
  return BigInt.asUintN(64, value ^ (value >> 30n));
}

function rotateLeft64(value, bits) {
  return BigInt.asUintN(64, (value << bits) | (value >> (64n - bits)));
}
