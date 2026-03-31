import { NextResponse } from "next/server";
import { sessionCookieName, sessionSameSite, sessionSecure } from "@/src/lib/env";

export async function POST() {
  const response = NextResponse.json({ ok: true });
  response.cookies.set({
    name: sessionCookieName,
    value: "",
    httpOnly: true,
    sameSite: sessionSameSite,
    secure: sessionSecure,
    path: "/",
    maxAge: 0,
  });
  return response;
}
