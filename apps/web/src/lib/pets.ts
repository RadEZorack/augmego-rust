import { createHash, randomInt } from "node:crypto";
import path from "node:path";
import { prisma } from "@/src/lib/prisma";
import {
  generatedPetCacheControl,
  meshyApiBaseUrl,
  meshyApiKey,
  meshyTextTo3dEnablePbr,
  meshyTextTo3dEnableRefine,
  meshyTextTo3dModel,
  meshyTextTo3dRefineModel,
  meshyTextTo3dTargetPolycount,
  meshyTextTo3dTopology,
  petGenerationMaxAttempts,
  petGenerationPollIntervalMs,
  petGenerationWorkerIntervalMs,
  petPoolTarget,
  worldStorageNamespace,
} from "@/src/lib/env";
import {
  readWorldStorageObject,
  resolveWorldStoragePublicUrl,
  sanitizeFilename,
  writeWorldStorageObject,
} from "@/src/lib/storage";

const PET_BASE_PROMPT = "a cute dog";
const PetStatus = {
  QUEUED: "QUEUED",
  GENERATING: "GENERATING",
  READY: "READY",
  SPAWNED: "SPAWNED",
  CAPTURED: "CAPTURED",
  FAILED: "FAILED",
} as const;
const ACTIVE_PET_STATUSES = [
  PetStatus.QUEUED,
  PetStatus.GENERATING,
  PetStatus.READY,
  PetStatus.SPAWNED,
] as const;
const PET_FILE_CONTENT_TYPE = "model/gltf-binary";
const PET_ACTIVE_FOLLOWER_LIMIT = 6;
const PET_GENERATION_START_BUDGET = 3;
const PET_GENERATION_POLL_BUDGET = 4;

const SIZE_TRAITS = [
  { key: "tiny", label: "Tiny", prompt: "tiny-sized" },
  { key: "small", label: "Small", prompt: "small-sized" },
  { key: "sturdy", label: "Sturdy", prompt: "sturdy build" },
  { key: "lean", label: "Lean", prompt: "lean athletic build" },
  { key: "puffy", label: "Puffy", prompt: "slightly puffy proportions" },
] as const;
const COAT_TRAITS = [
  { key: "fluffy", label: "Fluffy", prompt: "fluffy fur" },
  { key: "curly", label: "Curly", prompt: "soft curly fur" },
  { key: "smooth", label: "Smooth", prompt: "smooth short fur" },
  { key: "shaggy", label: "Shaggy", prompt: "shaggy layered fur" },
  { key: "silky", label: "Silky", prompt: "silky fur" },
] as const;
const COLOR_TRAITS = [
  { key: "golden", label: "Golden", prompt: "golden fur accents" },
  { key: "cream", label: "Cream", prompt: "cream fur" },
  { key: "cocoa", label: "Cocoa", prompt: "warm cocoa-brown fur" },
  { key: "snow", label: "Snowy", prompt: "snow-white fur" },
  { key: "speckled", label: "Speckled", prompt: "speckled fur markings" },
] as const;
const BREED_TRAITS = [
  { key: "beagle", label: "Beagle", prompt: "beagle-inspired face" },
  { key: "corgi", label: "Corgi", prompt: "corgi-inspired proportions" },
  { key: "pomeranian", label: "Pomeranian", prompt: "pomeranian-inspired fluff" },
  { key: "spaniel", label: "Spaniel", prompt: "spaniel-inspired ears" },
  { key: "terrier", label: "Terrier", prompt: "terrier-inspired muzzle" },
] as const;
const ACCESSORY_TRAITS = [
  { key: "bandana", label: "Bandana", prompt: "wearing a tiny bandana" },
  { key: "bow", label: "Bow", prompt: "wearing a small bow collar" },
  { key: "scarf", label: "Scarf", prompt: "wearing a cozy scarf" },
  { key: "tag", label: "Tag", prompt: "wearing a round name tag collar" },
  { key: "none", label: "Classic", prompt: "simple collar-free look" },
] as const;

type PetVariation = {
  variationKey: string;
  displayName: string;
  effectivePrompt: string;
};

type MeshyCreateTaskResponse = {
  result?: string;
  task_id?: string;
  id?: string;
};

