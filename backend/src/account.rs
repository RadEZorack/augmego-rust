use crate::auth::{
    SameSitePolicy, SessionCookieConfig, clear_cookie, make_cookie, parse_cookie,
    sign_game_auth_token,
};
use crate::storage::{StorageObject, StorageService};
use anyhow::{Context, Result, anyhow};
use chrono::{Duration as ChronoDuration, Utc};
use jsonwebtoken::{Algorithm, DecodingKey, Validation, decode, decode_header};
use reqwest::{Client, Url};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::{PgPool, Row, types::Json};
use std::net::IpAddr;
use std::path::Path;
use std::time::Duration;
use uuid::Uuid;

const APPLE_AUTH_URL: &str = "https://appleid.apple.com/auth/authorize";
const APPLE_KEYS_URL: &str = "https://appleid.apple.com/auth/keys";
const APPLE_ISSUER: &str = "https://appleid.apple.com";
const APPLE_SCOPE_DEFAULT: &str = "name email";
const APPLE_STATE_COOKIE_NAME: &str = "oauth_state_apple";
const APPLE_NONCE_COOKIE_NAME: &str = "oauth_nonce_apple";
const GOOGLE_AUTH_URL: &str = "https://accounts.google.com/o/oauth2/v2/auth";
const GOOGLE_TOKEN_URL: &str = "https://oauth2.googleapis.com/token";
const GOOGLE_USERINFO_URL: &str = "https://openidconnect.googleapis.com/v1/userinfo";
const GOOGLE_STATE_COOKIE_NAME: &str = "oauth_state_google";
const MICROSOFT_DEFAULT_TENANT: &str = "common";
const MICROSOFT_SCOPE_DEFAULT: &str = "openid profile email";
const MICROSOFT_STATE_COOKIE_NAME: &str = "oauth_state_microsoft";
const PLAYER_AVATAR_CACHE_CONTROL: &str = "public, max-age=31536000, immutable";
const AVATAR_CUSTOMIZER_BETA_ENABLED: bool = true;

#[derive(Clone, Debug)]
pub struct AccountConfig {
    pub public_base_url: String,
    pub session_cookie: SessionCookieConfig,
    pub apple_client_id: String,
    pub apple_scope: String,
    pub google_client_id: String,
    pub google_client_secret: String,
    pub google_scope: String,
    pub microsoft_client_id: String,
    pub microsoft_client_secret: String,
    pub microsoft_scope: String,
    pub microsoft_tenant: String,
    pub game_auth_secret: String,
    pub game_auth_ttl: Duration,
}

