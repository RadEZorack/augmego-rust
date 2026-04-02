CREATE TYPE "PetStatus" AS ENUM ('QUEUED', 'GENERATING', 'READY', 'SPAWNED', 'CAPTURED', 'FAILED');

CREATE TABLE "Pet" (
    "id" UUID NOT NULL,
    "displayName" TEXT NOT NULL,
    "basePrompt" TEXT NOT NULL,
    "effectivePrompt" TEXT NOT NULL,
    "variationKey" TEXT NOT NULL,
    "status" "PetStatus" NOT NULL DEFAULT 'QUEUED',
    "meshyTaskId" TEXT,
    "meshyStatus" TEXT,
    "modelStorageKey" TEXT,
    "modelUrl" TEXT,
    "modelSha256" TEXT,
    "attempts" INTEGER NOT NULL DEFAULT 0,
    "failureReason" TEXT,
    "spawnedAt" TIMESTAMP(3),
    "capturedAt" TIMESTAMP(3),
    "capturedById" UUID,
    "createdAt" TIMESTAMP(3) NOT NULL DEFAULT CURRENT_TIMESTAMP,
    "updatedAt" TIMESTAMP(3) NOT NULL,

    CONSTRAINT "Pet_pkey" PRIMARY KEY ("id")
);

CREATE UNIQUE INDEX "Pet_variationKey_key" ON "Pet"("variationKey");
CREATE UNIQUE INDEX "Pet_meshyTaskId_key" ON "Pet"("meshyTaskId");
CREATE UNIQUE INDEX "Pet_modelStorageKey_key" ON "Pet"("modelStorageKey");
CREATE UNIQUE INDEX "Pet_modelSha256_key" ON "Pet"("modelSha256");
CREATE INDEX "Pet_status_updatedAt_idx" ON "Pet"("status", "updatedAt");
CREATE INDEX "Pet_capturedById_capturedAt_idx" ON "Pet"("capturedById", "capturedAt");

ALTER TABLE "Pet" ADD CONSTRAINT "Pet_capturedById_fkey"
FOREIGN KEY ("capturedById") REFERENCES "User"("id") ON DELETE SET NULL ON UPDATE CASCADE;
