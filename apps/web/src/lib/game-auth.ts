import jwt from "jsonwebtoken";
import { gameBackendAuthSecret } from "@/src/lib/env";

const GAME_AUTH_TTL_SECONDS = 60 * 10;

type GameAuthClaims = {
  sub: string;
};

export function signGameAuthToken(userId: string) {
  return jwt.sign({ sub: userId } satisfies GameAuthClaims, gameBackendAuthSecret, {
    algorithm: "HS256",
    expiresIn: GAME_AUTH_TTL_SECONDS,
  });
}
