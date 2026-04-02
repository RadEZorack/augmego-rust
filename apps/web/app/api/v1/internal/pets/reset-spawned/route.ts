import { NextResponse } from "next/server";
import { ensureInternalServiceRequest } from "@/src/lib/internal-api";
import { resetSpawnedPets } from "@/src/lib/pets";

export async function POST(request: Request) {
  const rejection = ensureInternalServiceRequest(request);
  if (rejection) {
    return rejection;
  }

  const resetCount = await resetSpawnedPets();
  return NextResponse.json({ ok: true, resetCount });
}
