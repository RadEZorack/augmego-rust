import http from "node:http";
import { Duplex } from "node:stream";
import next from "next";
import { gameViewRadius, gameWorldRoot, gameWorldSeed, gameWsHeartbeatMs } from "@/src/lib/env";
import { GameRuntime } from "@/src/server/game/runtime";

const dev = process.env.NODE_ENV !== "production";
const hostname = process.env.HOST || "0.0.0.0";
const port = Number(process.env.PORT || "3000");

async function main() {
  const app = next({ dev, hostname, port });
  const gameRuntime = new GameRuntime({
    worldRoot: gameWorldRoot,
    worldSeed: gameWorldSeed,
    defaultViewRadius: gameViewRadius,
    heartbeatMs: gameWsHeartbeatMs,
  });

  await Promise.all([app.prepare(), gameRuntime.init()]);

  const handle = app.getRequestHandler();
  const handleUpgrade = app.getUpgradeHandler();

  const server = http.createServer((request, response) => {
    void handle(request, response);
  });

  server.on("upgrade", (request, socket, head) => {
    if (request.url?.startsWith("/ws")) {
      void gameRuntime.handleUpgrade(request, socket as Duplex, head).catch((error) => {
        console.error("[server] websocket upgrade failed", error);
        socket.destroy();
      });
      return;
    }

    void handleUpgrade(request, socket as Duplex, head).catch((error) => {
      console.error("[server] websocket upgrade failed", error);
      socket.destroy();
    });
  });

  server.listen(port, hostname, () => {
    console.log(`[server] ready on http://${hostname}:${port}`);
    console.log(`[server] websocket endpoint ws://${hostname}:${port}/ws`);
  });

  const shutdown = async () => {
    await gameRuntime.shutdown();
    server.closeAllConnections();
    server.close(() => {
      process.exit(0);
    });
  };

  process.on("SIGINT", () => {
    void shutdown();
  });
  process.on("SIGTERM", () => {
    void shutdown();
  });
}

void main().catch((error) => {
  console.error("[server] failed to start", error);
  process.exit(1);
});
