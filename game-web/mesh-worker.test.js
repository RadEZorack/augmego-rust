const assert = require("assert").strict;

const { biomeAt, generateChunk, heightAt, parseWorldSeed } = require("./mesh-worker.js");

const CHUNK_WIDTH = 32;
const CHUNK_HEIGHT = 256;
const CHUNK_DEPTH = 32;

function blockAt(voxels, x, y, z) {
  return voxels[y * CHUNK_WIDTH * CHUNK_DEPTH + z * CHUNK_WIDTH + x];
}

function testOreGenerationDepthBands() {
  const seed = parseWorldSeed(42);
  let foundCoal = false;
  let foundIron = false;
  let foundGold = false;

  outer: for (let chunkX = -6; chunkX <= 6; chunkX += 1) {
    for (let chunkZ = -6; chunkZ <= 6; chunkZ += 1) {
      const { voxels } = generateChunk(chunkX, chunkZ, seed);
      for (let y = 0; y < CHUNK_HEIGHT; y += 1) {
        for (let z = 0; z < CHUNK_DEPTH; z += 1) {
          for (let x = 0; x < CHUNK_WIDTH; x += 1) {
            const block = blockAt(voxels, x, y, z);
            if (block === 13) {
              foundCoal = true;
              assert.ok(y < 72);
            } else if (block === 14) {
              foundIron = true;
              assert.ok(y < 56);
            } else if (block === 12) {
              foundGold = true;
              assert.ok(y < 32);
            }
          }
        }
      }

      if (foundCoal && foundIron && foundGold) {
        break outer;
      }
    }
  }

  assert.ok(foundCoal, "expected at least one coal ore block");
  assert.ok(foundIron, "expected at least one iron ore block");
  assert.ok(foundGold, "expected at least one gold ore block");
}

function testSandstonePlacementRules() {
  const seed = parseWorldSeed(42);
  let foundSandstone = false;

  for (let chunkX = -6; chunkX <= 6; chunkX += 1) {
    for (let chunkZ = -6; chunkZ <= 6; chunkZ += 1) {
      const { voxels } = generateChunk(chunkX, chunkZ, seed);
      const baseX = chunkX * CHUNK_WIDTH;
      const baseZ = chunkZ * CHUNK_DEPTH;

      for (let z = 0; z < CHUNK_DEPTH; z += 1) {
        for (let x = 0; x < CHUNK_WIDTH; x += 1) {
          const worldX = baseX + x;
          const worldZ = baseZ + z;
          const biome = biomeAt(worldX, worldZ, seed);
          const surface = heightAt(worldX, worldZ, biome, seed);

          for (let y = 0; y <= Math.min(surface, CHUNK_HEIGHT - 1); y += 1) {
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
  testOreGenerationDepthBands();
  testSandstonePlacementRules();
  console.log("mesh-worker tests passed");
}

main();
