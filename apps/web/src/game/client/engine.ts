import { pack, unpack } from "msgpackr";
import {
  AvatarSelection,
  BlockId,
  currentAvatarUrl,
  normalizeAvatarSelection,
} from "@/src/game/shared/content";
import {
  CHUNK_HEIGHT,
  ChunkPos,
  LocalVoxelPos,
  Vec3Tuple,
  WorldPos,
  chunkPosFromWorld,
  clamp,
  orderedChunkPositions,
  raycastGrid,
  toChunkLocal,
} from "@/src/game/shared/math";
import {
  ChunkData,
  TerrainGenerator,
  chunkVoxel,
  cloneChunkData,
  setChunkVoxel,
  withinReach,
} from "@/src/game/shared/world";
import { ClientMessage, PROTOCOL_VERSION, ServerMessage, WebRtcSignalPayload } from "@/src/game/shared/protocol";

const PLAYER_RADIUS = 0.35;
const PLAYER_HEIGHT = 1.8;
const PLAYER_EYE_HEIGHT = 1.62;
const PLAYER_WALK_SPEED = 7.5;
const PLAYER_SPRINT_SPEED = 11;
const PLAYER_JUMP_SPEED = 9.5;
const PLAYER_GRAVITY = 28;
const STEP_HEIGHT = 0.6;
const COLLISION_STEP = 0.2;
const CLIENT_RENDER_RADIUS = 2;
const CLIENT_SUBSCRIBE_RADIUS_FALLBACK = 4;
const MAX_CHAT_MESSAGES = 40;

export type GameAuthUser = {
  id: string;
  name: string | null;
  email: string | null;
  avatarUrl: string | null;
  avatarSelection: AvatarSelection;
} | null;

export type RemotePlayer = {
  playerId: number;
  displayName: string;
  position: Vec3Tuple;
  velocity: Vec3Tuple;
  yaw: number;
  avatarSelection: AvatarSelection;
  updatedAt: number;
};

export type ChatEntry = {
  id: string;
  from: string;
  body: string;
};

export type TargetSelection = {
  breakPosition: WorldPos;
  placePosition: WorldPos | null;
};

export type GameSnapshot = {
  status: "booting" | "connecting" | "ready" | "closed" | "error";
  error: string | null;
  authUser: GameAuthUser;
  localPlayerId: number | null;
  localPosition: Vec3Tuple;
  localVelocity: Vec3Tuple;
  yaw: number;
  pitch: number;
  worldSeed: number;
  viewRadius: number;
  worldRevision: number;
  target: TargetSelection | null;
  selectedSlot: number;
  inventory: Array<{ block: BlockId; count: number }>;
  remotePlayers: RemotePlayer[];
  chatMessages: ChatEntry[];
  localStream: MediaStream | null;
  remoteStreams: Array<{ playerId: number; stream: MediaStream }>;
  mediaError: string | null;
};

type PeerState = {
  connection: RTCPeerConnection;
  pendingIce: RTCIceCandidateInit[];
  remoteStream: MediaStream | null;
};

type EngineOptions = {
  authUser: GameAuthUser;
};

export class GameEngine extends EventTarget {
  private authUser: GameAuthUser;
  private ws: WebSocket | null = null;
  private status: GameSnapshot["status"] = "booting";
  private error: string | null = null;
  private localPlayerId: number | null = null;
  private localPosition: Vec3Tuple = [0.5, 90, 0.5];
  private localVelocity: Vec3Tuple = [0, 0, 0];
  private yaw = 0;
  private pitch = 0;
  private onGround = false;
  private worldSeed = 0xa66de601;
  private viewRadius = CLIENT_SUBSCRIBE_RADIUS_FALLBACK;
  private generator = new TerrainGenerator(this.worldSeed);
  private authoritativeChunks = new Map<string, ChunkData>();
  private optimisticChunks = new Map<string, ChunkData>();
  private generatedChunks = new Map<string, ChunkData>();
  private remotePlayers = new Map<number, RemotePlayer>();
  private inventory: Array<{ block: BlockId; count: number }> = [
    { block: BlockId.Grass, count: 64 },
    { block: BlockId.Stone, count: 64 },
    { block: BlockId.GoldOre, count: 32 },
    { block: BlockId.Planks, count: 32 },
  ];
  private selectedSlot = 0;
  private chatMessages: ChatEntry[] = [];
  private worldRevision = 0;
  private target: TargetSelection | null = null;
  private readonly keyState = new Set<string>();
  private lastSentAt = 0;
  private tickCounter = 0;
  private subscribedCenter: string | null = null;
  private localStream: MediaStream | null = null;
  private mediaError: string | null = null;
  private peers = new Map<number, PeerState>();

