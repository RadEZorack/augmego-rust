import { NextResponse } from "next/server";
import { forwardProviderCallback, isSupportedProvider } from "@/src/lib/auth-compat";

type RouteContext = {
  params: Promise<{
    provider: string;
  }>;
};

async function handleCallback(request: Request, context: RouteContext) {
  const { provider } = await context.params;
  if (!isSupportedProvider(provider)) {
    return NextResponse.json({ error: "UNKNOWN_PROVIDER" }, { status: 404 });
  }

  return forwardProviderCallback(request, provider);
}

export async function GET(request: Request, context: RouteContext) {
  return handleCallback(request, context);
}

export async function POST(request: Request, context: RouteContext) {
  return handleCallback(request, context);
}
