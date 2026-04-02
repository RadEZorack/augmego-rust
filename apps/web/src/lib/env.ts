import path from "node:path";
import jwt from "jsonwebtoken";

type SameSite = "lax" | "strict" | "none";
type StorageProvider = "local" | "spaces";

function toPositiveInteger(value: string | undefined, fallback: number) {
  const parsed = Number(value);
  if (!Number.isFinite(parsed) || parsed <= 0) {
    return fallback;
  }

  return Math.floor(parsed);
}

function normalizeSameSite(value: string | undefined, fallback: SameSite): SameSite {
  const normalized = value?.trim().toLowerCase();
  if (normalized === "lax" || normalized === "strict" || normalized === "none") {
    return normalized;
  }

  return fallback;
}

function normalizeBaseUrl(value: string) {
  return value.replace(/\/+$/, "");
}

const repoRoot = path.resolve(process.cwd(), "../..");

export const webBaseUrl = normalizeBaseUrl(
  process.env.WEB_BASE_URL ?? process.env.NEXTAUTH_URL ?? "http://localhost:3000",
);
process.env.AUTH_URL ??= webBaseUrl;
process.env.NEXTAUTH_URL ??= webBaseUrl;
export const webOrigin = (() => {
  try {
    return new URL(webBaseUrl).origin;
  } catch {
    return "http://localhost:3000";
  }
})();
export const authSecret = process.env.AUTH_SECRET ?? "dev-only-auth-secret";
export const sessionCookieName = process.env.SESSION_COOKIE_NAME ?? "session_id";
export const sessionTtlSeconds = toPositiveInteger(process.env.SESSION_TTL_HOURS, 168) * 60 * 60;
export const sessionSameSite = normalizeSameSite(
  process.env.COOKIE_SAMESITE,
  webOrigin.startsWith("https://") ? "none" : "lax",
);
export const sessionSecure =
  process.env.COOKIE_SECURE === "true" || webOrigin.startsWith("https://");

export const googleClientId = process.env.GOOGLE_CLIENT_ID ?? "";
export const googleClientSecret = process.env.GOOGLE_CLIENT_SECRET ?? "";
export const googleScope = process.env.GOOGLE_SCOPE ?? "openid email profile";

export const linkedinClientId = process.env.LINKEDIN_CLIENT_ID ?? "";
export const linkedinClientSecret = process.env.LINKEDIN_CLIENT_SECRET ?? "";
export const linkedinScope = process.env.LINKEDIN_SCOPE ?? "r_liteprofile r_emailaddress";

export const appleClientId = process.env.APPLE_CLIENT_ID ?? "";
export const appleClientSecret = process.env.APPLE_CLIENT_SECRET ?? "";
export const appleTeamId = process.env.APPLE_TEAM_ID ?? "";
export const appleKeyId = process.env.APPLE_KEY_ID ?? "";
export const applePrivateKey = process.env.APPLE_PRIVATE_KEY ?? "";
export const appleScope = process.env.APPLE_SCOPE ?? "name email";

const rawStorageProvider = (process.env.WORLD_STORAGE_PROVIDER ?? "").trim().toLowerCase();
export const doSpacesKey = process.env.DO_SPACES_KEY ?? "";
export const doSpacesSecret = process.env.DO_SPACES_SECRET ?? "";
export const doSpacesBucket = process.env.DO_SPACES_BUCKET ?? "";
export const doSpacesRegion = process.env.DO_SPACES_REGION ?? "";
export const doSpacesEndpoint = process.env.DO_SPACES_ENDPOINT ?? "";
export const doSpacesCustomDomain = process.env.DO_SPACES_CUSTOM_DOMAIN ?? "";
const doSpacesConfigured = Boolean(
  doSpacesKey &&
    doSpacesSecret &&
    doSpacesBucket &&
    doSpacesRegion &&
    doSpacesEndpoint,
);

export const worldStorageProvider: StorageProvider =
  rawStorageProvider === "spaces" && doSpacesConfigured ? "spaces" : "local";
export const worldStorageNamespace =
  process.env.WORLD_STORAGE_NAMESPACE ?? (process.env.NODE_ENV === "production" ? "prod" : "dev");
export const worldStorageRoot = process.env.WORLD_STORAGE_ROOT
  ? path.resolve(process.cwd(), process.env.WORLD_STORAGE_ROOT)
  : path.resolve(repoRoot, "storage", "world-assets");
export const playerAvatarCacheControl =
  process.env.PLAYER_AVATAR_CACHE_CONTROL ?? "public, max-age=31536000, immutable";
export const generatedPetCacheControl =
  process.env.GENERATED_PET_CACHE_CONTROL ?? "public, max-age=31536000, immutable";
export const meshyApiBaseUrl = normalizeBaseUrl(
  process.env.MESHY_API_BASE_URL ?? "https://api.meshy.ai",
);
export const meshyApiKey = process.env.MESHY_API_KEY ?? "";
export const meshyTextTo3dModel = process.env.MESHY_TEXT_TO_3D_MODEL ?? "";
export const meshyTextTo3dEnableRefine =
  (process.env.MESHY_TEXT_TO_3D_ENABLE_REFINE ?? "true").toLowerCase() !== "false";
export const meshyTextTo3dRefineModel = process.env.MESHY_TEXT_TO_3D_REFINE_MODEL ?? "";
export const meshyTextTo3dEnablePbr =
  (process.env.MESHY_TEXT_TO_3D_ENABLE_PBR ?? "false").toLowerCase() === "true";
export const meshyTextTo3dTopology = (
  process.env.MESHY_TEXT_TO_3D_TOPOLOGY ?? "triangle"
).toLowerCase();
export const meshyTextTo3dTargetPolycount = (() => {
  const value = Number(process.env.MESHY_TEXT_TO_3D_TARGET_POLYCOUNT ?? "");
  if (!Number.isFinite(value)) {
    return null;
  }

  const normalized = Math.floor(value);
  if (normalized < 100 || normalized > 300000) {
    return null;
  }

  return normalized;
})();
export const petPoolTarget = toPositiveInteger(process.env.PET_POOL_TARGET, 30);
export const petGenerationWorkerIntervalMs = toPositiveInteger(
  process.env.PET_GENERATION_WORKER_INTERVAL_MS,
  5000,
);
export const petGenerationPollIntervalMs = toPositiveInteger(
  process.env.PET_GENERATION_POLL_INTERVAL_MS,
  15000,
);
export const petGenerationMaxAttempts = toPositiveInteger(
  process.env.PET_GENERATION_MAX_ATTEMPTS,
  5,
);
export const gameBackendAuthSecret =
  process.env.GAME_BACKEND_AUTH_SECRET ?? "dev-only-game-backend-secret";
export const backendServiceToken =
  process.env.BACKEND_SERVICE_TOKEN ?? "dev-only-backend-service-token";

export function resolveAppleClientSecret() {
  if (appleClientSecret) {
    return appleClientSecret;
  }

  if (!appleTeamId || !appleKeyId || !applePrivateKey || !appleClientId) {
    return "";
  }

  const now = Math.floor(Date.now() / 1000);
  return jwt.sign(
    {
      iss: appleTeamId,
      iat: now,
      exp: now + 60 * 60 * 24 * 180,
      aud: "https://appleid.apple.com",
      sub: appleClientId,
    },
    applePrivateKey.replace(/\\n/g, "\n"),
    {
      algorithm: "ES256",
      keyid: appleKeyId,
    },
  );
}
