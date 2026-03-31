import { NextRequest, NextResponse } from "next/server";
import { isSupportedProvider, startProviderSignin } from "@/src/lib/auth-compat";

type RouteContext = {
  params: Promise<{
    provider: string;
  }>;
};

export async function GET(request: NextRequest, context: RouteContext) {
  const { provider } = await context.params;
  if (!isSupportedProvider(provider)) {
    return NextResponse.json({ error: "UNKNOWN_PROVIDER" }, { status: 404 });
  }

  return startProviderSignin(request, provider);
}
