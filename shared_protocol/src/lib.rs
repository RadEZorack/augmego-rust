use serde::{Deserialize, Serialize};
use shared_math::{ChunkPos, WorldPos};
use shared_world::{BlockId, ChunkData, ChunkDelta};
use thiserror::Error;

pub const PROTOCOL_VERSION: u16 = 1;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClientHello {
    pub protocol_version: u16,
    pub client_name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServerHello {
    pub protocol_version: u16,
    pub motd: String,
    pub world_seed: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LoginRequest {
    pub name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LoginResponse {
    pub accepted: bool,
    pub player_id: u64,
    pub spawn_position: WorldPos,
    pub message: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubscribeChunks {
    pub center: ChunkPos,
    pub radius: u8,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChunkUnload {
    pub positions: Vec<ChunkPos>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlayerInputTick {
    pub tick: u64,
    pub movement: [f32; 3],
    pub jump: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlayerStateSnapshot {
    pub player_id: u64,
    pub tick: u64,
    pub position: [f32; 3],
    pub velocity: [f32; 3],
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlaceBlockRequest {
    pub position: WorldPos,
    pub block: BlockId,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BreakBlockRequest {
    pub position: WorldPos,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BlockActionResult {
    pub accepted: bool,
    pub reason: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InventoryStack {
    pub block: BlockId,
    pub count: u16,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InventorySnapshot {
    pub slots: Vec<InventoryStack>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatMessage {
    pub from: String,
    pub body: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ClientMessage {
    ClientHello(ClientHello),
    LoginRequest(LoginRequest),
    SubscribeChunks(SubscribeChunks),
    PlayerInputTick(PlayerInputTick),
    PlaceBlockRequest(PlaceBlockRequest),
    BreakBlockRequest(BreakBlockRequest),
    ChatMessage(ChatMessage),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ServerMessage {
    ServerHello(ServerHello),
    LoginResponse(LoginResponse),
    ChunkData(ChunkData),
    ChunkUnload(ChunkUnload),
    ChunkDelta(ChunkDelta),
    PlayerStateSnapshot(PlayerStateSnapshot),
    InventorySnapshot(InventorySnapshot),
    BlockActionResult(BlockActionResult),
    ChatMessage(ChatMessage),
}

#[derive(Debug, Error)]
pub enum ProtocolError {
    #[error("failed to serialize packet")]
    Serialize(#[from] Box<bincode::ErrorKind>),
    #[error("packet length {0} exceeds u32")]
    PacketTooLarge(usize),
}

pub fn encode<T: Serialize>(message: &T) -> Result<Vec<u8>, ProtocolError> {
    Ok(bincode::serialize(message)?)
}

pub fn decode<T: for<'de> Deserialize<'de>>(bytes: &[u8]) -> Result<T, ProtocolError> {
    Ok(bincode::deserialize(bytes)?)
}

pub fn frame<T: Serialize>(message: &T) -> Result<Vec<u8>, ProtocolError> {
    let payload = encode(message)?;
    let length = u32::try_from(payload.len()).map_err(|_| ProtocolError::PacketTooLarge(payload.len()))?;
    let mut framed = Vec::with_capacity(payload.len() + 4);
    framed.extend_from_slice(&length.to_le_bytes());
    framed.extend_from_slice(&payload);
    Ok(framed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn server_message_round_trip() {
        let message = ServerMessage::LoginResponse(LoginResponse {
            accepted: true,
            player_id: 7,
            spawn_position: WorldPos { x: 0, y: 72, z: 0 },
            message: "welcome".to_string(),
        });

        let bytes = encode(&message).unwrap();
        let decoded: ServerMessage = decode(&bytes).unwrap();

        match decoded {
            ServerMessage::LoginResponse(response) => {
                assert_eq!(response.player_id, 7);
                assert!(response.accepted);
            }
            _ => panic!("unexpected message variant"),
        }
    }
}
