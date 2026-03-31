import fs from "node:fs/promises";
import path from "node:path";
import { IncomingMessage } from "node:http";
import { Duplex } from "node:stream";
import { pack, unpack } from "msgpackr";
import { WebSocket, WebSocketServer } from "ws";
import { AvatarSelection, BlockId, normalizeAvatarSelection } from "@/src/game/shared/content";
import {
  CHUNK_HEIGHT,
  ChunkPos,
  Vec3Tuple,
  WorldPos,
  chunkKey,
  chunkPosFromWorld,
  clamp,
  desiredChunkSet,
  orderedChunkPositions,
  parseChunkKey,
  toChunkLocal,
} from "@/src/game/shared/math";
import {
  ChunkData,
  TerrainGenerator,
  cloneChunkData,
  deserializeChunk,
  serializeChunk,
  setChunkVoxel,
  withinReach,
} from "@/src/game/shared/world";
import {
  ClientMessage,
  PROTOCOL_VERSION,
  ServerMessage,
  WebRtcSignalPayload,
} from "@/src/game/shared/protocol";
import { loadAuthUser } from "@/src/lib/auth-user";

const PLAYER_RADIUS = 0.35;
const PLAYER_HEIGHT = 1.8;
const PLAYER_EYE_HEIGHT = 1.62;
const COLLISION_STEP = 0.2;
const STEP_HEIGHT = 0.6;

type AuthUser = Awaited<ReturnType<typeof loadAuthUser>>;

export type GameRuntimeConfig = {
  worldRoot: string;
  worldSeed: number;
  defaultViewRadius: number;
  heartbeatMs: number;
};

type ConnectionState = "await_hello" | "await_login" | "ready" | "closed";

type Session = {
  ws: WebSocket;
  request: IncomingMessage;
  authUser: AuthUser | null;
  state: ConnectionState;
  isAlive: boolean;
  playerId: number | null;
};

type PlayerRecord = {
  id: number;
  authUserId: string | null;
  displayName: string;
  position: Vec3Tuple;
  velocity: Vec3Tuple;
  yaw: number;
  avatarSelection: AvatarSelection;
  subscribedChunks: Set<string>;
};

class PersistenceService {
  private readonly cache = new Map<string, ChunkData | null>();

  constructor(private readonly root: string) {}

  async init() {
    await fs.mkdir(this.root, { recursive: true });
  }

  async loadChunk(position: ChunkPos) {
    const key = chunkKey(position);
    if (this.cache.has(key)) {
      return this.cache.get(key) ?? null;
    }

    const chunkPath = resolveChunkPath(this.root, position);
    try {
      const bytes = await fs.readFile(chunkPath);
      const chunk = deserializeChunk(bytes);
      this.cache.set(key, chunk);
      return chunk;
    } catch (error) {
      if (isNotFound(error)) {
        this.cache.set(key, null);
        return null;
      }
      throw error;
    }
  }

  async saveChunk(chunk: ChunkData) {
    const chunkPath = resolveChunkPath(this.root, chunk.position);
    await fs.mkdir(path.dirname(chunkPath), { recursive: true });
    await fs.writeFile(chunkPath, serializeChunk(chunk));
    this.cache.set(chunkKey(chunk.position), cloneChunkData(chunk));
  }
}

class WorldService {
  private readonly generator: TerrainGenerator;
  private readonly chunks = new Map<string, ChunkData>();

  constructor(
    private readonly persistence: PersistenceService,
    private readonly worldSeed: number,
  ) {
    this.generator = new TerrainGenerator(worldSeed);
  }

  async chunk(position: ChunkPos) {
    const key = chunkKey(position);
    const cached = this.chunks.get(key);
    if (cached) {
      return cached;
    }

    const stored = await this.persistence.loadChunk(position);
    const chunk = stored ? cloneChunkData(stored) : this.generator.generateChunk(position);
    this.chunks.set(key, chunk);
    return chunk;
  }

  async chunkOverride(position: ChunkPos) {
    const chunk = await this.chunk(position);
    return chunk.revision > 0 ? cloneChunkData(chunk) : null;
  }

  async applyBlockEdit(position: WorldPos, block: BlockId) {
    if (position.y < 0 || position.y >= CHUNK_HEIGHT) {
      return {
        accepted: false,
        reason: "block is outside vertical bounds",
        chunk: null as ChunkData | null,
      };
    }

    const { chunk: chunkPosition, local } = toChunkLocal(position);
    const chunk = cloneChunkData(await this.chunk(chunkPosition));
    setChunkVoxel(chunk, local, { block });
    this.chunks.set(chunkKey(chunkPosition), chunk);
    await this.persistence.saveChunk(chunk);

    return {
      accepted: true,
      reason: "ok",
      chunk,
    };
  }

