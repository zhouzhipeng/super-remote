use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, bail};
use axum::http::HeaderMap;
use base64::{Engine, engine::general_purpose::STANDARD};
use hmac::{Hmac, Mac};
use jsonwebtoken::{Algorithm, DecodingKey, EncodingKey, Header, Validation, decode, encode};
use remote_protocol::signaling::TurnCredentials;
use serde::{Deserialize, Serialize};
use sha1::Sha1;

#[derive(Debug, Clone)]
pub struct AuthConfig {
    jwt_secret: Vec<u8>,
    username: String,
    password: String,
    device_token: String,
    turn_urls: Vec<String>,
    turn_secret: Option<Vec<u8>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LoginRequest {
    pub username: String,
    pub password: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct LoginResponse {
    pub access_token: String,
    pub expires_in: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct TicketResponse {
    pub ticket: String,
    pub expires_in: u64,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    User,
    Device,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Principal {
    pub subject: String,
    pub role: Role,
}

#[derive(Debug, Serialize, Deserialize)]
struct Claims {
    sub: String,
    role: Role,
    exp: usize,
    iat: usize,
}

impl AuthConfig {
    pub fn from_env() -> anyhow::Result<Self> {
        let jwt_secret = required("REMOTE_JWT_SECRET")?.into_bytes();
        if jwt_secret.len() < 32 {
            bail!("REMOTE_JWT_SECRET must contain at least 32 bytes");
        }
        let password = required("REMOTE_ADMIN_PASSWORD")?;
        if password.len() < 12 {
            bail!("REMOTE_ADMIN_PASSWORD must contain at least 12 characters");
        }
        let device_token = required("REMOTE_DEVICE_TOKEN")?;
        if device_token.len() < 24 {
            bail!("REMOTE_DEVICE_TOKEN must contain at least 24 characters");
        }
        Ok(Self {
            jwt_secret,
            username: std::env::var("REMOTE_ADMIN_USER").unwrap_or_else(|_| "admin".into()),
            password,
            device_token,
            turn_urls: std::env::var("REMOTE_TURN_URLS")
                .unwrap_or_default()
                .split(',')
                .filter(|s| !s.is_empty())
                .map(str::to_owned)
                .collect(),
            turn_secret: std::env::var("REMOTE_TURN_SECRET")
                .ok()
                .map(String::into_bytes),
        })
    }

    pub fn login(&self, request: &LoginRequest) -> anyhow::Result<String> {
        if request.username != self.username || request.password != self.password {
            bail!("invalid credentials");
        }
        let now = unix_seconds() as usize;
        let claims = Claims {
            sub: request.username.clone(),
            role: Role::User,
            iat: now,
            exp: now + 3600,
        };
        Ok(encode(
            &Header::new(Algorithm::HS256),
            &claims,
            &EncodingKey::from_secret(&self.jwt_secret),
        )?)
    }

    pub fn authenticate_bearer(&self, headers: &HeaderMap) -> anyhow::Result<Principal> {
        let value = headers
            .get("authorization")
            .context("missing authorization")?
            .to_str()?;
        let token = value
            .strip_prefix("Bearer ")
            .context("invalid authorization scheme")?;
        let claims = decode::<Claims>(
            token,
            &DecodingKey::from_secret(&self.jwt_secret),
            &Validation::new(Algorithm::HS256),
        )?
        .claims;
        Ok(Principal {
            subject: claims.sub,
            role: claims.role,
        })
    }

    pub fn authenticate_user(&self, headers: &HeaderMap) -> anyhow::Result<Principal> {
        let principal = self.authenticate_bearer(headers)?;
        if principal.role != Role::User {
            bail!("not a user");
        }
        Ok(principal)
    }

    pub fn verify_device_token(&self, candidate: &str) -> bool {
        constant_time_eq(candidate.as_bytes(), self.device_token.as_bytes())
    }

    pub fn turn_credentials(&self, subject: &str) -> Option<TurnCredentials> {
        let secret = self.turn_secret.as_ref()?;
        if self.turn_urls.is_empty() {
            return None;
        }
        let ttl_seconds = 600;
        let username = format!("{}:{}", unix_seconds() + ttl_seconds, subject);
        let mut mac = Hmac::<Sha1>::new_from_slice(secret).ok()?;
        mac.update(username.as_bytes());
        let credential = STANDARD.encode(mac.finalize().into_bytes());
        Some(TurnCredentials {
            urls: self.turn_urls.clone(),
            username,
            credential,
            ttl_seconds,
        })
    }
}

fn required(name: &str) -> anyhow::Result<String> {
    std::env::var(name).with_context(|| format!("required environment variable {name} is not set"))
}

pub fn unix_seconds() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}
