use anyhow::{Context, Result};
use jsonwebtoken::{Algorithm, DecodingKey, Validation, decode};
use reqwest::Client;
use serde::Deserialize;
use shared_protocol::{CapturedPet, PetIdentity};

#[derive(Clone, Debug)]
pub struct PlayerPetCollection {
    pub pets: Vec<CapturedPet>,
    pub active_pets: Vec<PetIdentity>,
}

#[derive(Clone)]
pub struct PetRegistryClient {
    client: Client,
    base_url: String,
    service_token: String,
    auth_secret: String,
}

#[derive(Debug, Deserialize)]
struct GameAuthClaims {
    sub: String,
    #[serde(rename = "exp")]
    _exp: usize,
}

#[derive(Debug, Deserialize)]
struct ReservePetResponse {
    pet: Option<PetIdentity>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct UserPetCollectionResponse {
    pets: Vec<CapturedPet>,
    active_pets: Vec<PetIdentity>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
enum CapturePetCode {
    Captured,
    NotFound,
    AlreadyTaken,
    NotSpawned,
}

#[derive(Debug, Deserialize)]
struct CapturePetResponse {
    code: CapturePetCode,
    collection: Option<UserPetCollectionResponse>,
}

impl PetRegistryClient {
    pub fn new(base_url: String, service_token: String, auth_secret: String) -> Self {
        Self {
            client: Client::new(),
            base_url: base_url.trim_end_matches('/').to_string(),
            service_token,
            auth_secret,
        }
    }

    pub fn verify_auth_token(&self, token: &str) -> Option<String> {
        let mut validation = Validation::new(Algorithm::HS256);
        validation.validate_exp = true;
        let claims = decode::<GameAuthClaims>(
            token,
            &DecodingKey::from_secret(self.auth_secret.as_bytes()),
            &validation,
        )
        .ok()?;
        Some(claims.claims.sub)
    }

    pub async fn reset_spawned_pets(&self) -> Result<usize> {
        #[derive(Debug, Deserialize)]
        struct ResetResponse {
            #[allow(dead_code)]
            ok: bool,
            #[serde(rename = "resetCount")]
            reset_count: usize,
        }

        let response = self
            .client
            .post(format!("{}/internal/pets/reset-spawned", self.base_url))
            .header("x-augmego-service-token", &self.service_token)
            .send()
            .await
            .context("send reset spawned pets request")?
            .error_for_status()
            .context("reset spawned pets returned error")?;
        let payload = response
            .json::<ResetResponse>()
            .await
            .context("decode reset spawned pets response")?;
        Ok(payload.reset_count)
    }

    pub async fn reserve_pet(&self) -> Result<Option<PetIdentity>> {
        let response = self
            .client
            .post(format!("{}/internal/pets/reserve", self.base_url))
            .header("x-augmego-service-token", &self.service_token)
            .send()
            .await
            .context("send reserve pet request")?
            .error_for_status()
            .context("reserve pet returned error")?;
        let payload = response
            .json::<ReservePetResponse>()
            .await
            .context("decode reserve pet response")?;
        Ok(payload.pet)
    }

    pub async fn load_user_pet_collection(&self, user_id: &str) -> Result<PlayerPetCollection> {
        let response = self
            .client
            .get(format!("{}/internal/users/{user_id}/pets", self.base_url))
            .header("x-augmego-service-token", &self.service_token)
            .send()
            .await
            .context("send load user pets request")?
            .error_for_status()
            .context("load user pets returned error")?;
        let payload = response
            .json::<UserPetCollectionResponse>()
            .await
            .context("decode load user pets response")?;
        Ok(PlayerPetCollection {
            pets: payload.pets,
            active_pets: payload.active_pets,
        })
    }

    pub async fn capture_pet(
        &self,
        pet_id: &str,
        user_id: &str,
    ) -> Result<CapturePetOutcome> {
        let response = self
            .client
            .post(format!("{}/internal/pets/{pet_id}/capture", self.base_url))
            .header("x-augmego-service-token", &self.service_token)
            .json(&serde_json::json!({ "userId": user_id }))
            .send()
            .await
            .context("send capture pet request")?
            .error_for_status()
            .context("capture pet returned error")?;
        let payload = response
            .json::<CapturePetResponse>()
            .await
            .context("decode capture pet response")?;
        Ok(match payload.code {
            CapturePetCode::Captured => {
                let collection = payload
                    .collection
                    .context("capture response missing collection")?;
                CapturePetOutcome::Captured(PlayerPetCollection {
                    pets: collection.pets,
                    active_pets: collection.active_pets,
                })
            }
            CapturePetCode::AlreadyTaken => CapturePetOutcome::AlreadyTaken,
            CapturePetCode::NotFound => CapturePetOutcome::NotFound,
            CapturePetCode::NotSpawned => CapturePetOutcome::NotSpawned,
        })
    }
}

pub enum CapturePetOutcome {
    Captured(PlayerPetCollection),
    AlreadyTaken,
    NotFound,
    NotSpawned,
}