#[derive(Clone)]
pub struct AccountService {
    pool: PgPool,
    storage: StorageService,
    config: AccountConfig,
    http: Client,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AvatarSelection {
    pub stationary_model_url: Option<String>,
    pub move_model_url: Option<String>,
    pub special_model_url: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AuthUserResponse {
    pub id: String,
    pub name: Option<String>,
    pub email: Option<String>,
    pub avatar_url: Option<String>,
    pub avatar_selection: AvatarSelection,
    pub avatar_customizer_access: AvatarCustomizerAccess,
    pub avatar_generation_defaults: AvatarStyleOptions,
    pub game_auth_token: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct AvatarStyleOptions {
    pub outfit_style: AvatarOutfitStyle,
    pub fit_style: AvatarFitStyle,
    pub neck_accessory: AvatarNeckAccessory,
    pub headwear: AvatarHeadwear,
    pub body_build: AvatarBodyBuild,
    pub skin_tone: AvatarSkinTone,
    pub pose_energy: AvatarPoseEnergy,
    pub material_style: AvatarMaterialStyle,
    pub ring: bool,
    pub earrings: bool,
    pub bracelet: bool,
    pub watch: bool,
    pub gloves: bool,
    pub cape: bool,
}

impl Default for AvatarStyleOptions {
    fn default() -> Self {
        Self {
            outfit_style: AvatarOutfitStyle::MatchReference,
            fit_style: AvatarFitStyle::MatchReference,
            neck_accessory: AvatarNeckAccessory::None,
            headwear: AvatarHeadwear::None,
            body_build: AvatarBodyBuild::MatchReference,
            skin_tone: AvatarSkinTone::MatchReference,
            pose_energy: AvatarPoseEnergy::Neutral,
            material_style: AvatarMaterialStyle::Natural,
            ring: false,
            earrings: false,
            bracelet: false,
            watch: false,
            gloves: false,
            cape: false,
        }
    }
}

impl AvatarStyleOptions {
    pub fn is_default(&self) -> bool {
        self == &Self::default()
    }
}

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AvatarOutfitStyle {
    #[default]
    MatchReference,
    Suit,
    Dress,
    Casual,
    Streetwear,
    Fantasy,
    Armor,
}

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AvatarFitStyle {
    #[default]
    MatchReference,
    Tailored,
    Relaxed,
    Oversized,
}

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AvatarNeckAccessory {
    #[default]
    None,
    Tie,
    BowTie,
    Necklace,
    Scarf,
}

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AvatarHeadwear {
    #[default]
    None,
    Hat,
    Beanie,
    Hood,
    Crown,
}

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AvatarBodyBuild {
    #[default]
    MatchReference,
    Slim,
    Average,
    Broad,
    Plus,
}

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AvatarSkinTone {
    #[default]
    MatchReference,
    Fair,
    Light,
    Medium,
    Tan,
    Deep,
}

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AvatarPoseEnergy {
    #[default]
    Neutral,
    Confident,
    Playful,
    Heroic,
}

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AvatarMaterialStyle {
    #[default]
    Natural,
    Glam,
    SoftFabric,
    FormalLuxury,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AvatarCustomizerAccessSource {
    Beta,
    Entitlement,
    None,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct AvatarCustomizerAccess {
    pub enabled: bool,
    pub source: AvatarCustomizerAccessSource,
}

#[derive(Clone, Debug)]
pub struct SessionUser {
    pub id: Uuid,
    pub name: Option<String>,
    pub email: Option<String>,
    pub avatar_url: Option<String>,
}

#[derive(Clone, Debug)]
pub struct OAuthSigninStart {
    pub redirect_url: String,
    pub set_cookies: Vec<String>,
}

#[derive(Clone, Debug)]
pub struct OAuthCallbackResult {
    pub redirect_url: String,
    pub session_cookie: String,
    pub clear_cookies: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct AppleAuthorizeUser {
    email: Option<String>,
    name: Option<AppleAuthorizeUserName>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct AppleAuthorizeUserName {
    first_name: Option<String>,
    last_name: Option<String>,
}

#[derive(Debug, Deserialize)]
struct AppleKeySet {
    keys: Vec<AppleJsonWebKey>,
}

#[derive(Debug, Deserialize)]
struct AppleJsonWebKey {
    kid: String,
    n: String,
    e: String,
}

#[derive(Debug, Deserialize)]
struct AppleIdTokenClaims {
    sub: String,
    email: Option<String>,
    nonce: Option<String>,
}

#[derive(Debug, Deserialize)]
struct MicrosoftOpenIdConfiguration {
    userinfo_endpoint: String,
}

#[derive(Debug, Deserialize)]
struct MicrosoftUserInfo {
    sub: String,
    name: Option<String>,
    email: Option<String>,
    picture: Option<String>,
}

pub enum AvatarFileResponse {
    Redirect { url: String },
    Bytes(StorageObject),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PlayerAvatarSlot {
    Idle,
    Run,
    Dance,
}

impl PlayerAvatarSlot {
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "idle" => Some(Self::Idle),
            "run" => Some(Self::Run),
            "dance" => Some(Self::Dance),
            _ => None,
        }
    }

    pub fn as_db_value(self) -> &'static str {
        match self {
            Self::Idle => "IDLE",
            Self::Run => "RUN",
            Self::Dance => "DANCE",
        }
    }

    pub fn as_path_value(self) -> &'static str {
        match self {
            Self::Idle => "idle",
            Self::Run => "run",
            Self::Dance => "dance",
        }
    }
}

impl AccountService {
    pub fn new(pool: PgPool, storage: StorageService, config: AccountConfig) -> Self {
        Self {
            pool,
            storage,
            config,
            http: Client::new(),
        }
    }

    pub async fn auth_user_from_cookie_header(
        &self,
        cookie_header: Option<&str>,
    ) -> Result<Option<AuthUserResponse>> {
        let Some(user) = self.session_user_from_cookie_header(cookie_header).await? else {
            return Ok(None);
        };
        Ok(Some(self.build_auth_user(user).await?))
    }

    pub async fn session_user_from_cookie_header(
        &self,
        cookie_header: Option<&str>,
    ) -> Result<Option<SessionUser>> {
        let Some(session_id) = parse_cookie(cookie_header, &self.config.session_cookie.name) else {
            return Ok(None);
        };
        let session_id = Uuid::parse_str(&session_id).context("parse session id cookie")?;
        let row = sqlx::query(
            "SELECT users.id, users.name, users.email, users.avatar_url
             FROM sessions
             JOIN users ON users.id = sessions.user_id
             WHERE sessions.id = $1
               AND sessions.revoked_at IS NULL
               AND sessions.expires_at > NOW()",
        )
        .bind(session_id)
        .fetch_optional(&self.pool)
        .await
        .context("load session user")?;

        let Some(row) = row else {
            return Ok(None);
        };

        Ok(Some(SessionUser {
            id: row.try_get("id")?,
            name: row.try_get("name")?,
            email: row.try_get("email")?,
            avatar_url: row.try_get("avatar_url")?,
        }))
    }

    pub fn start_google_signin(&self) -> Result<OAuthSigninStart> {
        if self.config.google_client_id.trim().is_empty() {
            return Err(anyhow!("Google OAuth is not configured"));
        }

        let state = Uuid::new_v4().to_string();
        let redirect_uri = self.google_redirect_uri();
        let query = [
            ("response_type", "code".to_string()),
            ("client_id", self.config.google_client_id.clone()),
            ("redirect_uri", redirect_uri),
            ("scope", self.config.google_scope.clone()),
            ("state", state.clone()),
        ]
        .into_iter()
        .map(|(key, value)| format!("{key}={}", url_encode(&value)))
        .collect::<Vec<_>>()
        .join("&");

        Ok(OAuthSigninStart {
            redirect_url: format!("{GOOGLE_AUTH_URL}?{query}"),
            set_cookies: vec![make_cookie(
                GOOGLE_STATE_COOKIE_NAME,
                &state,
                &self.config.session_cookie,
                Some(Duration::from_secs(60 * 15)),
            )],
        })
    }

    pub fn start_microsoft_signin(&self) -> Result<OAuthSigninStart> {
        if self.config.microsoft_client_id.trim().is_empty() {
            return Err(anyhow!("Microsoft OAuth is not configured"));
        }

        let state = Uuid::new_v4().to_string();
        let query = [
            ("client_id", self.config.microsoft_client_id.clone()),
            ("response_type", "code".to_string()),
            ("redirect_uri", self.microsoft_redirect_uri()),
            ("response_mode", "query".to_string()),
            ("scope", self.microsoft_scope().to_string()),
            ("state", state.clone()),
        ]
        .into_iter()
        .map(|(key, value)| format!("{key}={}", url_encode(&value)))
        .collect::<Vec<_>>()
        .join("&");

        Ok(OAuthSigninStart {
            redirect_url: format!("{}?{query}", self.microsoft_authorize_url()),
            set_cookies: vec![make_cookie(
                MICROSOFT_STATE_COOKIE_NAME,
                &state,
                &self.config.session_cookie,
                Some(Duration::from_secs(60 * 15)),
            )],
        })
    }

    pub fn start_apple_signin(&self) -> Result<OAuthSigninStart> {
        if self.config.apple_client_id.trim().is_empty() {
            return Err(anyhow!("Apple OAuth is not configured"));
        }

        let cookie_config = self.apple_oauth_cookie_config()?;
        let state = Uuid::new_v4().to_string();
        let nonce = Uuid::new_v4().to_string();
        let mut query = vec![
            ("response_type", "code id_token".to_string()),
            ("response_mode", "form_post".to_string()),
            ("client_id", self.config.apple_client_id.clone()),
            ("redirect_uri", self.apple_redirect_uri()),
            ("state", state.clone()),
            ("nonce", nonce.clone()),
        ];
        let scope = self.config.apple_scope.trim();
        query.push((
            "scope",
            if scope.is_empty() {
                APPLE_SCOPE_DEFAULT
            } else {
                scope
            }
            .to_string(),
        ));

        let query = query
            .into_iter()
            .map(|(key, value)| format!("{key}={}", url_encode(&value)))
            .collect::<Vec<_>>()
            .join("&");

        Ok(OAuthSigninStart {
            redirect_url: format!("{APPLE_AUTH_URL}?{query}"),
            set_cookies: vec![
                make_cookie(
                    APPLE_STATE_COOKIE_NAME,
                    &state,
                    &cookie_config,
                    Some(Duration::from_secs(60 * 15)),
                ),
                make_cookie(
                    APPLE_NONCE_COOKIE_NAME,
                    &nonce,
                    &cookie_config,
                    Some(Duration::from_secs(60 * 15)),
                ),
            ],
        })
    }

    pub async fn handle_google_callback(
        &self,
        code: &str,
        state: &str,
        cookie_header: Option<&str>,
    ) -> Result<OAuthCallbackResult> {
        let expected_state = parse_cookie(cookie_header, GOOGLE_STATE_COOKIE_NAME)
            .ok_or_else(|| anyhow!("missing oauth state cookie"))?;
        if expected_state != state {
            return Err(anyhow!("invalid oauth state"));
        }
        if self.config.google_client_id.trim().is_empty()
            || self.config.google_client_secret.trim().is_empty()
        {
            return Err(anyhow!("Google OAuth is not configured"));
        }
        let redirect_uri = self.google_redirect_uri();

        let token_response = self
            .http
            .post(GOOGLE_TOKEN_URL)
            .form(&[
                ("grant_type", "authorization_code"),
                ("code", code),
                ("redirect_uri", redirect_uri.as_str()),
                ("client_id", self.config.google_client_id.as_str()),
                ("client_secret", self.config.google_client_secret.as_str()),
            ])
            .send()
            .await
            .context("exchange google auth code")?;
        if !token_response.status().is_success() {
            let status = token_response.status();
            let text = token_response.text().await.unwrap_or_default();
            anyhow::bail!("Google token exchange failed ({status}): {text}");
        }

        let token_body = token_response
            .json::<Value>()
            .await
            .context("decode google token response")?;
        let access_token = token_body
            .get("access_token")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow!("missing google access token"))?;

        let profile_response = self
            .http
            .get(GOOGLE_USERINFO_URL)
            .bearer_auth(access_token)
            .send()
            .await
            .context("fetch google user info")?;
        if !profile_response.status().is_success() {
            let status = profile_response.status();
            let text = profile_response.text().await.unwrap_or_default();
            anyhow::bail!("Google userinfo failed ({status}): {text}");
        }

        let profile = profile_response
            .json::<Value>()
            .await
            .context("decode google user info")?;
        let google_subject = profile
            .get("sub")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow!("missing google subject"))?;
        let email = profile
            .get("email")
            .and_then(Value::as_str)
            .map(str::to_string);
        let name = profile
            .get("name")
            .and_then(Value::as_str)
            .map(str::to_string);
        let avatar_url = profile
            .get("picture")
            .and_then(Value::as_str)
            .map(str::to_string);

        let user_id = self
            .upsert_oauth_user(
                "google",
                "Google",
                google_subject,
                email.as_deref(),
                name.as_deref(),
                avatar_url.as_deref(),
            )
            .await?;
        self.create_oauth_session(
            user_id,
            vec![clear_cookie(
                GOOGLE_STATE_COOKIE_NAME,
                &self.config.session_cookie,
            )],
        )
        .await
    }

    pub async fn handle_apple_callback(
        &self,
        id_token: &str,
        state: &str,
        user_payload: Option<&str>,
        cookie_header: Option<&str>,
    ) -> Result<OAuthCallbackResult> {
        let cookie_config = self.apple_oauth_cookie_config()?;
        let expected_state = parse_cookie(cookie_header, APPLE_STATE_COOKIE_NAME)
            .ok_or_else(|| anyhow!("missing apple oauth state cookie"))?;
        if expected_state != state {
            return Err(anyhow!("invalid apple oauth state"));
        }
        let expected_nonce = parse_cookie(cookie_header, APPLE_NONCE_COOKIE_NAME)
            .ok_or_else(|| anyhow!("missing apple oauth nonce cookie"))?;
        let claims = self
            .validate_apple_id_token(id_token, &expected_nonce)
            .await?;
        let parsed_user = match user_payload
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            Some(payload) => Some(
                serde_json::from_str::<AppleAuthorizeUser>(payload)
                    .context("decode apple user payload")?,
            ),
            None => None,
        };
        let email = claims
            .email
            .as_deref()
            .or_else(|| parsed_user.as_ref().and_then(|user| user.email.as_deref()));
        let name = parsed_user
            .as_ref()
            .and_then(|user| user.name.as_ref())
            .and_then(apple_user_display_name);

        let user_id = self
            .upsert_oauth_user("apple", "Apple", &claims.sub, email, name.as_deref(), None)
            .await?;
        self.create_oauth_session(
            user_id,
            vec![
                clear_cookie(APPLE_STATE_COOKIE_NAME, &cookie_config),
                clear_cookie(APPLE_NONCE_COOKIE_NAME, &cookie_config),
            ],
        )
        .await
    }

    pub async fn handle_microsoft_callback(
        &self,
        code: &str,
        state: &str,
        cookie_header: Option<&str>,
    ) -> Result<OAuthCallbackResult> {
        let expected_state = parse_cookie(cookie_header, MICROSOFT_STATE_COOKIE_NAME)
            .ok_or_else(|| anyhow!("missing microsoft oauth state cookie"))?;
        if expected_state != state {
            return Err(anyhow!("invalid microsoft oauth state"));
        }
        if self.config.microsoft_client_id.trim().is_empty()
            || self.config.microsoft_client_secret.trim().is_empty()
        {
            return Err(anyhow!("Microsoft OAuth is not configured"));
        }

        let scope = self.microsoft_scope().to_string();
        let redirect_uri = self.microsoft_redirect_uri();
        let token_response = self
            .http
            .post(self.microsoft_token_url())
            .form(&vec![
                ("grant_type".to_string(), "authorization_code".to_string()),
                ("code".to_string(), code.to_string()),
                ("redirect_uri".to_string(), redirect_uri),
                (
                    "client_id".to_string(),
                    self.config.microsoft_client_id.clone(),
                ),
                (
                    "client_secret".to_string(),
                    self.config.microsoft_client_secret.clone(),
                ),
                ("scope".to_string(), scope),
            ])
            .send()
            .await
            .context("exchange microsoft auth code")?;
        if !token_response.status().is_success() {
            let status = token_response.status();
            let text = token_response.text().await.unwrap_or_default();
            anyhow::bail!("Microsoft token exchange failed ({status}): {text}");
        }

        let token_body = token_response
            .json::<Value>()
            .await
            .context("decode microsoft token response")?;
        let access_token = token_body
            .get("access_token")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow!("missing microsoft access token"))?;

        let openid_config = self.load_microsoft_openid_configuration().await?;
        let profile_response = self
            .http
            .get(&openid_config.userinfo_endpoint)
            .bearer_auth(access_token)
            .send()
            .await
            .context("fetch microsoft user info")?;
        if !profile_response.status().is_success() {
            let status = profile_response.status();
            let text = profile_response.text().await.unwrap_or_default();
            anyhow::bail!("Microsoft userinfo failed ({status}): {text}");
        }

        let profile = profile_response
            .json::<MicrosoftUserInfo>()
            .await
            .context("decode microsoft user info")?;
        let user_id = self
            .upsert_oauth_user(
                "microsoft",
                "Microsoft",
                &profile.sub,
                profile.email.as_deref(),
                profile.name.as_deref(),
                profile.picture.as_deref(),
            )
            .await?;
        self.create_oauth_session(
            user_id,
            vec![clear_cookie(
                MICROSOFT_STATE_COOKIE_NAME,
                &self.config.session_cookie,
            )],
        )
        .await
    }

