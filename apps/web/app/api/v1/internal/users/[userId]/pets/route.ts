import { NextResponse } from "next/server";
import { ensureInternalServiceRequest } from "@/src/lib/internal-api";
import { loadUserPetCollection } from "@/src/lib/pets";

export async function GET(
  request: Request,
  context: { params: Promise<{ userId: string }> },
) {
  const rejection = ensureInternalServiceRequest(request);
  if (rejection) {
    return rejection;
  }

  const { userId } = await context.params;
  return NextResponse.json(await loadUserPetCollection(userId));
}