  safeSpawnPosition(): WorldPos {
    const surface = this.generator.surfaceHeight(0, 0);
    return {
      x: 0,
      y: Math.min(surface + 3, CHUNK_HEIGHT - 1),
      z: 0,
    };
  }

  async resolvePlayerMotion(currentEyePosition: Vec3Tuple, movement: Vec3Tuple) {
    const velocity: Vec3Tuple = [movement[0] * 0.2, 0, movement[2] * 0.2];
    const position: Vec3Tuple = [...currentEyePosition];

    await this.sweepAxis(position, velocity[0], "x", true);
    await this.sweepAxis(position, velocity[2], "z", true);
    position[1] = clamp(position[1], 1 + PLAYER_EYE_HEIGHT, CHUNK_HEIGHT - 1 + PLAYER_EYE_HEIGHT);

    return {
      position,
      velocity,
    };
  }

  async validateReportedPosition(candidate: Vec3Tuple) {
    const position: Vec3Tuple = [...candidate];
    position[1] = clamp(position[1], 1 + PLAYER_EYE_HEIGHT, CHUNK_HEIGHT - 1 + PLAYER_EYE_HEIGHT);
    return !(await this.playerCollides(position));
  }

  private async sweepAxis(
    position: Vec3Tuple,
    delta: number,
    axis: "x" | "z",
    allowStep: boolean,
  ) {
    if (Math.abs(delta) <= Number.EPSILON) {
      return false;
    }

    const steps = Math.max(1, Math.ceil(Math.abs(delta) / COLLISION_STEP));
    const step = delta / steps;
    let moved = false;

    for (let index = 0; index < steps; index += 1) {
      const candidate: Vec3Tuple = [...position];
      if (axis === "x") {
        candidate[0] += step;
      } else {
        candidate[2] += step;
      }

      if (await this.playerCollides(candidate)) {
        if (allowStep) {
          const stepped: Vec3Tuple = [candidate[0], candidate[1] + STEP_HEIGHT, candidate[2]];
          if (!(await this.playerCollides(stepped))) {
            position[0] = stepped[0];
            position[1] = stepped[1];
            position[2] = stepped[2];
            moved = true;
            continue;
          }
        }
        return moved;
      }

      position[0] = candidate[0];
      position[1] = candidate[1];
      position[2] = candidate[2];
      moved = true;
    }

    return moved;
  }

  private async playerCollides(eyePosition: Vec3Tuple) {
    const min = [
      eyePosition[0] - PLAYER_RADIUS,
      eyePosition[1] - PLAYER_EYE_HEIGHT,
      eyePosition[2] - PLAYER_RADIUS,
    ] as const;
    const max = [
      eyePosition[0] + PLAYER_RADIUS,
      eyePosition[1] + (PLAYER_HEIGHT - PLAYER_EYE_HEIGHT),
      eyePosition[2] + PLAYER_RADIUS,
    ] as const;

    const minX = Math.floor(min[0]);
    const maxX = Math.floor(max[0] - 0.001);
    const minY = Math.floor(min[1]);
    const maxY = Math.floor(max[1] - 0.001);
    const minZ = Math.floor(min[2]);
    const maxZ = Math.floor(max[2] - 0.001);

    for (let y = minY; y <= maxY; y += 1) {
      for (let z = minZ; z <= maxZ; z += 1) {
        for (let x = minX; x <= maxX; x += 1) {
          if (await this.worldBlockIsSolid(x, y, z)) {
            return true;
          }
        }
      }
    }

    return false;
  }

  private async worldBlockIsSolid(x: number, y: number, z: number) {
    if (y < 0) {
      return true;
    }
    if (y >= CHUNK_HEIGHT) {
      return false;
    }

    const { chunk: chunkPosition, local } = toChunkLocal({ x, y, z });
    const chunk = await this.chunk(chunkPosition);
    return chunk.sections.length > 0 && unpackBlock(chunk, local) !== BlockId.Air && unpackBlock(chunk, local) !== BlockId.Water && unpackBlock(chunk, local) !== BlockId.Leaves && unpackBlock(chunk, local) !== BlockId.Glass;
  }

  generatorSeed() {
    return this.worldSeed;
  }
}

export class GameRuntime {
  readonly wss = new WebSocketServer({ noServer: true });
  private readonly sessions = new Map<WebSocket, Session>();
  private readonly players = new Map<number, PlayerRecord>();
  private readonly persistence: PersistenceService;
  private readonly world: WorldService;
  private nextPlayerId = 1;
  private heartbeat: NodeJS.Timeout | null = null;

