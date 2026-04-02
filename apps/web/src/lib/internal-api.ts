import { NextResponse } from "next/server";
import { backendServiceToken } from "@/src/lib/env";

export const INTERNAL_SERVICE_TOKEN_HEADER = "x-augmego-service-token";

export function ensureInternalServiceRequest(request: Request) {
  const provided = request.headers.get(INTERNAL_SERVICE_TOKEN_HEADER);
  if (!provided || provided !== backendServiceToken) {
    return NextResponse.json({ error: "FORBIDDEN" }, { status: 403 });
  }

  return null;
}
