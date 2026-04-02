import { NextResponse } from "next/server";
import { readPetModelFile } from "@/src/lib/pets";

export async function GET(
  _request: Request,
  context: { params: Promise<{ petId: string }> },
) {
  const { petId } = await context.params;
  const file = await readPetModelFile(petId);
  if (!file) {
    return NextResponse.json({ error: "NOT_FOUND" }, { status: 404 });
  }

  if ("redirectUrl" in file && typeof file.redirectUrl === "string") {
    return NextResponse.redirect(file.redirectUrl);
  }

  return new NextResponse(file.bytes, {
    headers: {
      "Content-Type": file.contentType,
      "Cache-Control": file.cacheControl,
    },
  });
}