    pub fn logout_cookie(&self) -> String {
        clear_cookie(
            &self.config.session_cookie.name,
            &self.config.session_cookie,
        )
    }

    pub async fn revoke_session(&self, cookie_header: Option<&str>) -> Result<()> {
        let Some(session_id) = parse_cookie(cookie_header, &self.config.session_cookie.name) else {
            return Ok(());
        };
        let session_id = Uuid::parse_str(&session_id).context("parse session id for logout")?;
        sqlx::query(
            "UPDATE sessions
             SET revoked_at = NOW()
             WHERE id = $1 AND revoked_at IS NULL",
        )
        .bind(session_id)
        .execute(&self.pool)
        .await
        .context("revoke session")?;
        Ok(())
    }

    pub async fn update_profile(
        &self,
        user_id: Uuid,
        name: Option<Option<String>>,
        avatar_url: Option<Option<String>>,
    ) -> Result<()> {
        sqlx::query(
            "UPDATE users
             SET name = CASE WHEN $2 THEN $3 ELSE name END,
                 avatar_url = CASE WHEN $4 THEN $5 ELSE avatar_url END,
                 updated_at = NOW()
             WHERE id = $1",
        )
        .bind(user_id)
        .bind(name.is_some())
        .bind(name.flatten())
        .bind(avatar_url.is_some())
        .bind(avatar_url.flatten())
        .execute(&self.pool)
        .await
        .context("update profile")?;
        Ok(())
    }