  constructor(private readonly config: GameRuntimeConfig) {
    this.persistence = new PersistenceService(this.config.worldRoot);
    this.world = new WorldService(this.persistence, this.config.worldSeed);
  }

  async init() {
    await this.persistence.init();

    this.wss.on("connection", (ws: WebSocket, request: IncomingMessage) => {
      const req = request as IncomingMessage & { authUser?: AuthUser | null };
      const session: Session = {
        ws,
        request,
        authUser: req.authUser ?? null,
        state: "await_hello",
        isAlive: true,
        playerId: null,
      };
      this.sessions.set(ws, session);

      ws.on("pong", () => {
        session.isAlive = true;
      });

      ws.on("message", async (raw: WebSocket.RawData) => {
        try {
          await this.handleRawMessage(session, raw);
        } catch (error) {
          console.error("[game-runtime] message handling failed", error);
          this.safeClose(session.ws, 1011, "server error");
        }
      });

      ws.on("close", () => {
        void this.handleDisconnect(session);
      });

      ws.on("error", (error: Error) => {
        console.error("[game-runtime] websocket error", error);
      });
    });

    this.heartbeat = setInterval(() => {
      for (const session of this.sessions.values()) {
        if (!session.isAlive) {
          this.safeClose(session.ws, 1001, "heartbeat timeout");
          continue;
        }
        session.isAlive = false;
        session.ws.ping();
      }
    }, this.config.heartbeatMs);
  }

  async shutdown() {
    if (this.heartbeat) {
      clearInterval(this.heartbeat);
      this.heartbeat = null;
    }
    for (const session of this.sessions.values()) {
      this.safeClose(session.ws, 1001, "server shutdown");
    }
    this.wss.close();
  }

  async resolveAuthUser(request: IncomingMessage) {
    const cookieHeaders = Array.isArray(request.headers.cookie)
      ? request.headers.cookie.join("; ")
      : request.headers.cookie ?? "";
    const headerMap = new Headers();
    if (cookieHeaders) {
      headerMap.set("cookie", cookieHeaders);
    }
    if (request.headers.authorization) {
      headerMap.set("authorization", request.headers.authorization);
    }

    const { getToken } = await import("next-auth/jwt");
    const token = await getToken({
      req: { headers: headerMap },
      secret: process.env.AUTH_SECRET ?? "dev-only-auth-secret",
      cookieName: process.env.SESSION_COOKIE_NAME ?? "session_id",
      secureCookie:
        process.env.COOKIE_SECURE === "true" ||
        (process.env.WEB_BASE_URL ?? process.env.NEXTAUTH_URL ?? "").startsWith("https://"),
    });

    if (!token || typeof token.userId !== "string") {
      return null;
    }

    return loadAuthUser(token.userId);
  }

  async handleUpgrade(request: IncomingMessage, socket: Duplex, head: Buffer) {
    const authUser = await this.resolveAuthUser(request);
    (request as IncomingMessage & { authUser?: AuthUser | null }).authUser = authUser;

      this.wss.handleUpgrade(request, socket, head, (ws: WebSocket) => {
      this.wss.emit("connection", ws, request);
    });
  }

  private async handleRawMessage(session: Session, raw: WebSocket.RawData) {
    const bytes =
      raw instanceof Buffer
        ? raw
        : raw instanceof ArrayBuffer
          ? new Uint8Array(raw)
          : Array.isArray(raw)
            ? Buffer.concat(raw)
            : raw;
    const message = unpack(bytes) as ClientMessage;

    if (session.state === "await_hello") {
      if (message.type !== "client_hello" || message.protocolVersion !== PROTOCOL_VERSION) {
        this.safeClose(session.ws, 1002, "unsupported protocol");
        return;
      }

      session.state = "await_login";
      this.send(session.ws, {
        type: "server_hello",
        protocolVersion: PROTOCOL_VERSION,
        motd: "Augmego TypeScript frontier",
        worldSeed: this.config.worldSeed,
        defaultViewRadius: this.config.defaultViewRadius,
      });
      return;
    }

    if (session.state === "await_login") {
      if (message.type !== "login") {
        this.safeClose(session.ws, 1002, "expected login");
        return;
      }

      await this.handleLogin(session, message);
      return;
    }

    if (session.state !== "ready" || session.playerId === null) {
      return;
    }

    await this.routePlayerMessage(session, session.playerId, message);
  }

