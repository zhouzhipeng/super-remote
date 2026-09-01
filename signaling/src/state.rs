use std::{
    collections::HashMap,
    time::{Duration, Instant},
};

use rand::RngCore;
use remote_protocol::{
    device::DeviceSummary,
    signaling::{CreateSessionResponse, ServerSignal},
};
use tokio::sync::{RwLock, mpsc};
use uuid::Uuid;

use crate::auth::{AuthConfig, Principal};

pub type SignalTx = mpsc::Sender<ServerSignal>;

pub struct DeviceConnection {
    pub summary: DeviceSummary,
    pub sender: Option<SignalTx>,
}

pub struct SessionRecord {
    pub owner: String,
    pub device_id: String,
    pub session_token: Option<String>,
    pub browser_sender: Option<SignalTx>,
}

pub struct AppState {
    pub auth: AuthConfig,
    pub devices: RwLock<HashMap<String, DeviceConnection>>,
    pub sessions: RwLock<HashMap<Uuid, SessionRecord>>,
    tickets: RwLock<HashMap<String, (Principal, Instant)>>,
}

#[derive(Debug)]
pub enum CreateSessionError {
    NotFound,
    Offline,
}

impl AppState {
    pub fn new(auth: AuthConfig) -> Self {
        Self {
            auth,
            devices: RwLock::new(HashMap::new()),
            sessions: RwLock::new(HashMap::new()),
            tickets: RwLock::new(HashMap::new()),
        }
    }

    pub async fn issue_ticket(&self, principal: Principal) -> String {
        let mut bytes = [0u8; 32];
        rand::rng().fill_bytes(&mut bytes);
        let ticket = bytes.iter().map(|b| format!("{b:02x}")).collect::<String>();
        let expires = Instant::now() + Duration::from_secs(60);
        let mut tickets = self.tickets.write().await;
        tickets.retain(|_, (_, expiry)| *expiry > Instant::now());
        tickets.insert(ticket.clone(), (principal, expires));
        ticket
    }

    pub async fn consume_ticket(&self, ticket: &str) -> Option<Principal> {
        let (principal, expiry) = self.tickets.write().await.remove(ticket)?;
        (expiry > Instant::now()).then_some(principal)
    }

    pub async fn create_session(
        &self,
        owner: &str,
        device_id: &str,
    ) -> Result<CreateSessionResponse, CreateSessionError> {
        let sender = {
            let devices = self.devices.read().await;
            let device = devices.get(device_id).ok_or(CreateSessionError::NotFound)?;
            device.sender.clone().ok_or(CreateSessionError::Offline)?
        };
        let session_id = Uuid::new_v4();
        let session_token = Uuid::new_v4().simple().to_string();
        self.sessions.write().await.insert(
            session_id,
            SessionRecord {
                owner: owner.to_owned(),
                device_id: device_id.to_owned(),
                session_token: Some(session_token.clone()),
                browser_sender: None,
            },
        );
        sender
            .send(ServerSignal::SessionRequested {
                session_id,
                session_token: session_token.clone(),
            })
            .await
            .map_err(|_| CreateSessionError::Offline)?;
        Ok(CreateSessionResponse {
            session_id,
            session_token,
        })
    }

    pub async fn route_to_device(
        &self,
        session_id: Uuid,
        sender_id: &str,
        message: ServerSignal,
    ) -> bool {
        let device_id = {
            let sessions = self.sessions.read().await;
            let Some(session) = sessions.get(&session_id) else {
                return false;
            };
            if session.owner != sender_id {
                return false;
            }
            session.device_id.clone()
        };
        let tx = self
            .devices
            .read()
            .await
            .get(&device_id)
            .and_then(|d| d.sender.clone());
        match tx {
            Some(tx) => tx.send(message).await.is_ok(),
            None => false,
        }
    }

    pub async fn authorize_offer(
        &self,
        session_id: Uuid,
        owner: &str,
        session_token: &str,
        browser_sender: SignalTx,
    ) -> bool {
        let mut sessions = self.sessions.write().await;
        let Some(session) = sessions.get_mut(&session_id) else {
            return false;
        };
        if session.owner != owner || session.session_token.as_deref() != Some(session_token) {
            return false;
        }
        session.session_token = None;
        // Bind Host replies to the exact WebSocket that submitted this offer.
        // A user may have Safari and Chrome open simultaneously, and one
        // browser may briefly own multiple sockets while reconnecting.
        session.browser_sender = Some(browser_sender);
        true
    }

    pub async fn route_to_browser(
        &self,
        session_id: Uuid,
        device_id: &str,
        message: ServerSignal,
    ) -> bool {
        let tx = {
            let sessions = self.sessions.read().await;
            let Some(session) = sessions.get(&session_id) else {
                return false;
            };
            if session.device_id != device_id {
                return false;
            }
            session.browser_sender.clone()
        };
        match tx {
            Some(tx) => tx.send(message).await.is_ok(),
            None => false,
        }
    }
}