    pub async fn load_avatar_selection(&self, user_id: Uuid) -> Result<AvatarSelection> {
        let rows = sqlx::query(
            "SELECT slot, model_url, storage_key
             FROM avatar_slots
             WHERE user_id = $1",
        )
        .bind(user_id)
        .fetch_all(&self.pool)
        .await
        .context("load avatar slots")?;

        let mut selection = AvatarSelection {
            stationary_model_url: None,
            move_model_url: None,
            special_model_url: None,
        };

        for row in rows {
            let slot: String = row.try_get("slot")?;
            let model_url: Option<String> = row.try_get("model_url")?;
            let storage_key: Option<String> = row.try_get("storage_key")?;
            let resolved = model_url.or_else(|| {
                storage_key
                    .as_deref()
                    .and_then(|key| self.storage.public_url(key))
                    .or_else(|| {
                        Some(self.resolve_player_avatar_file_url(
                            user_id,
                            PlayerAvatarSlot::parse(slot.to_ascii_lowercase().as_str())?,
                        ))
                    })
            });
            match slot.as_str() {
                "IDLE" => selection.stationary_model_url = resolved,
                "RUN" => selection.move_model_url = resolved,
                "DANCE" => selection.special_model_url = resolved,
                _ => {}
            }
        }

        Ok(selection)
    }