type MeshyTextTo3dTaskResponse = {
  status?: string;
  progress?: number;
  prompt?: string;
  model_urls?: Record<string, string | null | undefined>;
  result?: Record<string, string | null | undefined>;
  glb_url?: string;
  preview_glb_url?: string;
};

export type PetIdentity = {
  id: string;
  displayName: string;
  modelUrl: string | null;
};

export type PetCollectionEntry = PetIdentity & {
  capturedAtMs: number | null;
  active: boolean;
};

export type PetCollectionSnapshot = {
  pets: PetCollectionEntry[];
  activePets: PetIdentity[];
};

type CapturePetResult =
  | {
      code: "CAPTURED";
      collection: PetCollectionSnapshot;
    }
  | {
      code: "NOT_FOUND" | "ALREADY_TAKEN" | "NOT_SPAWNED";
      collection?: undefined;
    };

type PetDelegate = {
  findMany<T = unknown>(args: unknown): Promise<T[]>;
  findFirst<T = unknown>(args: unknown): Promise<T | null>;
  findUnique<T = unknown>(args: unknown): Promise<T | null>;
  update<T = unknown>(args: unknown): Promise<T>;
  updateMany(args: unknown): Promise<{ count: number }>;
  create<T = unknown>(args: unknown): Promise<T>;
  count(args: unknown): Promise<number>;
};

const petModel = (prisma as typeof prisma & { pet: PetDelegate }).pet;

function buildVariationKey(indices: number[]) {
  return indices.join("-");
}

function decodeVariationKey(variationKey: string) {
  const [sizeIndex, coatIndex, colorIndex, breedIndex, accessoryIndex] = variationKey
    .split("-")
    .map((value) => Number.parseInt(value, 10));
  const size = SIZE_TRAITS[sizeIndex];
  const coat = COAT_TRAITS[coatIndex];
  const color = COLOR_TRAITS[colorIndex];
  const breed = BREED_TRAITS[breedIndex];
  const accessory = ACCESSORY_TRAITS[accessoryIndex];
  if (!size || !coat || !color || !breed || !accessory) {
    return null;
  }

  return { size, coat, color, breed, accessory };
}

function buildVariationFromKey(variationKey: string): PetVariation | null {
  const traits = decodeVariationKey(variationKey);
  if (!traits) {
    return null;
  }

  const displayName = [
    traits.size.label,
    traits.coat.label,
    traits.color.label,
    traits.breed.label,
  ].join(" ");
  const promptParts = [
    PET_BASE_PROMPT,
    "adorable stylized 3d game-ready animal",
    traits.size.prompt,
    traits.coat.prompt,
    traits.color.prompt,
    traits.breed.prompt,
    traits.accessory.prompt,
    "single centered character",
    "full body",
    "clean silhouette",
    "cute expressive face",
    "unique from other generated dogs",
  ];

  return {
    variationKey,
    displayName,
    effectivePrompt: promptParts.join(", "),
  };
}

function randomVariation(): PetVariation {
  const indices = [
    randomInt(SIZE_TRAITS.length),
    randomInt(COAT_TRAITS.length),
    randomInt(COLOR_TRAITS.length),
    randomInt(BREED_TRAITS.length),
    randomInt(ACCESSORY_TRAITS.length),
  ];
  const variationKey = buildVariationKey(indices);
  const variation = buildVariationFromKey(variationKey);
  if (!variation) {
    throw new Error("Failed to build pet variation.");
  }
  return variation;
}

function resolvePetModelFileUrl(petId: string, storageKey: string | null) {
  if (storageKey) {
    const publicUrl = resolveWorldStoragePublicUrl(storageKey);
    if (publicUrl) {
      return publicUrl;
    }
  }

  return `/api/v1/pets/${petId}/file`;
}

function mapPetIdentity(record: {
  id: string;
  displayName: string;
  modelUrl: string | null;
  modelStorageKey: string | null;
}) {
  return {
    id: record.id,
    displayName: record.displayName,
    modelUrl: record.modelUrl ?? resolvePetModelFileUrl(record.id, record.modelStorageKey),
  } satisfies PetIdentity;
}

