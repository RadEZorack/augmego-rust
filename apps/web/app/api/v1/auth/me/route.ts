import { NextResponse } from "next/server";
import { auth } from "@/src/auth";
import { loadAuthUser } from "@/src/lib/auth-user";

export async function GET() {
  const session = await auth();
  const userId = session?.user?.id;

  if (!userId) {
    return NextResponse.json({ user: null });
  }

  const user = await loadAuthUser(userId);
  if (!user) {
    return NextResponse.json({ user: null });
  }

  return NextResponse.json({ user });
}