    pub async fn update_avatar_selection(
        &self,
        user_id: Uuid,
        selection: &AvatarSelection,
    ) -> Result<()> {
        self.upsert_avatar_slot(
            user_id,
            PlayerAvatarSlot::Idle,
            selection.stationary_model_url.as_deref(),
            None,
        )
        .await?;
        self.upsert_avatar_slot(
            user_id,
            PlayerAvatarSlot::Run,
            selection.move_model_url.as_deref(),
            None,
        )
        .await?;
        self.upsert_avatar_slot(
            user_id,
            PlayerAvatarSlot::Dance,
            selection.special_model_url.as_deref(),
            None,
        )
        .await?;
        Ok(())
    }

    pub async fn save_avatar_file(
        &self,
        user_id: Uuid,
        slot: PlayerAvatarSlot,
        bytes: &[u8],
        content_type: &str,
    ) -> Result<AvatarSelection> {
        let storage_key = self.resolve_player_avatar_storage_key(user_id, slot);
        self.storage
            .write_object(
                &storage_key,
                bytes,
                content_type,
                Some(PLAYER_AVATAR_CACHE_CONTROL),
                None,
            )
            .await?;
        let model_url = self.resolve_player_avatar_file_url(user_id, slot);
        self.upsert_avatar_slot(user_id, slot, Some(&model_url), Some(&storage_key))
            .await?;
        self.load_avatar_selection(user_id).await
    }

    pub async fn activate_avatar_storage_keys(
        &self,
        user_id: Uuid,
        idle_storage_key: &str,
        run_storage_key: &str,
        dance_storage_key: &str,
    ) -> Result<AvatarSelection> {
        self.upsert_avatar_slot(
            user_id,
            PlayerAvatarSlot::Idle,
            None,
            Some(idle_storage_key),
        )
        .await?;
        self.upsert_avatar_slot(user_id, PlayerAvatarSlot::Run, None, Some(run_storage_key))
            .await?;
        self.upsert_avatar_slot(
            user_id,
            PlayerAvatarSlot::Dance,
            None,
            Some(dance_storage_key),
        )
        .await?;
        self.load_avatar_selection(user_id).await
    }

    pub async fn read_avatar_file(
        &self,
        user_id: Uuid,
        slot: PlayerAvatarSlot,
    ) -> Result<Option<AvatarFileResponse>> {
        let row = sqlx::query(
            "SELECT model_url, storage_key
             FROM avatar_slots
             WHERE user_id = $1 AND slot = $2",
        )
        .bind(user_id)
        .bind(slot.as_db_value())
        .fetch_optional(&self.pool)
        .await
        .context("load avatar slot file")?;
        let Some(row) = row else {
            return Ok(None);
        };

        let model_url: Option<String> = row.try_get("model_url")?;
        let storage_key: Option<String> = row.try_get("storage_key")?;

        if let Some(storage_key) = storage_key.as_deref() {
            if let Some(url) = self.storage.public_url(storage_key) {
                return Ok(Some(AvatarFileResponse::Redirect { url }));
            }
            if let Some(object) = self.storage.read_object(storage_key).await? {
                return Ok(Some(AvatarFileResponse::Bytes(object)));
            }
        }

        if let Some(url) = model_url {
            return Ok(Some(AvatarFileResponse::Redirect { url }));
        }

        Ok(None)
    }

    pub fn direct_avatar_upload_url_available(&self) -> bool {
        false
    }

    pub async fn load_avatar_generation_preferences(
        &self,
        user_id: Uuid,
    ) -> Result<AvatarStyleOptions> {
        let value: Option<Json<AvatarStyleOptions>> = sqlx::query_scalar(
            "SELECT style_options
             FROM avatar_generation_preferences
             WHERE user_id = $1",
        )
        .bind(user_id)
        .fetch_optional(&self.pool)
        .await
        .context("load avatar generation preferences")?;
        Ok(value.map(|value| value.0).unwrap_or_default())
    }

    pub async fn save_avatar_generation_preferences(
        &self,
        user_id: Uuid,
        style_options: &AvatarStyleOptions,
    ) -> Result<()> {
        sqlx::query(
            "INSERT INTO avatar_generation_preferences (user_id, style_options, updated_at)
             VALUES ($1, $2, NOW())
             ON CONFLICT (user_id)
             DO UPDATE SET style_options = EXCLUDED.style_options,
                           updated_at = NOW()",
        )
        .bind(user_id)
        .bind(Json(style_options.clone()))
        .execute(&self.pool)
        .await
        .context("save avatar generation preferences")?;
        Ok(())
    }

    pub async fn load_avatar_customizer_access(
        &self,
        user_id: Uuid,
    ) -> Result<AvatarCustomizerAccess> {
        let has_entitlement = sqlx::query_scalar::<_, bool>(
            "SELECT avatar_customizer_premium
             FROM account_entitlements
             WHERE user_id = $1",
        )
        .bind(user_id)
        .fetch_optional(&self.pool)
        .await
        .context("load avatar customizer entitlement")?
        .unwrap_or(false);
        Ok(resolve_avatar_customizer_access(
            AVATAR_CUSTOMIZER_BETA_ENABLED,
            has_entitlement,
        ))
    }

