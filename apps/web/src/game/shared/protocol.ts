import { AvatarSelection, BlockId } from "@/src/game/shared/content";
import { ChunkPos, WorldPos } from "@/src/game/shared/math";
import { ChunkData } from "@/src/game/shared/world";

export const PROTOCOL_VERSION = 1;

export type ClientHelloMessage = {
  type: "client_hello";
  protocolVersion: number;
  clientName: string;
};

export type LoginMessage = {
  type: "login";
  name: string;
  avatarSelection: AvatarSelection | null;
};

export type SubscribeChunksMessage = {
  type: "subscribe_chunks";
  center: ChunkPos;
  radius: number;
};

export type PlayerInputMessage = {
  type: "player_input";
  tick: number;
  movement: [number, number, number];
  position: [number, number, number] | null;
  velocity: [number, number, number] | null;
  yaw: number | null;
  jump: boolean;
};

export type PlaceBlockMessage = {
  type: "place_block";
  position: WorldPos;
  block: BlockId;
};

export type BreakBlockMessage = {
  type: "break_block";
  position: WorldPos;
};

export type ChatMessage = {
  type: "chat";
  body: string;
};

export type WebRtcSignalPayload =
  | {
      type: "offer";
      sdp: string;
    }
  | {
      type: "answer";
      sdp: string;
    }
  | {
      type: "ice_candidate";
      candidate: string;
      sdpMid: string | null;
      sdpMLineIndex: number | null;
    };

export type WebRtcSignalMessage = {
  type: "webrtc_signal";
  targetPlayerId: number;
  payload: WebRtcSignalPayload;
};

export type AvatarUpdateMessage = {
  type: "avatar_update";
  avatarSelection: AvatarSelection;
};

export type ClientMessage =
  | ClientHelloMessage
  | LoginMessage
  | SubscribeChunksMessage
  | PlayerInputMessage
  | PlaceBlockMessage
  | BreakBlockMessage
  | ChatMessage
  | WebRtcSignalMessage
  | AvatarUpdateMessage;

export type ServerHelloMessage = {
  type: "server_hello";
  protocolVersion: number;
  motd: string;
  worldSeed: number;
  defaultViewRadius: number;
};

export type LoginResponseMessage = {
  type: "login_response";
  accepted: boolean;
  playerId: number;
  spawnPosition: WorldPos;
  message: string;
};

export type ChunkDataMessage = {
  type: "chunk_data";
  chunk: ChunkData;
};

export type ChunkUnloadMessage = {
  type: "chunk_unload";
  positions: ChunkPos[];
};

export type PlayerStateSnapshotMessage = {
  type: "player_state_snapshot";
  playerId: number;
  tick: number;
  displayName: string;
  position: [number, number, number];
  velocity: [number, number, number];
  yaw: number;
  avatarSelection: AvatarSelection;
};

export type PlayerLeftMessage = {
  type: "player_left";
  playerId: number;
};

export type InventorySnapshotMessage = {
  type: "inventory_snapshot";
  slots: Array<{
    block: BlockId;
    count: number;
  }>;
};

export type BlockActionResultMessage = {
  type: "block_action_result";
  accepted: boolean;
  reason: string;
};

export type ServerChatMessage = {
  type: "chat";
  from: string;
  playerId: number;
  body: string;
};

export type ServerWebRtcSignalMessage = {
  type: "webrtc_signal";
  sourcePlayerId: number;
  payload: WebRtcSignalPayload;
};

export type ServerMessage =
  | ServerHelloMessage
  | LoginResponseMessage
  | ChunkDataMessage
  | ChunkUnloadMessage
  | PlayerStateSnapshotMessage
  | PlayerLeftMessage
  | InventorySnapshotMessage
  | BlockActionResultMessage
  | ServerChatMessage
  | ServerWebRtcSignalMessage;
