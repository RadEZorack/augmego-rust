const assert = require("assert").strict;

const { biomeAt, generateChunk, heightAt, parseWorldSeed } = require("./mesh-worker.js");

const CHUNK_WIDTH = 32;
const CHUNK_HEIGHT = 256;
const CHUNK_DEPTH = 32;
const SEARCH_RADIUS = 2;

function blockAt(voxels, x, y, z) {
  return voxels[y * CHUNK_WIDTH * CHUNK_DEPTH + z * CHUNK_WIDTH + x];
}

function testGeneratedTerrainDoesNotEmbedOreNodes() {
  const seed = parseWorldSeed(42);

  for (let chunkX = -SEARCH_RADIUS; chunkX <= SEARCH_RADIUS; chunkX += 1) {
    for (let chunkZ = -SEARCH_RADIUS; chunkZ <= SEARCH_RADIUS; chunkZ += 1) {
      const { voxels } = generateChunk(chunkX, chunkZ, seed);
      for (const block of voxels) {
        assert.notEqual(block, 12);
        assert.notEqual(block, 13);
        assert.notEqual(block, 14);
      }
    }
  }
}

function testSandstonePlacementRules() {
  const seed = parseWorldSeed(42);
  let foundSandstone = false;

  for (let chunkX = -SEARCH_RADIUS; chunkX <= SEARCH_RADIUS; chunkX += 1) {
    for (let chunkZ = -SEARCH_RADIUS; chunkZ <= SEARCH_RADIUS; chunkZ += 1) {
      const { voxels } = generateChunk(chunkX, chunkZ, seed);
      const baseX = chunkX * CHUNK_WIDTH;
      const baseZ = chunkZ * CHUNK_DEPTH;

      for (let z = 0; z < CHUNK_DEPTH; z += 1) {
        for (let x = 0; x < CHUNK_WIDTH; x += 1) {
          const worldX = baseX + x;
          const worldZ = baseZ + z;
          const biome = biomeAt(worldX, worldZ, seed);
          const surface = heightAt(worldX, worldZ, biome, seed);

          for (let y = Math.max(surface - 5, 0); y <= Math.min(surface - 2, CHUNK_HEIGHT - 1); y += 1) {
            const block = blockAt(voxels, x, y, z);
            if (block !== 15) {
              continue;
            }

            foundSandstone = true;
            assert.equal(biome, 2);
            assert.ok(y <= surface - 2);
            assert.ok(y >= surface - 5);
          }
        }
      }
    }
  }

  assert.ok(foundSandstone, "expected at least one sandstone block");
}

function main() {
  testGeneratedTerrainDoesNotEmbedOreNodes();
  testSandstonePlacementRules();
  console.log("mesh-worker tests passed");
}

main();