  constructor(options: EngineOptions) {
    super();
    this.authUser = options.authUser;
    this.connect();
  }

  dispose() {
    if (this.ws) {
      this.ws.close();
      this.ws = null;
    }
    for (const peer of this.peers.values()) {
      peer.connection.close();
    }
    this.peers.clear();
    if (this.localStream) {
      for (const track of this.localStream.getTracks()) {
        track.stop();
      }
      this.localStream = null;
    }
  }

  subscribe(listener: () => void) {
    const handler = () => listener();
    this.addEventListener("change", handler);
    return () => this.removeEventListener("change", handler);
  }

  getSnapshot(): GameSnapshot {
    return {
      status: this.status,
      error: this.error,
      authUser: this.authUser,
      localPlayerId: this.localPlayerId,
      localPosition: [...this.localPosition],
      localVelocity: [...this.localVelocity],
      yaw: this.yaw,
      pitch: this.pitch,
      worldSeed: this.worldSeed,
      viewRadius: this.viewRadius,
      worldRevision: this.worldRevision,
      target: this.target,
      selectedSlot: this.selectedSlot,
      inventory: [...this.inventory],
      remotePlayers: [...this.remotePlayers.values()].sort((a, b) => a.playerId - b.playerId),
      chatMessages: [...this.chatMessages],
      localStream: this.localStream,
      remoteStreams: [...this.peers.entries()]
        .map(([playerId, peer]) => ({ playerId, stream: peer.remoteStream }))
        .filter((entry): entry is { playerId: number; stream: MediaStream } => entry.stream instanceof MediaStream),
      mediaError: this.mediaError,
    };
  }

  setAuthUser(user: GameAuthUser) {
    this.authUser = user;
    this.send({
      type: "avatar_update",
      avatarSelection: normalizeAvatarSelection(user?.avatarSelection),
    });
    this.emitChange();
  }

  setSelectedSlot(index: number) {
    this.selectedSlot = clamp(index, 0, Math.max(0, this.inventory.length - 1));
    this.emitChange();
  }

  setKey(code: string, pressed: boolean) {
    if (pressed) {
      this.keyState.add(code);
    } else {
      this.keyState.delete(code);
    }
  }

  look(deltaX: number, deltaY: number) {
    this.yaw -= deltaX * 0.0024;
    this.pitch = clamp(this.pitch - deltaY * 0.0024, -Math.PI / 2 + 0.05, Math.PI / 2 - 0.05);
    this.emitChange();
  }

  async requestMedia() {
    try {
      this.localStream = await navigator.mediaDevices.getUserMedia({
        audio: true,
        video: {
          width: 640,
          height: 360,
        },
      });
      this.mediaError = null;
      for (const playerId of this.remotePlayers.keys()) {
        this.ensurePeerConnection(playerId);
      }
    } catch (error) {
      this.mediaError = error instanceof Error ? error.message : "Unable to access media devices.";
    }
    this.emitChange();
  }

  sendChat(body: string) {
    const message = body.trim();
    if (!message) {
      return;
    }
    this.send({
      type: "chat",
      body: message.slice(0, 500),
    });
  }

  primaryAction() {
    if (!this.target?.breakPosition || !this.localPlayerId) {
      return;
    }

    const position = this.target.breakPosition;
    this.applyLocalChunkEdit(position, BlockId.Air);
    this.send({
      type: "break_block",
      position,
    });
  }

  secondaryAction() {
    if (!this.target?.placePosition || !this.localPlayerId) {
      return;
    }

    const selected = this.inventory[this.selectedSlot];
    if (!selected) {
      return;
    }

    const position = this.target.placePosition;
    if (!withinReach(this.localPosition, position)) {
      return;
    }

    this.applyLocalChunkEdit(position, selected.block);
    this.send({
      type: "place_block",
      position,
      block: selected.block,
    });
  }