function mapCollectionEntry(
  record: {
    id: string;
    displayName: string;
    modelUrl: string | null;
    modelStorageKey: string | null;
    capturedAt: Date | null;
  },
  activeIds: Set<string>,
) {
  const identity = mapPetIdentity(record);
  return {
    ...identity,
    capturedAtMs: record.capturedAt ? record.capturedAt.getTime() : null,
    active: activeIds.has(record.id),
  } satisfies PetCollectionEntry;
}

export async function loadUserPetCollection(userId: string): Promise<PetCollectionSnapshot> {
  const pets = await petModel.findMany<{
    id: string;
    displayName: string;
    modelUrl: string | null;
    modelStorageKey: string | null;
    capturedAt: Date | null;
  }>({
    where: {
      capturedById: userId,
      status: PetStatus.CAPTURED,
    },
    orderBy: [
      { capturedAt: "desc" },
      { createdAt: "desc" },
    ],
    select: {
      id: true,
      displayName: true,
      modelUrl: true,
      modelStorageKey: true,
      capturedAt: true,
    },
  });
  const activeIds = new Set(pets.slice(0, PET_ACTIVE_FOLLOWER_LIMIT).map((pet) => pet.id));
  const collectionPets = pets.map((pet) => mapCollectionEntry(pet, activeIds));
  return {
    pets: collectionPets,
    activePets: collectionPets
      .filter((pet) => pet.active)
      .map((pet) => ({
        id: pet.id,
        displayName: pet.displayName,
        modelUrl: pet.modelUrl,
      })),
  };
}

export async function reserveNextReadyPet() {
  for (let attempt = 0; attempt < 6; attempt += 1) {
    const pet = await petModel.findFirst<{
      id: string;
      displayName: string;
      modelUrl: string | null;
      modelStorageKey: string | null;
    }>({
      where: {
        status: PetStatus.READY,
        modelStorageKey: { not: null },
      },
      orderBy: [{ updatedAt: "asc" }, { createdAt: "asc" }],
      select: {
        id: true,
        displayName: true,
        modelUrl: true,
        modelStorageKey: true,
      },
    });
    if (!pet) {
      return null;
    }

    const update = await petModel.updateMany({
      where: {
        id: pet.id,
        status: PetStatus.READY,
      },
      data: {
        status: PetStatus.SPAWNED,
        spawnedAt: new Date(),
      },
    });
    if (update.count > 0) {
      return mapPetIdentity(pet);
    }
  }

  return null;
}

export async function capturePetForUser(petId: string, userId: string): Promise<CapturePetResult> {
  const pet = await petModel.findUnique<{
    id: string;
    status: string;
  }>({
    where: { id: petId },
    select: {
      id: true,
      status: true,
    },
  });
  if (!pet) {
    return { code: "NOT_FOUND" };
  }
  if (pet.status === PetStatus.CAPTURED) {
    return { code: "ALREADY_TAKEN" };
  }
  if (pet.status !== PetStatus.SPAWNED) {
    return { code: "NOT_SPAWNED" };
  }

  const update = await petModel.updateMany({
    where: {
      id: pet.id,
      status: PetStatus.SPAWNED,
    },
    data: {
      status: PetStatus.CAPTURED,
      capturedById: userId,
      capturedAt: new Date(),
      spawnedAt: null,
    },
  });
  if (update.count === 0) {
    return { code: "ALREADY_TAKEN" };
  }

  return {
    code: "CAPTURED",
    collection: await loadUserPetCollection(userId),
  };
}

export async function resetSpawnedPets() {
  const result = await petModel.updateMany({
    where: { status: PetStatus.SPAWNED },
    data: {
      status: PetStatus.READY,
      spawnedAt: null,
    },
  });
  return result.count;
}

export async function readPetModelFile(petId: string) {
  const pet = await petModel.findUnique<{
    modelStorageKey: string | null;
  }>({
    where: { id: petId },
    select: {
      modelStorageKey: true,
    },
  });
  if (!pet?.modelStorageKey) {
    return null;
  }

  const publicUrl = resolveWorldStoragePublicUrl(pet.modelStorageKey);
  if (publicUrl) {
    return { redirectUrl: publicUrl } as const;
  }

  const bytes = await readWorldStorageObject(pet.modelStorageKey);
  if (!bytes) {
    return null;
  }

  return {
    bytes: bytes.bytes,
    contentType: bytes.contentType || PET_FILE_CONTENT_TYPE,
    cacheControl: bytes.cacheControl ?? generatedPetCacheControl,
  } as const;
}

