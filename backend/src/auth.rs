use anyhow::Result;
use jsonwebtoken::{Algorithm, DecodingKey, EncodingKey, Header, Validation, decode, encode};
use serde::{Deserialize, Serialize};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SameSitePolicy {
    Lax,
    Strict,
    None,
}

impl SameSitePolicy {
    pub fn parse(value: &str) -> Self {
        match value.trim().to_ascii_lowercase().as_str() {
            "strict" => Self::Strict,
            "none" => Self::None,
            _ => Self::Lax,
        }
    }

    pub fn as_header_value(self) -> &'static str {
        match self {
            Self::Lax => "Lax",
            Self::Strict => "Strict",
            Self::None => "None",
        }
    }
}

#[derive(Clone, Debug)]
pub struct SessionCookieConfig {
    pub name: String,
    pub secure: bool,
    pub same_site: SameSitePolicy,
    pub ttl: Duration,
}

#[derive(Debug, Serialize, Deserialize)]
struct GameAuthClaims {
    sub: String,
    exp: usize,
}

pub fn sign_game_auth_token(secret: &str, user_id: &str, ttl: Duration) -> Result<String> {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let claims = GameAuthClaims {
        sub: user_id.to_string(),
        exp: usize::try_from(now + ttl.as_secs()).unwrap_or(usize::MAX),
    };
    Ok(encode(
        &Header::new(Algorithm::HS256),
        &claims,
        &EncodingKey::from_secret(secret.as_bytes()),
    )?)
}

pub fn verify_game_auth_token(secret: &str, token: &str) -> Option<String> {
    let mut validation = Validation::new(Algorithm::HS256);
    validation.validate_exp = true;
    let decoded = decode::<GameAuthClaims>(
        token,
        &DecodingKey::from_secret(secret.as_bytes()),
        &validation,
    )
    .ok()?;
    Some(decoded.claims.sub)
}

pub fn parse_cookie(header: Option<&str>, name: &str) -> Option<String> {
    let header = header?;
    for pair in header.split(';') {
        let mut parts = pair.trim().splitn(2, '=');
        let key = parts.next()?.trim();
        if key != name {
            continue;
        }
        let value = parts.next().unwrap_or_default();
        return Some(percent_decode(value));
    }
    None
}

pub fn make_cookie(
    name: &str,
    value: &str,
    config: &SessionCookieConfig,
    max_age: Option<Duration>,
) -> String {
    let encoded_value = percent_encode(value);
    let mut parts = vec![format!("{name}={encoded_value}")];
    let age = max_age.unwrap_or(config.ttl).as_secs();
    parts.push(format!("Max-Age={age}"));
    parts.push("Path=/".to_string());
    parts.push(format!("SameSite={}", config.same_site.as_header_value()));
    parts.push("HttpOnly".to_string());
    if config.secure {
        parts.push("Secure".to_string());
    }
    parts.join("; ")
}

pub fn clear_cookie(name: &str, config: &SessionCookieConfig) -> String {
    let mut parts = vec![format!("{name}=")];
    parts.push("Max-Age=0".to_string());
    parts.push("Path=/".to_string());
    parts.push(format!("SameSite={}", config.same_site.as_header_value()));
    parts.push("HttpOnly".to_string());
    if config.secure {
        parts.push("Secure".to_string());
    }
    parts.join("; ")
}

fn percent_encode(value: &str) -> String {
    value.bytes()
        .flat_map(|byte| match byte {
            b'A'..=b'Z'
            | b'a'..=b'z'
            | b'0'..=b'9'
            | b'-'
            | b'_'
            | b'.'
            | b'~' => vec![byte as char],
            _ => format!("%{byte:02X}").chars().collect::<Vec<_>>(),
        })
        .collect()
}

fn percent_decode(value: &str) -> String {
    let mut bytes = Vec::with_capacity(value.len());
    let mut chars = value.as_bytes().iter().copied();

    while let Some(byte) = chars.next() {
        if byte == b'%' {
            let hi = chars.next().unwrap_or(b'0');
            let lo = chars.next().unwrap_or(b'0');
            let hex = [hi, lo];
            if let Ok(hex_str) = std::str::from_utf8(&hex) {
                if let Ok(decoded) = u8::from_str_radix(hex_str, 16) {
                    bytes.push(decoded);
                    continue;
                }
            }
        }
        bytes.push(byte);
    }

    String::from_utf8(bytes).unwrap_or_default()
}