  update(deltaSeconds: number) {
    if (this.localPlayerId !== null) {
      this.updatePhysics(deltaSeconds);
      this.updateTarget();
      this.updateSubscriptions();
      this.flushPlayerState();
    }
  }

  getRenderableChunks() {
    const center = chunkPosFromWorld({
      x: Math.floor(this.localPosition[0]),
      y: Math.floor(this.localPosition[1]),
      z: Math.floor(this.localPosition[2]),
    });

    return orderedChunkPositions(center, Math.min(CLIENT_RENDER_RADIUS, this.viewRadius))
      .map((position) => this.getEffectiveChunk(position));
  }

  private connect() {
    this.status = "connecting";
    this.emitChange();

    const protocol = window.location.protocol === "https:" ? "wss:" : "ws:";
    this.ws = new WebSocket(`${protocol}//${window.location.host}/ws`);
    this.ws.binaryType = "arraybuffer";

    this.ws.addEventListener("open", () => {
      this.status = "connecting";
      this.send({
        type: "client_hello",
        protocolVersion: PROTOCOL_VERSION,
        clientName: "augmego-next-client",
      });
      this.emitChange();
    });

    this.ws.addEventListener("message", (event) => {
      if (!(event.data instanceof ArrayBuffer)) {
        return;
      }
      const message = unpack(new Uint8Array(event.data)) as ServerMessage;
      this.handleServerMessage(message);
    });

    this.ws.addEventListener("close", () => {
      this.status = "closed";
      this.emitChange();
    });

    this.ws.addEventListener("error", () => {
      this.status = "error";
      this.error = "Realtime connection failed.";
      this.emitChange();
    });
  }

  private handleServerMessage(message: ServerMessage) {
    switch (message.type) {
      case "server_hello": {
        this.worldSeed = message.worldSeed;
        this.viewRadius = Math.max(1, message.defaultViewRadius);
        this.generator = new TerrainGenerator(this.worldSeed);
        this.status = "connecting";
        this.login();
        break;
      }
      case "login_response": {
        this.localPlayerId = message.playerId;
        this.localPosition = [message.spawnPosition.x + 0.5, message.spawnPosition.y, message.spawnPosition.z + 0.5];
        this.localVelocity = [0, 0, 0];
        this.status = "ready";
        this.updateSubscriptions(true);
        break;
      }
      case "inventory_snapshot": {
        this.inventory = [...message.slots];
        break;
      }
      case "chunk_data": {
        const key = this.chunkKeyFor(message.chunk.position);
        this.authoritativeChunks.set(key, cloneChunkData(message.chunk));
        this.optimisticChunks.delete(key);
        this.bumpWorldRevision();
        break;
      }
      case "chunk_unload": {
        for (const position of message.positions) {
          const key = this.chunkKeyFor(position);
          this.authoritativeChunks.delete(key);
          this.optimisticChunks.delete(key);
        }
        this.bumpWorldRevision();
        break;
      }
      case "player_state_snapshot": {
        if (message.playerId === this.localPlayerId) {
          this.localPosition = [...message.position];
          this.localVelocity = [...message.velocity];
        } else {
          const remote = this.remotePlayers.get(message.playerId);
          this.remotePlayers.set(message.playerId, {
            playerId: message.playerId,
            displayName: message.displayName,
            position: [...message.position],
            velocity: [...message.velocity],
            yaw: message.yaw,
            avatarSelection: normalizeAvatarSelection(message.avatarSelection),
            updatedAt: performance.now(),
          });
          if (!remote && this.localStream) {
            this.ensurePeerConnection(message.playerId);
          }
        }
        break;
      }
      case "player_left": {
        this.remotePlayers.delete(message.playerId);
        this.closePeer(message.playerId);
        break;
      }
      case "block_action_result": {
        if (!message.accepted) {
          this.error = message.reason;
        } else {
          this.error = null;
        }
        break;
      }
      case "chat": {
        this.chatMessages = [
          ...this.chatMessages,
          {
            id: `${Date.now()}-${Math.random().toString(36).slice(2, 8)}`,
            from: message.from,
            body: message.body,
          },
        ].slice(-MAX_CHAT_MESSAGES);
        break;
      }
      case "webrtc_signal": {
        this.handleSignal(message.sourcePlayerId, message.payload);
        break;
      }
    }

    this.emitChange();
  }

