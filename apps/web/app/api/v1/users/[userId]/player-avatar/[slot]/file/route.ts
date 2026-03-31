import { NextResponse } from "next/server";
import { isPlayerAvatarSlot, readPlayerAvatarFile } from "@/src/lib/avatar";

type RouteContext = {
  params: Promise<{
    userId: string;
    slot: string;
  }>;
};

export async function GET(_request: Request, context: RouteContext) {
  const { userId, slot } = await context.params;
  if (!isPlayerAvatarSlot(slot)) {
    return NextResponse.json({ error: "INVALID_AVATAR_SLOT" }, { status: 400 });
  }

  const avatarFile = await readPlayerAvatarFile(userId, slot);
  if (!avatarFile) {
    return NextResponse.json({ error: "PLAYER_AVATAR_NOT_FOUND" }, { status: 404 });
  }

  if ("redirectUrl" in avatarFile) {
    return Response.redirect(avatarFile.redirectUrl, 302);
  }

  return new NextResponse(new Uint8Array(avatarFile.bytes), {
    headers: {
      "Content-Type": avatarFile.contentType,
      "Cache-Control": avatarFile.cacheControl,
    },
  });
}
