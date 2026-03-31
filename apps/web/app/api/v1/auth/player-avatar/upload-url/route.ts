import { NextResponse } from "next/server";
import { auth } from "@/src/auth";
import { createPlayerAvatarUploadUrl, isPlayerAvatarSlot } from "@/src/lib/avatar";

export async function POST(request: Request) {
  const session = await auth();
  const userId = session?.user?.id;
  if (!userId) {
    return NextResponse.json({ error: "AUTH_REQUIRED" }, { status: 401 });
  }

  const body = (await request.json().catch(() => null)) as
    | {
        slot?: unknown;
        fileName?: unknown;
        contentType?: unknown;
      }
    | null;

  const slotValue = String(body?.slot ?? "").trim().toLowerCase();
  if (!isPlayerAvatarSlot(slotValue)) {
    return NextResponse.json({ error: "INVALID_AVATAR_SLOT" }, { status: 400 });
  }

  const upload = await createPlayerAvatarUploadUrl(
    userId,
    slotValue,
    typeof body?.fileName === "string" ? body.fileName : `${slotValue}.glb`,
    typeof body?.contentType === "string" ? body.contentType : "model/gltf-binary",
  );
  if (!upload?.publicUrl) {
    return NextResponse.json({ error: "DIRECT_UPLOAD_NOT_AVAILABLE" }, { status: 503 });
  }

  return NextResponse.json(upload);
}