  private login() {
    const guestName = `Guest ${Math.random().toString(36).slice(2, 8)}`;
    const name = this.authUser?.name ?? this.authUser?.email ?? guestName;
    this.send({
      type: "login",
      name,
      avatarSelection: normalizeAvatarSelection(this.authUser?.avatarSelection),
    });
  }

  private updatePhysics(deltaSeconds: number) {
    const moveForward = (this.keyState.has("KeyW") ? 1 : 0) - (this.keyState.has("KeyS") ? 1 : 0);
    const moveSide = (this.keyState.has("KeyD") ? 1 : 0) - (this.keyState.has("KeyA") ? 1 : 0);
    const sprinting = this.keyState.has("ShiftLeft") || this.keyState.has("ShiftRight");
    const speed = sprinting ? PLAYER_SPRINT_SPEED : PLAYER_WALK_SPEED;
    const cosine = Math.cos(this.yaw);
    const sine = Math.sin(this.yaw);
    let inputX = 0;
    let inputZ = 0;

    if (moveForward !== 0 || moveSide !== 0) {
      const length = Math.hypot(moveForward, moveSide) || 1;
      const forwardX = -Math.sin(this.yaw);
      const forwardZ = -Math.cos(this.yaw);
      const rightX = Math.cos(this.yaw);
      const rightZ = -Math.sin(this.yaw);
      inputX = ((forwardX * moveForward) + (rightX * moveSide)) / length;
      inputZ = ((forwardZ * moveForward) + (rightZ * moveSide)) / length;
    } else {
      inputX = -sine * 0;
      inputZ = -cosine * 0;
    }

    const horizontalStep: Vec3Tuple = [inputX * speed * deltaSeconds, 0, inputZ * speed * deltaSeconds];
    this.sweepAxis(this.localPosition, horizontalStep[0], "x", true);
    this.sweepAxis(this.localPosition, horizontalStep[2], "z", true);

    if ((this.keyState.has("Space") || this.keyState.has("KeyF")) && this.onGround) {
      this.localVelocity[1] = PLAYER_JUMP_SPEED;
      this.onGround = false;
    }

    this.localVelocity[1] -= PLAYER_GRAVITY * deltaSeconds;
    const nextVertical = this.localPosition[1] + this.localVelocity[1] * deltaSeconds;
    const candidate: Vec3Tuple = [this.localPosition[0], nextVertical, this.localPosition[2]];
    if (this.playerCollides(candidate)) {
      if (this.localVelocity[1] < 0) {
        this.onGround = true;
      }
      this.localVelocity[1] = 0;
    } else {
      this.localPosition[1] = candidate[1];
      this.onGround = false;
    }

    this.localPosition[1] = clamp(this.localPosition[1], 1 + PLAYER_EYE_HEIGHT, CHUNK_HEIGHT - 1 + PLAYER_EYE_HEIGHT);
  }

  private sweepAxis(position: Vec3Tuple, delta: number, axis: "x" | "z", allowStep: boolean) {
    if (Math.abs(delta) <= Number.EPSILON) {
      return;
    }

    const steps = Math.max(1, Math.ceil(Math.abs(delta) / COLLISION_STEP));
    const step = delta / steps;

    for (let index = 0; index < steps; index += 1) {
      const candidate: Vec3Tuple = [...position];
      if (axis === "x") {
        candidate[0] += step;
      } else {
        candidate[2] += step;
      }

      if (this.playerCollides(candidate)) {
        if (allowStep) {
          const stepped: Vec3Tuple = [candidate[0], candidate[1] + STEP_HEIGHT, candidate[2]];
          if (!this.playerCollides(stepped)) {
            position[0] = stepped[0];
            position[1] = stepped[1];
            position[2] = stepped[2];
            continue;
          }
        }
        return;
      }

      position[0] = candidate[0];
      position[1] = candidate[1];
      position[2] = candidate[2];
    }
  }

