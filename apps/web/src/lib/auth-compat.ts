import { NextRequest } from "next/server";
import { handlers, signIn } from "@/src/auth";
import { webBaseUrl } from "@/src/lib/env";

const supportedProviders = new Set(["google", "apple", "linkedin"]);

export function isSupportedProvider(provider: string) {
  return supportedProviders.has(provider);
}

export function resolvePublicOrigin(request: Request) {
  const forwardedProto = request.headers.get("x-forwarded-proto");
  const forwardedHost = request.headers.get("x-forwarded-host");
  if (forwardedProto && forwardedHost) {
    return `${forwardedProto}://${forwardedHost}`;
  }

  const host = request.headers.get("host");
  if (host) {
    try {
      const url = new URL(request.url);
      return `${url.protocol}//${host}`;
    } catch {
      return `https://${host}`;
    }
  }

  return webBaseUrl;
}

export function resolveReturnTo(request: NextRequest) {
  const returnTo = request.nextUrl.searchParams.get("returnTo");
  if (returnTo?.startsWith("/")) {
    return returnTo;
  }

  return "/play";
}

export async function startProviderSignin(request: NextRequest, provider: string) {
  const redirectUrl = await signIn(provider, {
    redirect: false,
    redirectTo: resolveReturnTo(request),
  });
  return new Response(null, {
    status: 302,
    headers: {
      Location: new URL(redirectUrl, resolvePublicOrigin(request)).toString(),
    },
  });
}

export async function forwardProviderCallback(request: Request, provider: string) {
  const sourceUrl = new URL(request.url);
  const destination = new URL(`/api/auth/callback/${provider}`, resolvePublicOrigin(request));
  destination.search = sourceUrl.search;
  const forwardedRequest = new NextRequest(destination, request);
  const handler = request.method === "POST" ? handlers.POST : handlers.GET;
  return handler(forwardedRequest);
}
