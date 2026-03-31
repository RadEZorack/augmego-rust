import { randomUUID } from "node:crypto";
import fs from "node:fs/promises";
import path from "node:path";
import { GetObjectCommand, PutObjectCommand, S3Client } from "@aws-sdk/client-s3";
import { getSignedUrl } from "@aws-sdk/s3-request-presigner";
import { prisma } from "@/src/lib/prisma";
import {
  doSpacesBucket,
  doSpacesCustomDomain,
  doSpacesEndpoint,
  doSpacesKey,
  doSpacesRegion,
  doSpacesSecret,
  playerAvatarCacheControl,
  worldStorageNamespace,
  worldStorageProvider,
  worldStorageRoot,
} from "@/src/lib/env";

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

const spacesClient =
  worldStorageProvider === "spaces"
    ? new S3Client({
        region: doSpacesRegion,
        endpoint: doSpacesEndpoint,
        credentials: {
          accessKeyId: doSpacesKey,
          secretAccessKey: doSpacesSecret,
        },
      })
    : null;

function sanitizeFilename(value: string) {
  const trimmed = value.trim();
  if (!trimmed) {
    return "model.glb";
  }

  return trimmed
    .replace(/[^a-zA-Z0-9._-]/g, "_")
    .replace(/_+/g, "_")
    .slice(0, 120);
}

function toUrlSafeStorageKey(storageKey: string) {
  return storageKey
    .split("/")
    .map((segment) => encodeURIComponent(segment))
    .join("/");
}

function resolveSpacesPublicBaseUrl() {
  if (doSpacesCustomDomain) {
    return doSpacesCustomDomain.replace(/\/+$/, "");
  }

  try {
    const endpointUrl = new URL(doSpacesEndpoint);
    return `${endpointUrl.protocol}//${doSpacesBucket}.${endpointUrl.host}`;
  } catch {
    return "";
  }
}

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

function resolvePlayerAvatarAbsolutePath(userId: string, slot: PlayerAvatarSlot) {
  return path.join(worldStorageRoot, resolvePlayerAvatarStorageKey(userId, slot));
}

export function resolveWorldAssetPublicUrl(storageKey: string) {
  if (worldStorageProvider !== "spaces") {
    return null;
  }

  const baseUrl = resolveSpacesPublicBaseUrl();
  if (!baseUrl) {
    return null;
  }

  return `${baseUrl}/${toUrlSafeStorageKey(storageKey)}`;
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

  if (worldStorageProvider === "spaces") {
    if (!spacesClient) {
      throw new Error("DigitalOcean Spaces client is not configured.");
    }

    const bytes = Buffer.from(await file.arrayBuffer());
    await spacesClient.send(
      new PutObjectCommand({
        Bucket: doSpacesBucket,
        Key: storageKey,
        Body: bytes,
        ACL: "public-read",
        CacheControl: playerAvatarCacheControl,
        ContentType: file.type || "model/gltf-binary",
      }),
    );

    return {
      storageKey,
      publicUrl: resolvePlayerAvatarFileUrl(userId, slot),
    };
  }

  const absolutePath = resolvePlayerAvatarAbsolutePath(userId, slot);
  await fs.mkdir(path.dirname(absolutePath), { recursive: true });
  await fs.writeFile(absolutePath, Buffer.from(await file.arrayBuffer()));

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

    const response = await spacesClient.send(
      new GetObjectCommand({
        Bucket: doSpacesBucket,
        Key: storageKey,
      }),
    );

    if (!response.Body) {
      return null;
    }

    const bytes = Buffer.from(await response.Body.transformToByteArray());
    return {
      bytes,
      contentType: response.ContentType ?? "model/gltf-binary",
      cacheControl: response.CacheControl ?? playerAvatarCacheControl,
    };
  }

  try {
    const bytes = await fs.readFile(resolvePlayerAvatarAbsolutePath(userId, slot));
    return {
      bytes,
      contentType: "model/gltf-binary",
      cacheControl: playerAvatarCacheControl,
    };
  } catch (error) {
    if ((error as NodeJS.ErrnoException).code === "ENOENT") {
      return null;
    }
    throw error;
  }
}