  private playerCollides(eyePosition: Vec3Tuple) {
    const minX = Math.floor(eyePosition[0] - PLAYER_RADIUS);
    const maxX = Math.floor(eyePosition[0] + PLAYER_RADIUS - 0.001);
    const minY = Math.floor(eyePosition[1] - PLAYER_EYE_HEIGHT);
    const maxY = Math.floor(eyePosition[1] + (PLAYER_HEIGHT - PLAYER_EYE_HEIGHT) - 0.001);
    const minZ = Math.floor(eyePosition[2] - PLAYER_RADIUS);
    const maxZ = Math.floor(eyePosition[2] + PLAYER_RADIUS - 0.001);

    for (let y = minY; y <= maxY; y += 1) {
      for (let z = minZ; z <= maxZ; z += 1) {
        for (let x = minX; x <= maxX; x += 1) {
          if (this.worldBlockIsSolid(x, y, z)) {
            return true;
          }
        }
      }
    }

    return false;
  }

  private worldBlockIsSolid(x: number, y: number, z: number) {
    if (y < 0) {
      return true;
    }
    if (y >= CHUNK_HEIGHT) {
      return false;
    }

    const { chunk, local } = toChunkLocal({ x, y, z });
    const block = chunkVoxel(this.getEffectiveChunk(chunk), local).block;
    return ![BlockId.Air, BlockId.Water, BlockId.Leaves, BlockId.Glass].includes(block);
  }

  private updateTarget() {
    const direction = this.forwardVector();
    let previous: WorldPos | null = null;
    let selection: TargetSelection | null = null;

    for (const position of raycastGrid(this.localPosition, direction, 6)) {
      if (this.worldBlockIsSolid(position.x, position.y, position.z)) {
        selection = {
          breakPosition: position,
          placePosition: previous,
        };
        break;
      }
      previous = position;
    }

    this.target = selection;
  }

  private updateSubscriptions(force = false) {
    if (this.localPlayerId === null) {
      return;
    }

    const center = chunkPosFromWorld({
      x: Math.floor(this.localPosition[0]),
      y: Math.floor(this.localPosition[1]),
      z: Math.floor(this.localPosition[2]),
    });
    const key = this.chunkKeyFor(center);
    if (!force && this.subscribedCenter === key) {
      return;
    }

    this.subscribedCenter = key;
    this.send({
      type: "subscribe_chunks",
      center,
      radius: this.viewRadius,
    });
  }

  private flushPlayerState() {
    const now = performance.now();
    if (now - this.lastSentAt < 50 || this.localPlayerId === null) {
      return;
    }

    this.lastSentAt = now;
    this.tickCounter += 1;
    const moveForward = (this.keyState.has("KeyW") ? 1 : 0) - (this.keyState.has("KeyS") ? 1 : 0);
    const moveSide = (this.keyState.has("KeyD") ? 1 : 0) - (this.keyState.has("KeyA") ? 1 : 0);
    const forwardX = -Math.sin(this.yaw);
    const forwardZ = -Math.cos(this.yaw);
    const rightX = Math.cos(this.yaw);
    const rightZ = -Math.sin(this.yaw);
    const movement: Vec3Tuple = [
      forwardX * moveForward + rightX * moveSide,
      0,
      forwardZ * moveForward + rightZ * moveSide,
    ];

    this.send({
      type: "player_input",
      tick: this.tickCounter,
      movement,
      position: [...this.localPosition],
      velocity: [...this.localVelocity],
      yaw: this.yaw,
      jump: this.keyState.has("Space"),
    });
  }

  private getEffectiveChunk(position: ChunkPos) {
    const key = this.chunkKeyFor(position);
    const optimistic = this.optimisticChunks.get(key);
    if (optimistic) {
      return optimistic;
    }

    const authoritative = this.authoritativeChunks.get(key);
    if (authoritative) {
      return authoritative;
    }

    const cached = this.generatedChunks.get(key);
    if (cached) {
      return cached;
    }

    const generated = this.generator.generateChunk(position);
    this.generatedChunks.set(key, generated);
    return generated;
  }

  private applyLocalChunkEdit(position: WorldPos, block: BlockId) {
    const { chunk, local } = toChunkLocal(position);
    const base = cloneChunkData(this.getEffectiveChunk(chunk));
    setChunkVoxel(base, local, { block });
    this.optimisticChunks.set(this.chunkKeyFor(chunk), base);
    this.bumpWorldRevision();
  }

