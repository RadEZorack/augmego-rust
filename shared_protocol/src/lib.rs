use serde::{Deserialize, Serialize};
use shared_math::{ChunkPos, WorldPos};
use shared_world::{BlockId, ChunkData, ChunkDelta};
use thiserror::Error;

pub const PROTOCOL_VERSION: u16 = 13;

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
    pub idle_model_url: Option<String>,
    pub run_model_url: Option<String>,
    pub dance_model_url: Option<String>,
    pub auth_token: Option<String>,
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
    pub client_sent_at_ms: Option<u64>,
    pub movement: [f32; 3],
    pub position: Option<[f32; 3]>,
    pub velocity: Option<[f32; 3]>,
    pub yaw: Option<f32>,
    pub jump: bool,
    pub pet_states: Vec<PetStateSnapshot>,
    pub wild_pet_states: Vec<WildPetMotionSnapshot>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PetStateSnapshot {
    pub position: [f32; 3],
    pub yaw: f32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PeerRealtimeState {
    pub tick: u64,
    pub position: [f32; 3],
    pub velocity: [f32; 3],
    pub yaw: f32,
    pub pet_states: Vec<PetStateSnapshot>,
    pub wild_pet_states: Vec<WildPetMotionSnapshot>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WildPetMotionSnapshot {
    pub pet_id: u64,
    pub position: [f32; 3],
    pub velocity: [f32; 3],
    pub yaw: f32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WildPetSnapshot {
    pub pet_id: u64,
    pub tick: u64,
    pub spawn_position: [f32; 3],
    pub position: [f32; 3],
    pub velocity: [f32; 3],
    pub yaw: f32,
    pub host_player_id: Option<u64>,
    pub pet_identity: PetIdentity,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WildPetUnload {
    pub pet_ids: Vec<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PetIdentity {
    pub id: String,
    pub display_name: String,
    pub model_url: Option<String>,
    pub equipped_weapon: Option<WeaponIdentity>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WeaponIdentity {
    pub id: String,
    pub kind: String,
    pub display_name: String,
    pub model_url: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CapturedPet {
    pub id: String,
    pub display_name: String,
    pub model_url: Option<String>,
    pub captured_at_ms: Option<u64>,
    pub active: bool,
    pub equipped_weapon_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CapturedPetsSnapshot {
    pub pets: Vec<CapturedPet>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CollectedWeapon {
    pub id: String,
    pub kind: String,
    pub display_name: String,
    pub model_url: Option<String>,
    pub collected_at_ms: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CollectedWeaponsSnapshot {
    pub weapons: Vec<CollectedWeapon>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorldWeaponSnapshot {
    pub weapon_id: u64,
    pub tick: u64,
    pub position: [f32; 3],
    pub weapon_identity: WeaponIdentity,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PetWeaponShot {
    pub tick: u64,
    pub shooter_player_id: u64,
    pub origin: [f32; 3],
    pub target: [f32; 3],
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorldWeaponUnload {
    pub weapon_ids: Vec<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum PickupWorldWeaponStatus {
    Collected,
    AlreadyTaken,
    OutOfRange,
    NotFound,
    Failed,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PickupWorldWeaponResult {
    pub weapon_id: u64,
    pub status: PickupWorldWeaponStatus,
    pub message: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum CaptureWildPetStatus {
    Captured,
    SignInRequired,
    AlreadyTaken,
    OutOfRange,
    NotFound,
    Failed,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CaptureWildPetResult {
    pub pet_id: u64,
    pub status: CaptureWildPetStatus,
    pub message: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PetWeaponAssignment {
    pub pet_id: String,
    pub weapon_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdatePetPartyRequest {
    pub active_pet_ids: Vec<String>,
    pub equipped_weapon_assignments: Vec<PetWeaponAssignment>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UpdatePetPartyResult {
    pub accepted: bool,
    pub message: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlayerStateSnapshot {
    pub player_id: u64,
    pub tick: u64,
    pub position: [f32; 3],
    pub velocity: [f32; 3],
    pub yaw: f32,
    pub idle_model_url: Option<String>,
    pub run_model_url: Option<String>,
    pub dance_model_url: Option<String>,
    pub pet_states: Vec<PetStateSnapshot>,
    pub active_pet_models: Vec<PetIdentity>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlayerLeft {
    pub player_id: u64,
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
pub enum WebRtcSignalPayload {
    Offer {
        sdp: String,
    },
    Answer {
        sdp: String,
    },
    IceCandidate {
        candidate: String,
        sdp_mid: Option<String>,
        sdp_mline_index: Option<u16>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClientWebRtcSignal {
    pub target_player_id: u64,
    pub payload: WebRtcSignalPayload,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServerWebRtcSignal {
    pub source_player_id: u64,
    pub payload: WebRtcSignalPayload,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ClientMessage {
    ClientHello(ClientHello),
    LoginRequest(LoginRequest),
    SubscribeChunks(SubscribeChunks),
    PlayerInputTick(PlayerInputTick),
    CaptureWildPetRequest { pet_id: u64 },
    PickupWorldWeaponRequest { weapon_id: u64 },
    UpdatePetPartyRequest(UpdatePetPartyRequest),
    PlaceBlockRequest(PlaceBlockRequest),
    BreakBlockRequest(BreakBlockRequest),
    ChatMessage(ChatMessage),
    WebRtcSignal(ClientWebRtcSignal),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ServerMessage {
    ServerHello(ServerHello),
    LoginResponse(LoginResponse),
    ChunkData(ChunkData),
    ChunkUnload(ChunkUnload),
    ChunkDelta(ChunkDelta),
    PlayerStateSnapshot(PlayerStateSnapshot),
    PlayerLeft(PlayerLeft),
    WildPetSnapshot(WildPetSnapshot),
    WildPetUnload(WildPetUnload),
    WorldWeaponSnapshot(WorldWeaponSnapshot),
    WorldWeaponUnload(WorldWeaponUnload),
    PetWeaponShot(PetWeaponShot),
    CapturedPetsSnapshot(CapturedPetsSnapshot),
    CollectedWeaponsSnapshot(CollectedWeaponsSnapshot),
    CaptureWildPetResult(CaptureWildPetResult),
    PickupWorldWeaponResult(PickupWorldWeaponResult),
    UpdatePetPartyResult(UpdatePetPartyResult),
    InventorySnapshot(InventorySnapshot),
    BlockActionResult(BlockActionResult),
    ChatMessage(ChatMessage),
    WebRtcSignal(ServerWebRtcSignal),
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
    let length =
        u32::try_from(payload.len()).map_err(|_| ProtocolError::PacketTooLarge(payload.len()))?;
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

    #[test]
    fn pet_party_message_round_trip() {
        let request = ClientMessage::UpdatePetPartyRequest(UpdatePetPartyRequest {
            active_pet_ids: vec!["pet-a".to_string(), "pet-b".to_string()],
            equipped_weapon_assignments: vec![PetWeaponAssignment {
                pet_id: "pet-a".to_string(),
                weapon_id: Some("weapon-a".to_string()),
            }],
        });
        let request_bytes = encode(&request).unwrap();
        let decoded_request: ClientMessage = decode(&request_bytes).unwrap();
        match decoded_request {
            ClientMessage::UpdatePetPartyRequest(request) => {
                assert_eq!(request.active_pet_ids, vec!["pet-a", "pet-b"]);
                assert_eq!(request.equipped_weapon_assignments.len(), 1);
                assert_eq!(request.equipped_weapon_assignments[0].pet_id, "pet-a");
                assert_eq!(
                    request.equipped_weapon_assignments[0].weapon_id.as_deref(),
                    Some("weapon-a")
                );
            }
            _ => panic!("unexpected client message variant"),
        }

        let response = ServerMessage::UpdatePetPartyResult(UpdatePetPartyResult {
            accepted: true,
            message: "Pet party updated.".to_string(),
        });
        let response_bytes = encode(&response).unwrap();
        let decoded_response: ServerMessage = decode(&response_bytes).unwrap();
        match decoded_response {
            ServerMessage::UpdatePetPartyResult(result) => {
                assert!(result.accepted);
                assert_eq!(result.message, "Pet party updated.");
            }
            _ => panic!("unexpected server message variant"),
        }
    }

    #[test]
    fn weapon_pickup_message_round_trip() {
        let request = ClientMessage::PickupWorldWeaponRequest { weapon_id: 42 };
        let request_bytes = encode(&request).unwrap();
        let decoded_request: ClientMessage = decode(&request_bytes).unwrap();
        match decoded_request {
            ClientMessage::PickupWorldWeaponRequest { weapon_id } => {
                assert_eq!(weapon_id, 42);
            }
            _ => panic!("unexpected client message variant"),
        }

        let response = ServerMessage::PickupWorldWeaponResult(PickupWorldWeaponResult {
            weapon_id: 42,
            status: PickupWorldWeaponStatus::Collected,
            message: "Weapon collected.".to_string(),
        });
        let response_bytes = encode(&response).unwrap();
        let decoded_response: ServerMessage = decode(&response_bytes).unwrap();
        match decoded_response {
            ServerMessage::PickupWorldWeaponResult(result) => {
                assert_eq!(result.weapon_id, 42);
                assert!(matches!(result.status, PickupWorldWeaponStatus::Collected));
            }
            _ => panic!("unexpected server message variant"),
        }
    }

    #[test]
    fn pet_weapon_shot_message_round_trip() {
        let response = ServerMessage::PetWeaponShot(PetWeaponShot {
            tick: 99,
            shooter_player_id: 7,
            origin: [1.0, 2.0, 3.0],
            target: [4.0, 5.0, 6.0],
        });
        let response_bytes = encode(&response).unwrap();
        let decoded_response: ServerMessage = decode(&response_bytes).unwrap();
        match decoded_response {
            ServerMessage::PetWeaponShot(shot) => {
                assert_eq!(shot.tick, 99);
                assert_eq!(shot.shooter_player_id, 7);
                assert_eq!(shot.origin, [1.0, 2.0, 3.0]);
                assert_eq!(shot.target, [4.0, 5.0, 6.0]);
            }
            _ => panic!("unexpected server message variant"),
        }
    }
}