    pub async fn build_auth_user(&self, user: SessionUser) -> Result<AuthUserResponse> {
        Ok(AuthUserResponse {
            id: user.id.to_string(),
            name: user.name,
            email: user.email,
            avatar_url: user.avatar_url,
            avatar_selection: self.load_avatar_selection(user.id).await?,
            avatar_customizer_access: self.load_avatar_customizer_access(user.id).await?,
            avatar_generation_defaults: self.load_avatar_generation_preferences(user.id).await?,
            game_auth_token: sign_game_auth_token(
                &self.config.game_auth_secret,
                &user.id.to_string(),
                self.config.game_auth_ttl,
            )?,
        })
    }

    fn apple_redirect_uri(&self) -> String {
        format!(
            "{}/api/v1/auth/apple/callback",
            self.config.public_base_url.trim_end_matches('/')
        )
    }

    fn google_redirect_uri(&self) -> String {
        format!(
            "{}/api/v1/auth/google/callback",
            self.config.public_base_url.trim_end_matches('/')
        )
    }

    fn microsoft_authorize_url(&self) -> String {
        format!(
            "https://login.microsoftonline.com/{}/oauth2/v2.0/authorize",
            self.microsoft_tenant()
        )
    }

    fn microsoft_openid_configuration_url(&self) -> String {
        format!(
            "https://login.microsoftonline.com/{}/v2.0/.well-known/openid-configuration",
            self.microsoft_tenant()
        )
    }

    fn microsoft_redirect_uri(&self) -> String {
        format!(
            "{}/api/v1/auth/microsoft/callback",
            self.config.public_base_url.trim_end_matches('/')
        )
    }

    fn microsoft_scope(&self) -> &str {
        let scope = self.config.microsoft_scope.trim();
        if scope.is_empty() {
            MICROSOFT_SCOPE_DEFAULT
        } else {
            scope
        }
    }

    fn microsoft_tenant(&self) -> &str {
        let tenant = self.config.microsoft_tenant.trim().trim_matches('/');
        if tenant.is_empty() {
            MICROSOFT_DEFAULT_TENANT
        } else {
            tenant
        }
    }

    fn microsoft_token_url(&self) -> String {
        format!(
            "https://login.microsoftonline.com/{}/oauth2/v2.0/token",
            self.microsoft_tenant()
        )
    }

    fn apple_oauth_cookie_config(&self) -> Result<SessionCookieConfig> {
        let redirect_uri = self.apple_redirect_uri();
        let redirect_uri = Url::parse(&redirect_uri).context("parse apple redirect uri")?;
        if redirect_uri.scheme() != "https" {
            anyhow::bail!("Apple OAuth requires PUBLIC_BASE_URL to use https");
        }
        let host = redirect_uri
            .host_str()
            .ok_or_else(|| anyhow!("Apple OAuth requires PUBLIC_BASE_URL to include a host"))?;
        if host.eq_ignore_ascii_case("localhost") || host.parse::<IpAddr>().is_ok() {
            anyhow::bail!("Apple OAuth requires a domain-based PUBLIC_BASE_URL");
        }

        Ok(SessionCookieConfig {
            name: self.config.session_cookie.name.clone(),
            secure: true,
            same_site: SameSitePolicy::None,
            ttl: self.config.session_cookie.ttl,
        })
    }

    async fn create_oauth_session(
        &self,
        user_id: Uuid,
        clear_cookies: Vec<String>,
    ) -> Result<OAuthCallbackResult> {
        let session_id = Uuid::new_v4();
        let expires_at = Utc::now()
            + ChronoDuration::from_std(self.config.session_cookie.ttl)
                .unwrap_or_else(|_| ChronoDuration::hours(24 * 7));

        sqlx::query(
            "INSERT INTO sessions (id, user_id, expires_at)
             VALUES ($1, $2, $3)",
        )
        .bind(session_id)
        .bind(user_id)
        .bind(expires_at)
        .execute(&self.pool)
        .await
        .context("create session")?;

        Ok(OAuthCallbackResult {
            redirect_url: self.post_auth_redirect_url(),
            session_cookie: make_cookie(
                &self.config.session_cookie.name,
                &session_id.to_string(),
                &self.config.session_cookie,
                None,
            ),
            clear_cookies,
        })
    }

    fn post_auth_redirect_url(&self) -> String {
        Url::parse(&self.config.public_base_url)
            .ok()
            .and_then(|base| base.join("play/").ok())
            .map(|url| url.to_string())
            .unwrap_or_else(|| {
                format!(
                    "{}/play/",
                    self.config.public_base_url.trim_end_matches('/')
                )
            })
    }

    async fn validate_apple_id_token(
        &self,
        id_token: &str,
        expected_nonce: &str,
    ) -> Result<AppleIdTokenClaims> {
        let header = decode_header(id_token).context("decode apple id token header")?;
        let kid = header
            .kid
            .ok_or_else(|| anyhow!("apple id token missing key id"))?;

        let keys_response = self
            .http
            .get(APPLE_KEYS_URL)
            .send()
            .await
            .context("fetch apple signing keys")?;
        if !keys_response.status().is_success() {
            let status = keys_response.status();
            let text = keys_response.text().await.unwrap_or_default();
            anyhow::bail!("Apple signing key lookup failed ({status}): {text}");
        }
        let key_set = keys_response
            .json::<AppleKeySet>()
            .await
            .context("decode apple signing keys")?;
        let key = key_set
            .keys
            .into_iter()
            .find(|candidate| candidate.kid == kid)
            .ok_or_else(|| anyhow!("apple signing key not found"))?;

        let decoding_key =
            DecodingKey::from_rsa_components(&key.n, &key.e).context("build apple decoding key")?;
        let mut validation = Validation::new(Algorithm::RS256);
        validation.set_audience(&[self.config.apple_client_id.as_str()]);
        validation.set_issuer(&[APPLE_ISSUER]);
        validation.set_required_spec_claims(&["exp", "iss", "aud", "sub"]);
        let claims = decode::<AppleIdTokenClaims>(id_token, &decoding_key, &validation)
            .context("validate apple id token")?
            .claims;

        if claims.nonce.as_deref() != Some(expected_nonce) {
            anyhow::bail!("invalid apple oauth nonce");
        }

        Ok(claims)
    }