  private async handleLogin(session: Session, message: Extract<ClientMessage, { type: "login" }>) {
    const spawnPosition = this.world.safeSpawnPosition();
    const authFallbackName =
      session.authUser?.name ??
      session.authUser?.email ??
      null;
    const requestedName = message.name.trim();
    const displayName =
      requestedName ||
      authFallbackName ||
      `Guest ${Math.random().toString(36).slice(2, 8)}`;
    const avatarSelection = normalizeAvatarSelection(
      message.avatarSelection ?? session.authUser?.avatarSelection ?? undefined,
    );
    const playerId = this.nextPlayerId;
    this.nextPlayerId += 1;

    const player: PlayerRecord = {
      id: playerId,
      authUserId: session.authUser?.id ?? null,
      displayName,
      position: [spawnPosition.x + 0.5, spawnPosition.y, spawnPosition.z + 0.5],
      velocity: [0, 0, 0],
      yaw: 0,
      avatarSelection,
      subscribedChunks: new Set(),
    };
    this.players.set(playerId, player);
    session.playerId = playerId;
    session.state = "ready";

    this.send(session.ws, {
      type: "login_response",
      accepted: true,
      playerId,
      spawnPosition,
      message: `Welcome, ${displayName}`,
    });
    this.send(session.ws, {
      type: "inventory_snapshot",
      slots: [
        { block: BlockId.Grass, count: 64 },
        { block: BlockId.Stone, count: 64 },
        { block: BlockId.GoldOre, count: 32 },
        { block: BlockId.Planks, count: 32 },
      ],
    });
  }

  private async routePlayerMessage(session: Session, playerId: number, message: ClientMessage) {
    const player = this.players.get(playerId);
    if (!player) {
      return;
    }

    switch (message.type) {
      case "subscribe_chunks": {
        await this.updateSubscription(player, message.center, message.radius);
        this.sendPlayerSnapshot(session.ws, player, 0);
        this.broadcastPlayerSnapshot(player, 0, true);
        break;
      }
      case "player_input": {
        await this.handlePlayerInput(player, message);
        this.sendPlayerSnapshot(session.ws, player, message.tick);
        this.broadcastPlayerSnapshot(player, message.tick, false);
        break;
      }
      case "place_block": {
        await this.handleBlockEdit(player, message.position, message.block, "target outside placement reach");
        break;
      }
      case "break_block": {
        await this.handleBlockEdit(player, message.position, BlockId.Air, "target outside break reach");
        break;
      }
      case "chat": {
        this.broadcast({
          type: "chat",
          from: player.displayName,
          playerId: player.id,
          body: message.body.trim().slice(0, 500),
        });
        break;
      }
      case "webrtc_signal": {
        this.sendToPlayer(message.targetPlayerId, {
          type: "webrtc_signal",
          sourcePlayerId: player.id,
          payload: message.payload,
        });
        break;
      }
      case "avatar_update": {
        player.avatarSelection = normalizeAvatarSelection(message.avatarSelection);
        this.broadcastPlayerSnapshot(player, 0, true);
        break;
      }
      case "client_hello":
      case "login":
        break;
    }
  }

  private async updateSubscription(player: PlayerRecord, center: ChunkPos, radius: number) {
    const desired = desiredChunkSet(center, radius);
    const previous = new Set(player.subscribedChunks);
    player.subscribedChunks = desired;

    const removals = [...previous]
      .filter((key) => !desired.has(key))
      .map(parseChunkKey);
    if (removals.length > 0) {
      this.sendToPlayer(player.id, {
        type: "chunk_unload",
        positions: removals,
      });
    }

    for (const other of this.players.values()) {
      if (other.id === player.id) {
        continue;
      }
      const currentChunk = chunkPosFromWorld({
        x: Math.floor(other.position[0]),
        y: Math.floor(other.position[1]),
        z: Math.floor(other.position[2]),
      });
      if (desired.has(chunkKey(currentChunk))) {
        this.sendToPlayer(player.id, snapshotMessage(other, 0));
      }
    }

    const additions = orderedChunkPositions(center, radius)
      .filter((position) => !previous.has(chunkKey(position)));
    for (const position of additions) {
      const chunk = await this.world.chunkOverride(position);
      if (chunk) {
        this.sendToPlayer(player.id, {
          type: "chunk_data",
          chunk,
        });
      }
    }
  }

