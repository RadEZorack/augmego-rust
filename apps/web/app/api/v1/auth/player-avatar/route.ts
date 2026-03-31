import { NextResponse } from "next/server";
import { auth } from "@/src/auth";
import {
  loadUserAvatarSelection,
  normalizeAvatarUrl,
  updateUserAvatarSelection,
} from "@/src/lib/avatar";

export async function PATCH(request: Request) {
  const session = await auth();
  const userId = session?.user?.id;
  if (!userId) {
    return NextResponse.json({ error: "AUTH_REQUIRED" }, { status: 401 });
  }

  const body = (await request.json().catch(() => null)) as
    | {
        stationaryModelUrl?: unknown;
        moveModelUrl?: unknown;
        specialModelUrl?: unknown;
        idleModelUrl?: unknown;
        runModelUrl?: unknown;
        danceModelUrl?: unknown;
      }
    | null;
  if (!body) {
    return NextResponse.json({ error: "INVALID_AVATAR_SELECTION" }, { status: 400 });
  }

  const current = await loadUserAvatarSelection(userId);
  const next = { ...current };

  const resolveOptionalUrl = (value: unknown) => {
    if (value === undefined) {
      return { provided: false, value: null as string | null };
    }

    if (typeof value === "string" && value.trim() === "") {
      return { provided: true, value: null as string | null };
    }

    const normalized = normalizeAvatarUrl(value);
    if (normalized === null) {
      return { provided: true, value: null as string | null, invalid: true };
    }

    return { provided: true, value: normalized };
  };

  if (body.stationaryModelUrl !== undefined || body.idleModelUrl !== undefined) {
    const result = resolveOptionalUrl(
      body.stationaryModelUrl !== undefined ? body.stationaryModelUrl : body.idleModelUrl,
    );
    if (result.invalid) {
      return NextResponse.json({ error: "INVALID_AVATAR_URL" }, { status: 400 });
    }
    next.stationaryModelUrl = result.value;
  }

  if (body.moveModelUrl !== undefined || body.runModelUrl !== undefined) {
    const result = resolveOptionalUrl(
      body.moveModelUrl !== undefined ? body.moveModelUrl : body.runModelUrl,
    );
    if (result.invalid) {
      return NextResponse.json({ error: "INVALID_AVATAR_URL" }, { status: 400 });
    }
    next.moveModelUrl = result.value;
  }

  if (body.specialModelUrl !== undefined || body.danceModelUrl !== undefined) {
    const result = resolveOptionalUrl(
      body.specialModelUrl !== undefined ? body.specialModelUrl : body.danceModelUrl,
    );
    if (result.invalid) {
      return NextResponse.json({ error: "INVALID_AVATAR_URL" }, { status: 400 });
    }
    next.specialModelUrl = result.value;
  }

  await updateUserAvatarSelection(userId, next);

  return NextResponse.json({
    ok: true,
    avatarSelection: next,
  });
}