    async fn load_microsoft_openid_configuration(&self) -> Result<MicrosoftOpenIdConfiguration> {
        let response = self
            .http
            .get(self.microsoft_openid_configuration_url())
            .send()
            .await
            .context("fetch microsoft openid configuration")?;
        if !response.status().is_success() {
            let status = response.status();
            let text = response.text().await.unwrap_or_default();
            anyhow::bail!("Microsoft OpenID configuration failed ({status}): {text}");
        }

        response
            .json::<MicrosoftOpenIdConfiguration>()
            .await
            .context("decode microsoft openid configuration")
    }

    fn resolve_player_avatar_storage_key(&self, user_id: Uuid, slot: PlayerAvatarSlot) -> String {
        Path::new(self.storage.namespace())
            .join(user_id.to_string())
            .join("player-avatars")
            .join(slot.as_path_value())
            .join(format!("{}.glb", slot.as_path_value()))
            .to_string_lossy()
            .replace('\\', "/")
    }

    fn resolve_player_avatar_file_url(&self, user_id: Uuid, slot: PlayerAvatarSlot) -> String {
        format!(
            "/api/v1/users/{}/player-avatar/{}/file",
            user_id,
            slot.as_path_value()
        )
    }

    async fn upsert_avatar_slot(
        &self,
        user_id: Uuid,
        slot: PlayerAvatarSlot,
        model_url: Option<&str>,
        storage_key: Option<&str>,
    ) -> Result<()> {
        sqlx::query(
            "INSERT INTO avatar_slots (user_id, slot, model_url, storage_key)
             VALUES ($1, $2, $3, $4)
             ON CONFLICT (user_id, slot)
             DO UPDATE SET
               model_url = EXCLUDED.model_url,
               storage_key = EXCLUDED.storage_key,
               updated_at = NOW()",
        )
        .bind(user_id)
        .bind(slot.as_db_value())
        .bind(model_url)
        .bind(storage_key)
        .execute(&self.pool)
        .await
        .context("upsert avatar slot")?;
        Ok(())
    }

    async fn upsert_oauth_user(
        &self,
        provider: &str,
        provider_label: &str,
        subject: &str,
        email: Option<&str>,
        name: Option<&str>,
        avatar_url: Option<&str>,
    ) -> Result<Uuid> {
        let existing_by_email = if let Some(email) = email {
            sqlx::query(
                "SELECT users.id, auth_identities.subject
                 FROM users
                 LEFT JOIN auth_identities
                   ON auth_identities.user_id = users.id
                  AND auth_identities.provider = $2
                 WHERE users.email = $1
                 LIMIT 1",
            )
            .bind(email)
            .bind(provider)
            .fetch_optional(&self.pool)
            .await
            .context("load existing user by email")?
        } else {
            None
        };

        if let Some(row) = &existing_by_email {
            let existing_subject: Option<String> = row.try_get("subject")?;
            if let Some(existing_subject) = existing_subject {
                if existing_subject != subject {
                    anyhow::bail!("email already linked to another {provider_label} account");
                }
            }
        }

        if let Some(row) = existing_by_email {
            let user_id: Uuid = row.try_get("id")?;
            sqlx::query(
                "UPDATE users
                 SET email = $2,
                     name = $3,
                     avatar_url = $4,
                     updated_at = NOW()
                 WHERE id = $1",
            )
            .bind(user_id)
            .bind(email)
            .bind(name)
            .bind(avatar_url)
            .execute(&self.pool)
            .await
            .context("update user from oauth profile")?;
            sqlx::query(
                "INSERT INTO auth_identities (id, user_id, provider, subject, email)
                 VALUES ($1, $2, $3, $4, $5)
                 ON CONFLICT (provider, subject)
                 DO UPDATE SET email = EXCLUDED.email",
            )
            .bind(Uuid::new_v4())
            .bind(user_id)
            .bind(provider)
            .bind(subject)
            .bind(email)
            .execute(&self.pool)
            .await
            .context("upsert oauth identity for existing user")?;
            return Ok(user_id);
        }

        let existing_identity = sqlx::query(
            "SELECT user_id
             FROM auth_identities
             WHERE provider = $1 AND subject = $2
             LIMIT 1",
        )
        .bind(provider)
        .bind(subject)
        .fetch_optional(&self.pool)
        .await
        .context("load oauth identity")?;

        if let Some(row) = existing_identity {
            let user_id: Uuid = row.try_get("user_id")?;
            sqlx::query(
                "UPDATE users
                 SET email = $2,
                     name = $3,
                     avatar_url = $4,
                     updated_at = NOW()
                 WHERE id = $1",
            )
            .bind(user_id)
            .bind(email)
            .bind(name)
            .bind(avatar_url)
            .execute(&self.pool)
            .await
            .context("refresh oauth linked user")?;
            return Ok(user_id);
        }

        let user_id = Uuid::new_v4();
        sqlx::query(
            "INSERT INTO users (id, email, name, avatar_url)
             VALUES ($1, $2, $3, $4)",
        )
        .bind(user_id)
        .bind(email)
        .bind(name)
        .bind(avatar_url)
        .execute(&self.pool)
        .await
        .context("insert user")?;
        sqlx::query(
            "INSERT INTO auth_identities (id, user_id, provider, subject, email)
             VALUES ($1, $2, $3, $4, $5)",
        )
        .bind(Uuid::new_v4())
        .bind(user_id)
        .bind(provider)
        .bind(subject)
        .bind(email)
        .execute(&self.pool)
        .await
        .context("insert oauth identity")?;
        Ok(user_id)
    }
}