async function createQueuedPetRecord() {
  for (let attempt = 0; attempt < 64; attempt += 1) {
    const variation = randomVariation();
    try {
      await petModel.create({
        data: {
          displayName: variation.displayName,
          basePrompt: PET_BASE_PROMPT,
          effectivePrompt: variation.effectivePrompt,
          variationKey: variation.variationKey,
          status: PetStatus.QUEUED,
        },
      });
      return true;
    } catch (error) {
      const message = error instanceof Error ? error.message : String(error);
      if (!message.includes("variationKey")) {
        throw error;
      }
    }
  }

  return false;
}

async function ensurePetReservoir() {
  const activeCount = await petModel.count({
    where: {
      status: {
        in: [...ACTIVE_PET_STATUSES],
      },
    },
  });
  const missingCount = Math.max(0, petPoolTarget - activeCount);
  for (let index = 0; index < missingCount; index += 1) {
    const created = await createQueuedPetRecord();
    if (!created) {
      break;
    }
  }
}

function buildMeshyRequestBody(prompt: string) {
  const requestBody: Record<string, unknown> = {
    mode: "preview",
    prompt,
    should_remesh: true,
  };

  if (meshyTextTo3dModel) {
    requestBody.ai_model = meshyTextTo3dModel;
  }
  if (meshyTextTo3dTargetPolycount !== null) {
    requestBody.target_polycount = meshyTextTo3dTargetPolycount;
  }
  if (meshyTextTo3dTopology === "triangle" || meshyTextTo3dTopology === "quad") {
    requestBody.topology = meshyTextTo3dTopology;
  }

  return requestBody;
}

async function createMeshyTextTo3dPreviewTask(prompt: string) {
  const response = await fetch(`${meshyApiBaseUrl}/openapi/v2/text-to-3d`, {
    method: "POST",
    headers: {
      Authorization: `Bearer ${meshyApiKey}`,
      "Content-Type": "application/json",
    },
    body: JSON.stringify(buildMeshyRequestBody(prompt)),
  });
  if (!response.ok) {
    throw new Error(`Meshy create failed (${response.status}): ${await response.text()}`);
  }

  const payload = (await response.json()) as MeshyCreateTaskResponse;
  const taskId = payload.result ?? payload.task_id ?? payload.id;
  if (!taskId) {
    throw new Error("Meshy create response missing task id");
  }

  return taskId;
}

async function createMeshyTextTo3dRefineTask(previewTaskId: string) {
  const requestBody: Record<string, unknown> = {
    mode: "refine",
    preview_task_id: previewTaskId,
  };
  if (meshyTextTo3dRefineModel) {
    requestBody.ai_model = meshyTextTo3dRefineModel;
  }
  if (meshyTextTo3dEnablePbr) {
    requestBody.enable_pbr = true;
  }

  const response = await fetch(`${meshyApiBaseUrl}/openapi/v2/text-to-3d`, {
    method: "POST",
    headers: {
      Authorization: `Bearer ${meshyApiKey}`,
      "Content-Type": "application/json",
    },
    body: JSON.stringify(requestBody),
  });
  if (!response.ok) {
    throw new Error(`Meshy refine failed (${response.status}): ${await response.text()}`);
  }

  const payload = (await response.json()) as MeshyCreateTaskResponse;
  const taskId = payload.result ?? payload.task_id ?? payload.id;
  if (!taskId) {
    throw new Error("Meshy refine response missing task id");
  }

  return taskId;
}

async function fetchMeshyTextTo3dTask(taskId: string) {
  const response = await fetch(
    `${meshyApiBaseUrl}/openapi/v2/text-to-3d/${encodeURIComponent(taskId)}`,
    {
      headers: {
        Authorization: `Bearer ${meshyApiKey}`,
      },
    },
  );
  if (!response.ok) {
    throw new Error(`Meshy status failed (${response.status}): ${await response.text()}`);
  }

  return (await response.json()) as MeshyTextTo3dTaskResponse;
}

