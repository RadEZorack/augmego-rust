import { NextResponse } from "next/server";
import { auth } from "@/src/auth";
import {
  isPlayerAvatarSlot,
  isValidGlbUpload,
  loadUserAvatarSelection,
  PlayerAvatarSlot,
  resolvePlayerAvatarFileUrl,
  savePlayerAvatarFile,
  updateUserAvatarSelection,
} from "@/src/lib/avatar";

async function collectAvatarFiles(formData: FormData) {
  const slotValue = String(formData.get("slot") ?? "").trim().toLowerCase();
  const uploadedFiles = new Map<PlayerAvatarSlot, File>();

  if (isPlayerAvatarSlot(slotValue)) {
    const singleFile = formData.get("file");
    if (!(singleFile instanceof File)) {
      return { error: "FILE_REQUIRED" as const, uploadedFiles };
    }
    uploadedFiles.set(slotValue, singleFile);
    return { error: null, uploadedFiles };
  }

  const slotInputs: Array<[PlayerAvatarSlot, string]> = [
    ["idle", "idleFile"],
    ["run", "runFile"],
    ["dance", "danceFile"],
  ];

  for (const [slot, key] of slotInputs) {
    const file = formData.get(key);
    if (file instanceof File) {
      uploadedFiles.set(slot, file);
    }
  }

  return { error: null, uploadedFiles };
}

export async function POST(request: Request) {
  const session = await auth();
  const userId = session?.user?.id;
  if (!userId) {
    return NextResponse.json({ error: "AUTH_REQUIRED" }, { status: 401 });
  }

  const formData = await request.formData();
  const { error, uploadedFiles } = await collectAvatarFiles(formData);
  if (error) {
    return NextResponse.json({ error }, { status: 400 });
  }
  if (uploadedFiles.size === 0) {
    return NextResponse.json({ error: "PLAYER_AVATAR_FILES_REQUIRED" }, { status: 400 });
  }

  for (const file of uploadedFiles.values()) {
    if (!isValidGlbUpload(file)) {
      return NextResponse.json({ error: "INVALID_GLB_FILE" }, { status: 400 });
    }
  }

  const nextSelection = await loadUserAvatarSelection(userId);
  for (const [slot, file] of uploadedFiles.entries()) {
    await savePlayerAvatarFile(file, userId, slot);
    if (slot === "idle") {
      nextSelection.stationaryModelUrl = resolvePlayerAvatarFileUrl(userId, slot);
    } else if (slot === "run") {
      nextSelection.moveModelUrl = resolvePlayerAvatarFileUrl(userId, slot);
    } else {
      nextSelection.specialModelUrl = resolvePlayerAvatarFileUrl(userId, slot);
    }
  }

  await updateUserAvatarSelection(userId, nextSelection);

  return NextResponse.json({
    ok: true,
    uploadedSlots: [...uploadedFiles.keys()],
    avatarSelection: nextSelection,
  });
}