fn apple_user_display_name(name: &AppleAuthorizeUserName) -> Option<String> {
    let first = name.first_name.as_deref().unwrap_or_default().trim();
    let last = name.last_name.as_deref().unwrap_or_default().trim();
    let combined = format!("{first} {last}").trim().to_string();
    if combined.is_empty() {
        None
    } else {
        Some(combined)
    }
}

fn url_encode(value: &str) -> String {
    value
        .bytes()
        .flat_map(|byte| match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                vec![byte as char]
            }
            _ => format!("%{byte:02X}").chars().collect::<Vec<_>>(),
        })
        .collect()
}

fn resolve_avatar_customizer_access(
    beta_enabled: bool,
    has_entitlement: bool,
) -> AvatarCustomizerAccess {
    if beta_enabled {
        return AvatarCustomizerAccess {
            enabled: true,
            source: AvatarCustomizerAccessSource::Beta,
        };
    }
    if has_entitlement {
        return AvatarCustomizerAccess {
            enabled: true,
            source: AvatarCustomizerAccessSource::Entitlement,
        };
    }
    AvatarCustomizerAccess {
        enabled: false,
        source: AvatarCustomizerAccessSource::None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::{SameSitePolicy, SessionCookieConfig};
    use crate::db;
    use crate::server::ServerConfig;
    use crate::storage::{StorageConfig, StorageProvider};
    use std::env;

    fn test_account_config(config: &ServerConfig) -> AccountConfig {
        AccountConfig {
            public_base_url: config.public_base_url.clone(),
            session_cookie: SessionCookieConfig {
                name: "session".to_string(),
                secure: false,
                same_site: SameSitePolicy::Lax,
                ttl: Duration::from_secs(60),
            },
            apple_client_id: String::new(),
            apple_scope: String::new(),
            google_client_id: String::new(),
            google_client_secret: String::new(),
            google_scope: String::new(),
            microsoft_client_id: String::new(),
            microsoft_client_secret: String::new(),
            microsoft_scope: String::new(),
            microsoft_tenant: String::new(),
            game_auth_secret: "test-secret".to_string(),
            game_auth_ttl: Duration::from_secs(60),
        }
    }

    #[test]
    fn resolve_avatar_customizer_access_prefers_beta_then_entitlement() {
        let beta = resolve_avatar_customizer_access(true, false);
        assert!(beta.enabled);
        assert_eq!(beta.source, AvatarCustomizerAccessSource::Beta);

        let entitlement = resolve_avatar_customizer_access(false, true);
        assert!(entitlement.enabled);
        assert_eq!(
            entitlement.source,
            AvatarCustomizerAccessSource::Entitlement
        );

        let none = resolve_avatar_customizer_access(false, false);
        assert!(!none.enabled);
        assert_eq!(none.source, AvatarCustomizerAccessSource::None);
    }

    #[tokio::test]
    async fn build_auth_user_includes_avatar_customizer_defaults() {
        let config = ServerConfig::default();
        let base_database_url = config.database_url.clone();
        let (pool, schema_name) = db::connect_isolated_test_pool(&base_database_url)
            .await
            .expect("create isolated schema");
        let storage_root = env::temp_dir().join(format!("augmego-account-test-{schema_name}"));
        let storage = StorageService::new(StorageConfig {
            provider: StorageProvider::Local,
            root: storage_root,
            namespace: "test-assets".to_string(),
            spaces_bucket: String::new(),
            spaces_endpoint: String::new(),
            spaces_custom_domain: String::new(),
            spaces_access_key_id: String::new(),
            spaces_secret_access_key: String::new(),
            spaces_region: String::new(),
        })
        .await
        .expect("create storage");
        let service = AccountService::new(pool.clone(), storage, test_account_config(&config));

        let user_id = Uuid::new_v4();
        sqlx::query("INSERT INTO users (id, email) VALUES ($1, $2)")
            .bind(user_id)
            .bind("stylist@example.com")
            .execute(&pool)
            .await
            .expect("insert user");

        let saved_defaults = AvatarStyleOptions {
            outfit_style: AvatarOutfitStyle::Streetwear,
            fit_style: AvatarFitStyle::Oversized,
            neck_accessory: AvatarNeckAccessory::Scarf,
            headwear: AvatarHeadwear::Beanie,
            body_build: AvatarBodyBuild::MatchReference,
            skin_tone: AvatarSkinTone::MatchReference,
            pose_energy: AvatarPoseEnergy::Playful,
            material_style: AvatarMaterialStyle::SoftFabric,
            ring: false,
            earrings: true,
            bracelet: false,
            watch: true,
            gloves: true,
            cape: false,
        };
        service
            .save_avatar_generation_preferences(user_id, &saved_defaults)
            .await
            .expect("save avatar generation preferences");

        let auth_user = service
            .build_auth_user(SessionUser {
                id: user_id,
                name: Some("Stylist".to_string()),
                email: Some("stylist@example.com".to_string()),
                avatar_url: None,
            })
            .await
            .expect("build auth user");

        assert!(auth_user.avatar_customizer_access.enabled);
        assert_eq!(
            auth_user.avatar_customizer_access.source,
            AvatarCustomizerAccessSource::Beta
        );
        assert_eq!(auth_user.avatar_generation_defaults, saved_defaults);

        db::cleanup_isolated_test_schema(&base_database_url, &schema_name)
            .await
            .expect("cleanup isolated schema");
    }
}
