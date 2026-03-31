import NextAuth from "next-auth";
import Apple from "next-auth/providers/apple";
import Google from "next-auth/providers/google";
import LinkedIn from "next-auth/providers/linkedin";
import {
  appleClientId,
  appleScope,
  authSecret,
  googleClientId,
  googleClientSecret,
  googleScope,
  linkedinClientId,
  linkedinClientSecret,
  linkedinScope,
  resolveAppleClientSecret,
  sessionCookieName,
  sessionSameSite,
  sessionSecure,
  sessionTtlSeconds,
  webBaseUrl,
} from "@/src/lib/env";
import { loadAuthUser, normalizeProviderProfile, upsertUserFromProvider } from "@/src/lib/auth-user";

const providers = [];

if (googleClientId && googleClientSecret) {
  providers.push(
    Google({
      clientId: googleClientId,
      clientSecret: googleClientSecret,
      authorization: {
        params: {
          scope: googleScope,
        },
      },
    }),
  );
}

if (linkedinClientId && linkedinClientSecret) {
  providers.push(
    LinkedIn({
      clientId: linkedinClientId,
      clientSecret: linkedinClientSecret,
      authorization: {
        params: {
          scope: linkedinScope,
        },
      },
    }),
  );
}

const resolvedAppleClientSecret = resolveAppleClientSecret();
if (appleClientId && resolvedAppleClientSecret) {
  providers.push(
    Apple({
      clientId: appleClientId,
      clientSecret: resolvedAppleClientSecret,
      authorization: {
        params: {
          scope: appleScope,
          response_mode: "form_post",
        },
      },
    }),
  );
}

export const { auth, handlers, signIn, signOut } = NextAuth({
  trustHost: true,
  secret: authSecret,
  session: {
    strategy: "jwt",
    maxAge: sessionTtlSeconds,
  },
  providers,
  cookies: {
    sessionToken: {
      name: sessionCookieName,
      options: {
        httpOnly: true,
        sameSite: sessionSameSite,
        path: "/",
        secure: sessionSecure,
      },
    },
  },
  callbacks: {
    async jwt({ token, account, profile }) {
      if (account?.provider && account.providerAccountId) {
        const normalized = normalizeProviderProfile(
          account.provider as "google" | "apple" | "linkedin",
          profile,
          account.providerAccountId,
        );
        const user = await upsertUserFromProvider(normalized);
        token.userId = user.id;
      }

      return token;
    },
    async session({ session, token }) {
      if (typeof token.userId !== "string") {
        return session;
      }

      const user = await loadAuthUser(token.userId);
      if (!user) {
        return session;
      }

      session.user = {
        ...session.user,
        id: user.id,
        name: user.name ?? session.user?.name ?? "",
        email: user.email ?? session.user?.email ?? "",
        image: user.avatarUrl ?? session.user?.image ?? "",
      };

      return session;
    },
  },
});
