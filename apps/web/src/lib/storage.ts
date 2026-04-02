import fs from "node:fs/promises";
import path from "node:path";
import { GetObjectCommand, PutObjectCommand, S3Client } from "@aws-sdk/client-s3";
import {
  doSpacesBucket,
  doSpacesCustomDomain,
  doSpacesEndpoint,
  doSpacesKey,
  doSpacesRegion,
  doSpacesSecret,
  worldStorageProvider,
  worldStorageRoot,
} from "@/src/lib/env";

export const spacesClient =
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

export function sanitizeFilename(value: string) {
  const trimmed = value.trim();
  if (!trimmed) {
    return "model.glb";
  }

  return trimmed
    .replace(/[^a-zA-Z0-9._-]/g, "_")
    .replace(/_+/g, "_")
    .slice(0, 120);
}

export function toUrlSafeStorageKey(storageKey: string) {
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

export function resolveWorldStoragePublicUrl(storageKey: string) {
  if (worldStorageProvider !== "spaces") {
    return null;
  }

  const baseUrl = resolveSpacesPublicBaseUrl();
  if (!baseUrl) {
    return null;
  }

  return `${baseUrl}/${toUrlSafeStorageKey(storageKey)}`;
}

export async function writeWorldStorageObject(options: {
  storageKey: string;
  bytes: Buffer;
  contentType: string;
  cacheControl?: string;
}) {
  if (worldStorageProvider === "spaces") {
    if (!spacesClient) {
      throw new Error("DigitalOcean Spaces client is not configured.");
    }

    await spacesClient.send(
      new PutObjectCommand({
        Bucket: doSpacesBucket,
        Key: options.storageKey,
        Body: options.bytes,
        ACL: "public-read",
        ContentType: options.contentType,
        CacheControl: options.cacheControl,
      }),
    );

    return;
  }

  const absolutePath = path.join(worldStorageRoot, options.storageKey);
  await fs.mkdir(path.dirname(absolutePath), { recursive: true });
  await fs.writeFile(absolutePath, options.bytes);
}

export async function readWorldStorageObject(storageKey: string) {
  if (worldStorageProvider === "spaces") {
    if (!spacesClient) {
      throw new Error("DigitalOcean Spaces client is not configured.");
    }

    const response = await spacesClient.send(
      new GetObjectCommand({
        Bucket: doSpacesBucket,
        Key: storageKey,
      }),
    );
    const body = response.Body;
    if (!body) {
      return null;
    }

    const bytes = Buffer.from(await body.transformToByteArray());
    return {
      bytes,
      contentType: response.ContentType ?? "application/octet-stream",
      cacheControl: response.CacheControl ?? null,
    };
  }

  const absolutePath = path.join(worldStorageRoot, storageKey);
  const bytes = await fs.readFile(absolutePath).catch((error: NodeJS.ErrnoException) => {
    if (error.code === "ENOENT") {
      return null;
    }
    throw error;
  });
  if (!bytes) {
    return null;
  }

  return {
    bytes,
    contentType: "application/octet-stream",
    cacheControl: null,
  };
}
