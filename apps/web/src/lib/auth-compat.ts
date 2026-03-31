import { NextRequest } from "next/server";
import { handlers } from "@/src/auth";

const supportedProviders = new Set(["google", "apple", "linkedin"]);

export function isSupportedProvider(provider: string) {
  return supportedProviders.has(provider);
}

export function resolveReturnTo(request: NextRequest) {
  const returnTo = request.nextUrl.searchParams.get("returnTo");
  if (returnTo?.startsWith("/")) {
    return returnTo;
  }

  return "/play";
}

export function redirectToProviderSignin(request: NextRequest, provider: string) {
  const destination = new URL(`/api/auth/signin/${provider}`, request.url);
  destination.searchParams.set("callbackUrl", resolveReturnTo(request));
  return Response.redirect(destination, 302);
}

export async function forwardProviderCallback(request: Request, provider: string) {
  const sourceUrl = new URL(request.url);
  const destination = new URL(`/api/auth/callback/${provider}`, sourceUrl.origin);
  destination.search = sourceUrl.search;
  const forwardedRequest = new NextRequest(destination, request);
  const handler = request.method === "POST" ? handlers.POST : handlers.GET;
  return handler(forwardedRequest);
}