  private async handlePlayerInput(
    player: PlayerRecord,
    message: Extract<ClientMessage, { type: "player_input" }>,
  ) {
    let nextPosition = player.position;
    let nextVelocity = player.velocity;

    if (message.position && (await this.world.validateReportedPosition(message.position))) {
      nextPosition = [
        message.position[0],
        clamp(message.position[1], 1 + PLAYER_EYE_HEIGHT, CHUNK_HEIGHT - 1 + PLAYER_EYE_HEIGHT),
        message.position[2],
      ];
      nextVelocity = message.velocity ?? player.velocity;
    } else {
      const resolved = await this.world.resolvePlayerMotion(player.position, message.movement);
      nextPosition = resolved.position;
      nextVelocity = resolved.velocity;
    }

    player.position = nextPosition;
    player.velocity = nextVelocity;
    player.yaw = message.yaw ?? player.yaw;
  }

  private async handleBlockEdit(
    player: PlayerRecord,
    position: WorldPos,
    block: BlockId,
    outOfReachReason: string,
  ) {
    if (!withinReach(player.position, position)) {
      this.sendToPlayer(player.id, {
        type: "block_action_result",
        accepted: false,
        reason: outOfReachReason,
      });
      return;
    }

    const result = await this.world.applyBlockEdit(position, block);
    this.sendToPlayer(player.id, {
      type: "block_action_result",
      accepted: result.accepted,
      reason: result.reason,
    });

    if (result.accepted && result.chunk) {
      const subscribers = this.subscribersForChunk(result.chunk.position);
      for (const subscriberId of subscribers) {
        this.sendToPlayer(subscriberId, {
          type: "chunk_data",
          chunk: cloneChunkData(result.chunk),
        });
      }
    }
  }

  private subscribersForChunk(position: ChunkPos) {
    const key = chunkKey(position);
    return [...this.players.values()]
      .filter((player) => player.subscribedChunks.has(key))
      .map((player) => player.id);
  }

  private broadcastPlayerSnapshot(player: PlayerRecord, tick: number, includeSelf: boolean) {
    const currentChunk = chunkPosFromWorld({
      x: Math.floor(player.position[0]),
      y: Math.floor(player.position[1]),
      z: Math.floor(player.position[2]),
    });
    const subscribers = this.subscribersForChunk(currentChunk);
    for (const subscriberId of subscribers) {
      if (!includeSelf && subscriberId === player.id) {
        continue;
      }
      this.sendToPlayer(subscriberId, snapshotMessage(player, tick));
    }
  }

  private sendPlayerSnapshot(ws: WebSocket, player: PlayerRecord, tick: number) {
    this.send(ws, snapshotMessage(player, tick));
  }

  private sendToPlayer(playerId: number, message: ServerMessage) {
    for (const session of this.sessions.values()) {
      if (session.playerId === playerId && session.state === "ready") {
        this.send(session.ws, message);
        break;
      }
    }
  }

  private broadcast(message: ServerMessage) {
    for (const session of this.sessions.values()) {
      if (session.state === "ready") {
        this.send(session.ws, message);
      }
    }
  }

  private send(ws: WebSocket, message: ServerMessage) {
    if (ws.readyState !== WebSocket.OPEN) {
      return;
    }
    ws.send(pack(message));
  }

  private safeClose(ws: WebSocket, code: number, reason: string) {
    if (ws.readyState === WebSocket.OPEN || ws.readyState === WebSocket.CONNECTING) {
      ws.close(code, reason);
    }
  }

  private async handleDisconnect(session: Session) {
    session.state = "closed";
    this.sessions.delete(session.ws);
    if (session.playerId === null) {
      return;
    }

    const playerId = session.playerId;
    this.players.delete(playerId);
    this.broadcast({
      type: "player_left",
      playerId,
    });
  }
}

function resolveChunkPath(root: string, position: ChunkPos) {
  const regionX = Math.floor(position.x / 32);
  const regionZ = Math.floor(position.z / 32);
  return path.join(root, `r.${regionX}.${regionZ}`, `c.${position.x}.${position.z}.bin`);
}

function snapshotMessage(player: PlayerRecord, tick: number): ServerMessage {
  return {
    type: "player_state_snapshot",
    playerId: player.id,
    tick,
    displayName: player.displayName,
    position: [...player.position],
    velocity: [...player.velocity],
    yaw: player.yaw,
    avatarSelection: { ...player.avatarSelection },
  };
}

function isNotFound(error: unknown): error is NodeJS.ErrnoException {
  return error instanceof Error && "code" in error && (error as NodeJS.ErrnoException).code === "ENOENT";
}

function unpackBlock(chunk: ChunkData, local: { x: number; y: number; z: number }) {
  const section = Math.floor(local.y / 16);
  const yWithinSection = local.y % 16;
  const index = yWithinSection * 32 * 32 + local.z * 32 + local.x;
  const paletteSection = chunk.sections[section];
  return paletteSection.palette[paletteSection.indices[index] ?? 0] ?? BlockId.Air;
}