function resolveMeshyStatus(task: MeshyTextTo3dTaskResponse) {
  return typeof task.status === "string" ? task.status.trim() : "";
}

function isMeshyTerminalStatus(status: string) {
  const normalized = status.toUpperCase();
  return normalized === "SUCCEEDED" || normalized === "FAILED" || normalized === "CANCELED";
}

function isMeshySuccessStatus(status: string) {
  return status.toUpperCase() === "SUCCEEDED";
}

function extractMeshyGlbUrl(task: MeshyTextTo3dTaskResponse) {
  const modelUrls = task.model_urls ?? {};
  const result = task.result ?? {};
  const candidates = [
    modelUrls.glb,
    modelUrls.preview_glb,
    result.glb_url,
    task.glb_url,
    task.preview_glb_url,
  ];
  return (
    candidates.find(
      (candidate): candidate is string =>
        typeof candidate === "string" && candidate.trim().length > 0,
    ) ?? null
  );
}

async function downloadGeneratedGlb(glbUrl: string) {
  const response = await fetch(glbUrl);
  if (!response.ok) {
    throw new Error(
      `Failed to download generated GLB (${response.status} ${response.statusText})`,
    );
  }

  return Buffer.from(await response.arrayBuffer());
}

function resolvePetStorageKey(petId: string, displayName: string) {
  const safeName = sanitizeFilename(displayName || `pet_${petId}`);
  return path
    .join(worldStorageNamespace, "pets", petId, safeName.toLowerCase().endsWith(".glb") ? safeName : `${safeName}.glb`)
    .replace(/\\/g, "/");
}

async function handleGenerationFailure(petId: string, attempts: number, failureReason: string) {
  if (attempts >= petGenerationMaxAttempts) {
    await petModel.update({
      where: { id: petId },
      data: {
        status: PetStatus.FAILED,
        failureReason,
      },
    });
    return;
  }

  await petModel.update({
    where: { id: petId },
    data: {
      status: PetStatus.QUEUED,
      meshyTaskId: null,
      meshyStatus: null,
      failureReason,
      spawnedAt: null,
    },
  });
}

async function startQueuedPetGeneration() {
  if (!meshyApiKey) {
    return;
  }

  const queuedPets = await petModel.findMany<{
    id: string;
    effectivePrompt: string;
    attempts: number;
  }>({
    where: { status: PetStatus.QUEUED },
    orderBy: [{ createdAt: "asc" }],
    take: PET_GENERATION_START_BUDGET,
    select: {
      id: true,
      effectivePrompt: true,
      attempts: true,
    },
  });

  for (const queuedPet of queuedPets) {
    const claimed = await petModel.updateMany({
      where: {
        id: queuedPet.id,
        status: PetStatus.QUEUED,
      },
      data: {
        status: PetStatus.GENERATING,
        meshyStatus: "PREVIEW_SUBMITTING",
        failureReason: null,
        attempts: {
          increment: 1,
        },
      },
    });
    if (claimed.count === 0) {
      continue;
    }

    const claimedPet = await petModel.findUnique<{
      id: string;
      effectivePrompt: string;
      attempts: number;
    }>({
      where: { id: queuedPet.id },
      select: {
        id: true,
        effectivePrompt: true,
        attempts: true,
      },
    });
    if (!claimedPet) {
      continue;
    }

    try {
      const taskId = await createMeshyTextTo3dPreviewTask(claimedPet.effectivePrompt);
      await petModel.update({
        where: { id: claimedPet.id },
        data: {
          meshyTaskId: taskId,
          meshyStatus: "PREVIEW_SUBMITTED",
        },
      });
    } catch (error) {
      await handleGenerationFailure(
        claimedPet.id,
        claimedPet.attempts,
        error instanceof Error ? error.message : String(error),
      );
    }
  }
}

