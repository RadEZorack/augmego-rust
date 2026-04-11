const CHUNK_WIDTH = 32;
const CHUNK_HEIGHT = 256;
const CHUNK_DEPTH = 32;
const CHUNK_WORLD_RADIUS = CHUNK_WIDTH * 0.5;
const DEFAULT_WORLD_SEED = 0xA66DE601n;
const ATLAS_TILE_COUNT = 12;
const ATLAS_UV_EPS = 0.001;
const COAL_ORE_ATLAS_TILE = [9, 4];

if (typeof self !== "undefined") {
  self.onmessage = (event) => {
    const data = event.data;
    if (!data) {
      return;
    }

    const { x, z, kind } = data;

    try {
      let mesh;
      if (kind === "build") {
        mesh = buildChunkMesh(
          x,
          z,
          Array.isArray(data.edits) ? data.edits : [],
          parseWorldSeed(data.worldSeed),
        );
      } else if (kind === "mesh_chunk") {
        mesh = buildChunkMeshFromVoxels(x, z, new Uint16Array(data.voxels));
      } else {
        return;
      }

      self.postMessage(
        {
          kind: "mesh",
          x,
          z,
          vertices: mesh.vertices.buffer,
          indices: mesh.indices.buffer,
          heights: mesh.heights.buffer,
          voxels: mesh.voxels.buffer,
        },
        [mesh.vertices.buffer, mesh.indices.buffer, mesh.heights.buffer, mesh.voxels.buffer],
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
}

function buildChunkMesh(chunkX, chunkZ, edits, worldSeed) {
  const { voxels, heights } = generateChunk(chunkX, chunkZ, worldSeed);
  applyEdits(voxels, heights, edits);
  return buildChunkMeshFromVoxels(chunkX, chunkZ, voxels);
}

function buildChunkMeshFromVoxels(chunkX, chunkZ, voxelsInput) {
  const voxels = voxelsInput instanceof Uint16Array ? voxelsInput : new Uint16Array(voxelsInput);
  const heights = new Uint16Array(CHUNK_WIDTH * CHUNK_DEPTH);
  recomputeHeights(voxels, heights);
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
    heights,
    voxels,
  };
}

function generateChunk(chunkX, chunkZ, worldSeed) {
  const voxels = new Uint16Array(CHUNK_WIDTH * CHUNK_HEIGHT * CHUNK_DEPTH);
  const heights = new Uint16Array(CHUNK_WIDTH * CHUNK_DEPTH);
  const baseX = chunkX * CHUNK_WIDTH;
  const baseZ = chunkZ * CHUNK_DEPTH;
  const chunkCenterX = baseX + CHUNK_WIDTH / 2;
  const chunkCenterZ = baseZ + CHUNK_DEPTH / 2;
  const biome = biomeAt(chunkCenterX, chunkCenterZ, worldSeed);

  for (let x = 0; x < CHUNK_WIDTH; x++) {
    for (let z = 0; z < CHUNK_DEPTH; z++) {
      const worldX = baseX + x;
      const worldZ = baseZ + z;
      const columnBiome = biomeAt(worldX, worldZ, worldSeed);
      const surface = heightAt(worldX, worldZ, columnBiome, worldSeed);
      heights[z * CHUNK_WIDTH + x] = surface;

      for (let y = 0; y <= Math.min(surface, CHUNK_HEIGHT - 1); y++) {
        voxels[linearIndex(x, y, z)] = blockForColumn(worldX, worldZ, y, surface, columnBiome, worldSeed);
      }

      if (columnBiome === 1 && hash(worldSeed, worldX, worldZ, 99) % 23n === 0n) {
        placeTree(voxels, x, surface + 1, z);
      }
    }
  }

  return { voxels, heights };
}

function blockForColumn(worldX, worldZ, y, surface, biome, worldSeed) {
  if (y === surface) {
    return biome === 2 ? 4 : biome === 3 ? 3 : 1;
  }

  if (biome === 2) {
    if (y === surface - 1) {
      return 4;
    }
    if (y >= surface - 5 && y <= surface - 2) {
      return 15;
    }
  } else if (y > surface - 4) {
    return 2;
  }

  return decorateStoneBlock(worldX, worldZ, y, worldSeed);
}

function decorateStoneBlock(_worldX, _worldZ, _y, _worldSeed) {
  return 3;
}

function applyEdits(voxels, heights, edits) {
  for (const edit of edits) {
    if (!Array.isArray(edit) || edit.length !== 4) {
      continue;
    }

    const [x, y, z, block] = edit;
    if (x < 0 || x >= CHUNK_WIDTH || y < 0 || y >= CHUNK_HEIGHT || z < 0 || z >= CHUNK_DEPTH) {
      continue;
    }

    voxels[linearIndex(x, y, z)] = block;
  }

  recomputeHeights(voxels, heights);
}

function recomputeHeights(voxels, heights) {
  for (let z = 0; z < CHUNK_DEPTH; z++) {
    for (let x = 0; x < CHUNK_WIDTH; x++) {
      let surface = 0;
      for (let y = CHUNK_HEIGHT - 1; y >= 0; y--) {
        const block = voxels[linearIndex(x, y, z)];
        if (block !== 0) {
          surface = y;
          break;
        }
      }
      heights[z * CHUNK_WIDTH + x] = surface;
    }
  }
}

function biomeAt(x, z, worldSeed) {
  const temperature = valueNoise(x, z, 144, 7, worldSeed);
  const moisture = valueNoise(x, z, 144, 17, worldSeed);
  const elevation = valueNoise(x, z, 220, 23, worldSeed);

  if (elevation > 0.7) {
    return 3;
  }
  if (temperature > 0.58 && moisture < 0.42) {
    return 2;
  }
  if (moisture > 0.57) {
    return 1;
  }
  return 0;
}

function heightAt(x, z, biome, worldSeed) {
  const broad = valueNoise(x, z, 96, 13, worldSeed);
  const rolling = valueNoise(x, z, 42, 29, worldSeed);
  const detail = valueNoise(x, z, 18, 41, worldSeed);
  const biomeOffset = biome === 0 ? 60 : biome === 1 ? 63 : biome === 2 ? 58 : 70;
  const biomeScale = biome === 0 ? 7 : biome === 1 ? 9 : biome === 2 ? 6 : 14;
  return Math.round(biomeOffset + broad * biomeScale + rolling * 5 + detail * 2);
}

function valueNoise(x, z, scale, salt, worldSeed) {
  const scaledX = x / scale;
  const scaledZ = z / scale;
  const gx = Math.floor(scaledX);
  const gz = Math.floor(scaledZ);
  const fx = smoothstep(scaledX - gx);
  const fz = smoothstep(scaledZ - gz);
  const n00 = noiseValue(gx, gz, salt, worldSeed);
  const n10 = noiseValue(gx + 1, gz, salt, worldSeed);
  const n01 = noiseValue(gx, gz + 1, salt, worldSeed);
  const n11 = noiseValue(gx + 1, gz + 1, salt, worldSeed);
  const nx0 = lerp(n00, n10, fx);
  const nx1 = lerp(n01, n11, fx);
  return lerp(nx0, nx1, fz);
}

function noiseValue(x, z, salt, worldSeed) {
  return Number(hash(worldSeed, x, z, salt)) / Number((1n << 64n) - 1n);
}

function lerp(a, b, t) {
  return a + (b - a) * t;
}

function smoothstep(t) {
  const clamped = Math.max(0, Math.min(1, t));
  return clamped * clamped * (3 - 2 * clamped);
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
  const baseColor = blockBaseColor(block);
  const shadeColor = block === 13 ? [1, 1, 1] : baseColor;
  const faceUvs = block === 13 ? coalOreFaceUvs() : proceduralFaceUvs();
  const faces = [
    { offset: [0, 0, -1], face: "north" },
    { offset: [0, 0, 1], face: "south" },
    { offset: [-1, 0, 0], face: "west" },
    { offset: [1, 0, 0], face: "east" },
    { offset: [0, 1, 0], face: "up" },
    { offset: [0, -1, 0], face: "down" },
  ];

  for (const face of faces) {
    const neighbor = sampleVoxel(voxels, x + face.offset[0], y + face.offset[1], z + face.offset[2]);
    if (neighbor === null || isTransparent(neighbor)) {
      const shadow = skylightShadow(voxels, x + face.offset[0], y, z + face.offset[2]);
      const color = shadedFaceColor(shadeColor, face.face, shadow);
      const faceVerticesData = faceVertices(world, face.face, color, faceUvs, block);
      const base = vertices.length / 12;
      vertices.push(...faceVerticesData.flat());
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

function skylightShadow(voxels, x, y, z) {
  if (x < 0 || x >= CHUNK_WIDTH || z < 0 || z >= CHUNK_DEPTH) {
    return 1;
  }

  let light = 1;
  for (let yy = Math.max(y + 1, 0); yy < CHUNK_HEIGHT; yy++) {
    const block = sampleVoxel(voxels, x, yy, z);
    if (block === null) {
      break;
    }

    if (block === 0) {
      continue;
    }
    if (block === 9 || block === 5) {
      light *= 0.96;
    } else if (block === 7) {
      light *= 0.72;
    } else {
      light *= 0.52;
    }

    if (light <= 0.35 || !(block === 7 || block === 9 || block === 5)) {
      break;
    }
  }

  return Math.max(0.35, Math.min(1, light));
}

function shadedFaceColor(base, face, shadow) {
  let directional;
  switch (face) {
    case "up":
      directional = brighten(base, 0.08);
      break;
    case "down":
      directional = darken(base, 0.22);
      break;
    case "north":
    case "south":
      directional = darken(base, 0.08);
      break;
    default:
      directional = darken(base, 0.02);
      break;
  }

  return directional.map((value) => value * shadow);
}

function faceVertices(origin, face, color, uvs, materialId) {
  const [x, y, z] = origin;
  const normal = faceNormal(face);
  const make = (px, py, pz, uv) => [px, py, pz, color[0], color[1], color[2], normal[0], normal[1], normal[2], uv[0], uv[1], materialId];
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

function faceNormal(face) {
  switch (face) {
    case "north":
      return [0, 0, -1];
    case "south":
      return [0, 0, 1];
    case "east":
      return [1, 0, 0];
    case "west":
      return [-1, 0, 0];
    case "up":
      return [0, 1, 0];
    default:
      return [0, -1, 0];
  }
}

function blockBaseColor(block) {
  switch (block) {
    case 1:
      return [0.43, 0.66, 0.29];
    case 2:
      return [0.47, 0.33, 0.22];
    case 3:
      return [0.58, 0.58, 0.6];
    case 4:
      return [0.82, 0.76, 0.52];
    case 5:
      return [0.38, 0.58, 0.78];
    case 6:
      return [0.52, 0.38, 0.22];
    case 7:
      return [0.30, 0.54, 0.24];
    case 8:
      return [0.72, 0.56, 0.34];
    case 9:
      return [0.78, 0.88, 0.92];
    case 10:
      return [0.96, 0.78, 0.36];
    case 11:
      return [0.60, 0.42, 0.24];
    case 12:
      return [0.86, 0.72, 0.24];
    case 13:
      return [0.22, 0.22, 0.26];
    case 14:
      return [0.66, 0.48, 0.36];
    case 15:
      return [0.76, 0.66, 0.46];
    default:
      return [1, 1, 1];
  }
}

function proceduralFaceUvs() {
  return [[2, 2], [3, 2], [3, 3], [2, 3]];
}

function coalOreFaceUvs() {
  return atlasFaceUvs(COAL_ORE_ATLAS_TILE[0], COAL_ORE_ATLAS_TILE[1]);
}

function atlasFaceUvs(tileX, tileY) {
  const minU = tileX / ATLAS_TILE_COUNT + ATLAS_UV_EPS;
  const maxU = (tileX + 1) / ATLAS_TILE_COUNT - ATLAS_UV_EPS;
  const minV = tileY / ATLAS_TILE_COUNT + ATLAS_UV_EPS;
  const maxV = (tileY + 1) / ATLAS_TILE_COUNT - ATLAS_UV_EPS;
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

function hash(worldSeed, x, z, salt) {
  let value = wrapU64(worldSeed ^ BigInt(salt));
  value = wrapU64(value ^ wrapMulU64(BigInt(x), 0x9E3779B97F4A7C15n));
  value = rotateLeft64(value, 17n);
  value = wrapU64(value ^ wrapMulU64(BigInt(z), 0xBF58476D1CE4E5B9n));
  value = wrapU64(value ^ (value >> 31n));
  value = wrapMulU64(value, 0x94D049BB133111EBn);
  return wrapU64(value ^ (value >> 30n));
}

function parseWorldSeed(worldSeed) {
  if (typeof worldSeed === "string" && worldSeed.length > 0) {
    return BigInt(worldSeed);
  }
  if (typeof worldSeed === "number" && Number.isFinite(worldSeed)) {
    return BigInt(Math.trunc(worldSeed));
  }
  return DEFAULT_WORLD_SEED;
}

function rotateLeft64(value, bits) {
  return BigInt.asUintN(64, (value << bits) | (value >> (64n - bits)));
}

function wrapU64(value) {
  return BigInt.asUintN(64, value);
}

function wrapMulU64(a, b) {
  return wrapU64(wrapU64(a) * wrapU64(b));
}

if (typeof module !== "undefined") {
  module.exports = {
    biomeAt,
    blockBaseColor,
    buildChunkMesh,
    buildChunkMeshFromVoxels,
    generateChunk,
    heightAt,
    parseWorldSeed,
  };
}
