import { NextResponse } from "next/server";
import { auth } from "@/src/auth";
import { loadAuthUser } from "@/src/lib/auth-user";
import { prisma } from "@/src/lib/prisma";

function normalizeAvatarUrl(value: unknown) {
  if (typeof value !== "string") {
    return undefined;
  }

  const trimmed = value.trim().slice(0, 500);
  if (!trimmed) {
    return null;
  }

  try {
    const parsed = new URL(trimmed);
    if (parsed.protocol !== "http:" && parsed.protocol !== "https:") {
      return undefined;
    }
    return parsed.toString();
  } catch {
    return undefined;
  }
}

export async function PATCH(request: Request) {
  const session = await auth();
  const userId = session?.user?.id;
  if (!userId) {
    return NextResponse.json({ error: "AUTH_REQUIRED" }, { status: 401 });
  }

  const body = (await request.json().catch(() => null)) as
    | {
        name?: unknown;
        avatarUrl?: unknown;
      }
    | null;
  if (!body) {
    return NextResponse.json({ error: "INVALID_PROFILE" }, { status: 400 });
  }

  const name =
    typeof body.name === "string" ? body.name.trim().slice(0, 80) || null : undefined;
  const avatarUrl = normalizeAvatarUrl(body.avatarUrl);

  if (body.avatarUrl !== undefined && avatarUrl === undefined) {
    return NextResponse.json({ error: "INVALID_AVATAR_URL" }, { status: 400 });
  }

  await prisma.user.update({
    where: { id: userId },
    data: {
      ...(name !== undefined ? { name } : {}),
      ...(avatarUrl !== undefined ? { avatarUrl } : {}),
    },
  });

  const user = await loadAuthUser(userId);
  return NextResponse.json({
    ok: true,
    user,
  });
}
