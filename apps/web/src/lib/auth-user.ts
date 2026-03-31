import { prisma } from "@/src/lib/prisma";
import { loadUserAvatarSelection } from "@/src/lib/avatar";

type SupportedProvider = "google" | "apple" | "linkedin";

type ProviderProfile = {
  provider: SupportedProvider;
  providerAccountId: string;
  email: string | null;
  name: string | null;
  avatarUrl: string | null;
};

function providerField(provider: SupportedProvider) {
  if (provider === "google") {
    return "googleId" as const;
  }
  if (provider === "apple") {
    return "appleId" as const;
  }
  return "linkedinId" as const;
}

function cleanString(value: unknown, maxLength: number) {
  if (typeof value !== "string") {
    return null;
  }

  const trimmed = value.trim().slice(0, maxLength);
  return trimmed || null;
}

export function normalizeProviderProfile(
  provider: SupportedProvider,
  profile: unknown,
  providerAccountId: string,
): ProviderProfile {
  const record = typeof profile === "object" && profile !== null ? (profile as Record<string, unknown>) : {};
  const email =
    cleanString(record.email, 255) ??
    cleanString(record.emailAddress, 255);
  const avatarUrl =
    cleanString(record.picture, 2000) ??
    cleanString(record.image, 2000) ??
    cleanString(record.avatar_url, 2000) ??
    cleanString(record.profilePicture, 2000);
  const fullName =
    cleanString(record.name, 120) ??
    cleanString(
      [cleanString(record.localizedFirstName, 60), cleanString(record.localizedLastName, 60)]
        .filter(Boolean)
        .join(" "),
      120,
    );

  return {
    provider,
    providerAccountId,
    email,
    name: fullName,
    avatarUrl,
  };
}

export async function upsertUserFromProvider(profile: ProviderProfile) {
  const field = providerField(profile.provider);

  const existingByEmail = profile.email
    ? await prisma.user.findFirst({
        where: { email: profile.email },
        select: { id: true, googleId: true, appleId: true, linkedinId: true },
      })
    : null;

  const existingProviderId = existingByEmail?.[field];
  if (existingProviderId && existingProviderId !== profile.providerAccountId) {
    throw new Error(`Email already linked to another ${profile.provider} account.`);
  }

  if (profile.provider === "google") {
    const data = {
      googleId: profile.providerAccountId,
      email: profile.email,
      name: profile.name,
      avatarUrl: profile.avatarUrl,
    };

    if (existingByEmail) {
      return prisma.user.update({
        where: { id: existingByEmail.id },
        data,
        select: { id: true },
      });
    }

    return prisma.user.upsert({
      where: { googleId: profile.providerAccountId },
      create: data,
      update: data,
      select: { id: true },
    });
  }

  if (profile.provider === "apple") {
    const data = {
      appleId: profile.providerAccountId,
      email: profile.email,
      name: profile.name,
      avatarUrl: profile.avatarUrl,
    };

    if (existingByEmail) {
      return prisma.user.update({
        where: { id: existingByEmail.id },
        data,
        select: { id: true },
      });
    }

    return prisma.user.upsert({
      where: { appleId: profile.providerAccountId },
      create: data,
      update: data,
      select: { id: true },
    });
  }

  const data = {
    linkedinId: profile.providerAccountId,
    email: profile.email,
    name: profile.name,
    avatarUrl: profile.avatarUrl,
  };

  if (existingByEmail) {
    return prisma.user.update({
      where: { id: existingByEmail.id },
      data,
      select: { id: true },
    });
  }

  return prisma.user.upsert({
    where: { linkedinId: profile.providerAccountId },
    create: data,
    update: data,
    select: { id: true },
  });
}

export async function loadAuthUser(userId: string) {
  const user = await prisma.user.findUnique({
    where: { id: userId },
    select: {
      id: true,
      name: true,
      email: true,
      avatarUrl: true,
    },
  });

  if (!user) {
    return null;
  }

  const avatarSelection = await loadUserAvatarSelection(user.id);
  return {
    ...user,
    avatarSelection,
  };
}