  private async handleSignal(sourcePlayerId: number, payload: WebRtcSignalPayload) {
    const peer = this.ensurePeerConnection(sourcePlayerId, false);
    if (!peer) {
      return;
    }

    if (payload.type === "offer") {
      await peer.connection.setRemoteDescription({
        type: "offer",
        sdp: payload.sdp,
      });
      for (const candidate of peer.pendingIce.splice(0)) {
        await peer.connection.addIceCandidate(candidate);
      }
      const answer = await peer.connection.createAnswer();
      await peer.connection.setLocalDescription(answer);
      this.send({
        type: "webrtc_signal",
        targetPlayerId: sourcePlayerId,
        payload: {
          type: "answer",
          sdp: answer.sdp ?? "",
        },
      });
      return;
    }

    if (payload.type === "answer") {
      await peer.connection.setRemoteDescription({
        type: "answer",
        sdp: payload.sdp,
      });
      for (const candidate of peer.pendingIce.splice(0)) {
        await peer.connection.addIceCandidate(candidate);
      }
      return;
    }

    const candidate: RTCIceCandidateInit = {
      candidate: payload.candidate,
      sdpMid: payload.sdpMid ?? undefined,
      sdpMLineIndex: payload.sdpMLineIndex ?? undefined,
    };
    if (peer.connection.remoteDescription) {
      await peer.connection.addIceCandidate(candidate);
    } else {
      peer.pendingIce.push(candidate);
    }
  }

  private ensurePeerConnection(playerId: number, shouldOffer = true) {
    if (playerId === this.localPlayerId) {
      return null;
    }

    const existing = this.peers.get(playerId);
    if (existing) {
      if (shouldOffer) {
        void this.maybeCreateOffer(playerId, existing);
      }
      return existing;
    }

    const connection = new RTCPeerConnection({
      iceServers: [{ urls: "stun:stun.l.google.com:19302" }],
    });
    const peer: PeerState = {
      connection,
      pendingIce: [],
      remoteStream: null,
    };

    connection.onicecandidate = (event) => {
      if (!event.candidate) {
        return;
      }
      this.send({
        type: "webrtc_signal",
        targetPlayerId: playerId,
        payload: {
          type: "ice_candidate",
          candidate: event.candidate.candidate,
          sdpMid: event.candidate.sdpMid,
          sdpMLineIndex: event.candidate.sdpMLineIndex,
        },
      });
    };

    connection.ontrack = (event) => {
      peer.remoteStream = event.streams[0] ?? null;
      this.emitChange();
    };

    connection.onconnectionstatechange = () => {
      if (connection.connectionState === "failed" || connection.connectionState === "closed" || connection.connectionState === "disconnected") {
        this.closePeer(playerId);
      }
    };

    if (this.localStream) {
      for (const track of this.localStream.getTracks()) {
        connection.addTrack(track, this.localStream);
      }
    }

    this.peers.set(playerId, peer);
    if (shouldOffer) {
      void this.maybeCreateOffer(playerId, peer);
    }

    return peer;
  }

  private async maybeCreateOffer(playerId: number, peer: PeerState) {
    if (!this.localStream || this.localPlayerId === null || this.localPlayerId > playerId) {
      return;
    }
    if (peer.connection.signalingState !== "stable") {
      return;
    }

    const offer = await peer.connection.createOffer();
    await peer.connection.setLocalDescription(offer);
    this.send({
      type: "webrtc_signal",
      targetPlayerId: playerId,
      payload: {
        type: "offer",
        sdp: offer.sdp ?? "",
      },
    });
  }

  private closePeer(playerId: number) {
    const peer = this.peers.get(playerId);
    if (!peer) {
      return;
    }
    peer.connection.close();
    this.peers.delete(playerId);
    this.emitChange();
  }

  private send(message: ClientMessage) {
    if (!this.ws || this.ws.readyState !== WebSocket.OPEN) {
      return;
    }
    this.ws.send(pack(message));
  }

  private forwardVector(): Vec3Tuple {
    const x = -Math.sin(this.yaw) * Math.cos(this.pitch);
    const y = Math.sin(this.pitch);
    const z = -Math.cos(this.yaw) * Math.cos(this.pitch);
    return [x, y, z];
  }

  private chunkKeyFor(position: ChunkPos) {
    return `${position.x},${position.z}`;
  }

  private bumpWorldRevision() {
    this.worldRevision += 1;
    this.emitChange();
  }

  private emitChange() {
    this.dispatchEvent(new Event("change"));
  }
}