async function pollGeneratingPets() {
  if (!meshyApiKey) {
    return;
  }

  const cutoff = new Date(Date.now() - petGenerationPollIntervalMs);
  const pets = await petModel.findMany<{
    id: string;
    displayName: string;
    meshyTaskId: string | null;
    attempts: number;
    meshyStatus: string | null;
  }>({
    where: {
      status: PetStatus.GENERATING,
      meshyTaskId: { not: null },
      updatedAt: { lte: cutoff },
    },
    orderBy: [{ updatedAt: "asc" }],
    take: PET_GENERATION_POLL_BUDGET,
    select: {
      id: true,
      displayName: true,
      meshyTaskId: true,
      attempts: true,
      meshyStatus: true,
    },
  });

  for (const pet of pets) {
    const actualTaskId = pet.meshyTaskId?.startsWith("refine:")
      ? pet.meshyTaskId.slice("refine:".length)
      : pet.meshyTaskId;
    if (!actualTaskId) {
      continue;
    }

    try {
      const task = await fetchMeshyTextTo3dTask(actualTaskId);
      const meshyStatus = resolveMeshyStatus(task);
      if (!meshyStatus) {
        throw new Error("Meshy task returned empty status");
      }
      if (!isMeshyTerminalStatus(meshyStatus)) {
        await petModel.update({
          where: { id: pet.id },
          data: {
            meshyStatus,
          },
        });
        continue;
      }
      if (!isMeshySuccessStatus(meshyStatus)) {
        await handleGenerationFailure(
          pet.id,
          pet.attempts,
          `Meshy generation ended with status ${meshyStatus}`,
        );
        continue;
      }
      if (meshyTextTo3dEnableRefine && !pet.meshyTaskId?.startsWith("refine:")) {
        const refineTaskId = await createMeshyTextTo3dRefineTask(actualTaskId);
        await petModel.update({
          where: { id: pet.id },
          data: {
            meshyTaskId: `refine:${refineTaskId}`,
            meshyStatus: "REFINE_SUBMITTED",
          },
        });
        continue;
      }

      const glbUrl = extractMeshyGlbUrl(task);
      if (!glbUrl) {
        await handleGenerationFailure(
          pet.id,
          pet.attempts,
          "Meshy generation succeeded but no GLB URL was returned",
        );
        continue;
      }

      const bytes = await downloadGeneratedGlb(glbUrl);
      const modelSha256 = createHash("sha256").update(bytes).digest("hex");
      const duplicate = await petModel.findFirst<{
        id: string;
      }>({
        where: {
          modelSha256,
          id: { not: pet.id },
        },
        select: { id: true },
      });
      if (duplicate) {
        await petModel.update({
          where: { id: pet.id },
          data: {
            status: PetStatus.FAILED,
            failureReason: `Duplicate generated mesh matched pet ${duplicate.id}`,
          },
        });
        continue;
      }

      const storageKey = resolvePetStorageKey(pet.id, pet.displayName);
      await writeWorldStorageObject({
        storageKey,
        bytes,
        contentType: PET_FILE_CONTENT_TYPE,
        cacheControl: generatedPetCacheControl,
      });
      await petModel.update({
        where: { id: pet.id },
        data: {
          status: PetStatus.READY,
          meshyStatus,
          modelStorageKey: storageKey,
          modelUrl: resolvePetModelFileUrl(pet.id, storageKey),
          modelSha256,
          failureReason: null,
          spawnedAt: null,
        },
      });
    } catch (error) {
      await handleGenerationFailure(
        pet.id,
        pet.attempts,
        error instanceof Error ? error.message : String(error),
      );
    }
  }
}

let petGenerationWorkerBusy = false;

async function runPetGenerationWorkerTick() {
  if (!meshyApiKey) {
    return;
  }
  if (petGenerationWorkerBusy) {
    return;
  }

  petGenerationWorkerBusy = true;
  try {
    await ensurePetReservoir();
    await startQueuedPetGeneration();
    await pollGeneratingPets();
  } finally {
    petGenerationWorkerBusy = false;
  }
}

type PetWorkerGlobal = typeof globalThis & {
  __augmegoPetWorkerStarted?: boolean;
};

export function startPetGenerationWorker() {
  const globalWorker = globalThis as PetWorkerGlobal;
  if (globalWorker.__augmegoPetWorkerStarted) {
    return;
  }

  globalWorker.__augmegoPetWorkerStarted = true;
  void runPetGenerationWorkerTick();
  setInterval(() => {
    void runPetGenerationWorkerTick();
  }, petGenerationWorkerIntervalMs);
}
