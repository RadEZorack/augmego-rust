import { randomUUID } from "node:crypto";
import path from "node:path";
import { PutObjectCommand } from "@aws-sdk/client-s3";
import { getSignedUrl } from "@aws-sdk/s3-request-presigner";
import { prisma } from "@/src/lib/prisma";
import {
  doSpacesBucket,
  playerAvatarCacheControl,
  worldStorageNamespace,
  worldStorageProvider,
} from "@/src/lib/env";
import {
  readWorldStorageObject,
  resolveWorldStoragePublicUrl,
  sanitizeFilename,
  spacesClient,
  writeWorldStorageObject,
} from "@/src/lib/storage";

export type PlayerAvatarSlot = "idle" | "run" | "dance";

export type AvatarSelection = {
  stationaryModelUrl: string | null;
  moveModelUrl: string | null;
  specialModelUrl: string | null;
};

export type AvatarFileResponse =
  | {
      redirectUrl: string;
    }
  | {
      bytes: Buffer;
      contentType: string;
      cacheControl: string;
    };

const DIRECT_UPLOAD_TTL_SECONDS = 60 * 15;

export function isPlayerAvatarSlot(value: unknown): value is PlayerAvatarSlot {
  return value === "idle" || value === "run" || value === "dance";
}

export function isValidGlbUpload(file: File) {
  return file.name.toLowerCase().endsWith(".glb");
}

export function normalizeAvatarUrl(value: unknown) {
  if (typeof value !== "string") {
    return null;
  }

  const trimmed = value.trim().slice(0, 2000);
  if (!trimmed) {
    return null;
  }

  try {
    const parsed = new URL(trimmed);
    if (parsed.protocol !== "http:" && parsed.protocol !== "https:") {
      return null;
    }
    return parsed.toString();
  } catch {
    return null;
  }
}

export async function loadUserAvatarSelection(userId: string): Promise<AvatarSelection> {
  const user = await prisma.user.findUnique({
    where: { id: userId },
    select: {
      playerAvatarStationaryModelUrl: true,
      playerAvatarMoveModelUrl: true,
      playerAvatarSpecialModelUrl: true,
    },
  });

  return {
    stationaryModelUrl: user?.playerAvatarStationaryModelUrl ?? null,
    moveModelUrl: user?.playerAvatarMoveModelUrl ?? null,
    specialModelUrl: user?.playerAvatarSpecialModelUrl ?? null,
  };
}

export async function updateUserAvatarSelection(userId: string, selection: AvatarSelection) {
  await prisma.user.update({
    where: { id: userId },
    data: {
      playerAvatarStationaryModelUrl: selection.stationaryModelUrl,
      playerAvatarMoveModelUrl: selection.moveModelUrl,
      playerAvatarSpecialModelUrl: selection.specialModelUrl,
    },
  });
}

function resolvePlayerAvatarStorageKey(userId: string, slot: PlayerAvatarSlot) {
  return path
    .join(worldStorageNamespace, userId, "player-avatars", slot, `${slot}.glb`)
    .replace(/\\/g, "/");
}

export function resolveWorldAssetPublicUrl(storageKey: string) {
  return resolveWorldStoragePublicUrl(storageKey);
}

export function resolvePlayerAvatarFileUrl(userId: string, slot: PlayerAvatarSlot) {
  const storageKey = resolvePlayerAvatarStorageKey(userId, slot);
  return (
    resolveWorldAssetPublicUrl(storageKey) ??
    `/api/v1/users/${userId}/player-avatar/${slot}/file`
  );
}

export async function savePlayerAvatarFile(file: File, userId: string, slot: PlayerAvatarSlot) {
  const storageKey = resolvePlayerAvatarStorageKey(userId, slot);
  await writeWorldStorageObject({
    storageKey,
    bytes: Buffer.from(await file.arrayBuffer()),
    contentType: file.type || "model/gltf-binary",
    cacheControl: playerAvatarCacheControl,
  });

  return {
    storageKey,
    publicUrl: resolvePlayerAvatarFileUrl(userId, slot),
  };
}

export async function createPlayerAvatarUploadUrl(
  userId: string,
  slot: PlayerAvatarSlot,
  fileName: string,
  contentType: string,
) {
  if (worldStorageProvider !== "spaces" || !spacesClient) {
    return null;
  }

  const normalizedName = sanitizeFilename(fileName || `${slot}.glb`);
  const safeName = normalizedName.toLowerCase().endsWith(".glb")
    ? normalizedName
    : `${normalizedName}.glb`;
  const storageKey = path
    .join(
      worldStorageNamespace,
      userId,
      "player-avatars",
      slot,
      `${randomUUID()}_${safeName}`,
    )
    .replace(/\\/g, "/");

  const uploadUrl = await getSignedUrl(
    spacesClient,
    new PutObjectCommand({
      Bucket: doSpacesBucket,
      Key: storageKey,
      ACL: "public-read",
      CacheControl: playerAvatarCacheControl,
      ContentType: contentType || "model/gltf-binary",
    }),
    { expiresIn: DIRECT_UPLOAD_TTL_SECONDS },
  );

  return {
    uploadUrl,
    publicUrl: resolveWorldAssetPublicUrl(storageKey),
    contentType: contentType || "model/gltf-binary",
    uploadHeaders: {
      "x-amz-acl": "public-read",
      "Cache-Control": playerAvatarCacheControl,
    },
  };
}

export async function readPlayerAvatarFile(
  userId: string,
  slot: PlayerAvatarSlot,
): Promise<AvatarFileResponse | null> {
  const storageKey = resolvePlayerAvatarStorageKey(userId, slot);

  if (worldStorageProvider === "spaces") {
    const publicUrl = resolveWorldAssetPublicUrl(storageKey);
    if (publicUrl) {
      return { redirectUrl: publicUrl };
    }

    if (!spacesClient) {
      return null;
    }

    const response = await readWorldStorageObject(storageKey);
    if (!response) {
      return null;
    }

    return {
      bytes: response.bytes,
      contentType: response.contentType || "model/gltf-binary",
      cacheControl: response.cacheControl ?? playerAvatarCacheControl,
    };
  }

  const response = await readWorldStorageObject(storageKey);
  if (!response) {
    return null;
  }

  return {
    bytes: response.bytes,
    contentType: response.contentType || "model/gltf-binary",
    cacheControl: response.cacheControl ?? playerAvatarCacheControl,
  };
}
