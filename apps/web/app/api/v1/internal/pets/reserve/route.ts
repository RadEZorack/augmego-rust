import { NextResponse } from "next/server";
import { ensureInternalServiceRequest } from "@/src/lib/internal-api";
import { reserveNextReadyPet } from "@/src/lib/pets";

export async function POST(request: Request) {
  const rejection = ensureInternalServiceRequest(request);
  if (rejection) {
    return rejection;
  }

  const pet = await reserveNextReadyPet();
  return NextResponse.json({ pet });
}
