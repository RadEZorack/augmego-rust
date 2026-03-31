FROM node:22-bookworm AS web-builder

WORKDIR /app/apps/web

COPY apps/web/package.json ./package.json
COPY apps/web/package-lock.json ./package-lock.json
COPY prisma /app/prisma

RUN npm ci

COPY apps/web ./

RUN npm run build

FROM node:22-bookworm-slim AS runner

WORKDIR /app/apps/web

ENV NODE_ENV=production
ENV PORT=3000

RUN apt-get update \
  && apt-get install -y --no-install-recommends ca-certificates openssl \
  && rm -rf /var/lib/apt/lists/*

COPY --from=web-builder /app/prisma /app/prisma
COPY --from=web-builder /app/apps/web/.next ./.next
COPY --from=web-builder /app/apps/web/public ./public
COPY --from=web-builder /app/apps/web/package.json ./package.json
COPY --from=web-builder /app/apps/web/node_modules ./node_modules
COPY --from=web-builder /app/apps/web/next.config.ts ./next.config.ts
COPY --from=web-builder /app/apps/web/server.ts ./server.ts
COPY --from=web-builder /app/apps/web/src ./src
COPY --from=web-builder /app/apps/web/tsconfig.json ./tsconfig.json
COPY --from=web-builder /app/apps/web/next-env.d.ts ./next-env.d.ts

EXPOSE 3000

CMD ["npm", "run", "start"]
