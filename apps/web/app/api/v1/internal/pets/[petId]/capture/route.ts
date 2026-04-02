import { NextResponse } from "next/server";
import { ensureInternalServiceRequest } from "@/src/lib/internal-api";
import { capturePetForUser } from "@/src/lib/pets";

export async function POST(
  request: Request,
  context: { params: Promise<{ petId: string }> },
) {
  const rejection = ensureInternalServiceRequest(request);
  if (rejection) {
    return rejection;
  }

  const { petId } = await context.params;
  const body = (await request.json().catch(() => null)) as { userId?: string } | null;
  const userId = typeof body?.userId === "string" ? body.userId : "";
  if (!userId) {
    return NextResponse.json({ error: "USER_ID_REQUIRED" }, { status: 400 });
  }

  const result = await capturePetForUser(petId, userId);
  return NextResponse.json(result);
}
