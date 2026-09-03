use std::{
    collections::HashMap,
    time::{Duration, Instant},
};

use rand::RngCore;
use remote_protocol::{
    device::DeviceSummary,
    signaling::{CreateSessionResponse, ServerSignal},
};
use tokio::sync::{Mutex, RwLock, mpsc};
use tracing::info;
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
    session_creation: Mutex<()>,
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
            session_creation: Mutex::new(()),
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
        // Serialize the whole handoff, including messages sent to the Host. This
        // guarantees that two browsers racing to connect cannot reorder their
        // SessionRequested messages after ownership has already changed.
        let _creation = self.session_creation.lock().await;
        let sender = {
            let devices = self.devices.read().await;
            let device = devices.get(device_id).ok_or(CreateSessionError::NotFound)?;
            device.sender.clone().ok_or(CreateSessionError::Offline)?
        };
        let session_id = Uuid::new_v4();
        let session_token = Uuid::new_v4().simple().to_string();
        let replaced = {
            let mut sessions = self.sessions.write().await;
            let replaced_ids = sessions
                .iter()
                .filter_map(|(id, session)| (session.device_id == device_id).then_some(*id))
                .collect::<Vec<_>>();
            let replaced = replaced_ids
                .into_iter()
                .filter_map(|id| sessions.remove(&id).map(|session| (id, session)))
                .collect::<Vec<_>>();
            sessions.insert(
                session_id,
                SessionRecord {
                    owner: owner.to_owned(),
                    device_id: device_id.to_owned(),
                    session_token: Some(session_token.clone()),
                    browser_sender: None,
                },
            );
            replaced
        };

        // The Host sees every old close before the new request on the same
        // ordered channel. Late close/ICE events from an evicted session are
        // harmless because its record was removed before any message was sent.
        for (old_id, _) in &replaced {
            if sender
                .send(ServerSignal::SessionClosed {
                    session_id: *old_id,
                    reason: "replaced_by_new_connection".into(),
                })
                .await
                .is_err()
            {
                self.remove_session_if_current(session_id).await;
                return Err(CreateSessionError::Offline);
            }
        }
        if sender
            .send(ServerSignal::SessionRequested {
                session_id,
                session_token: session_token.clone(),
            })
            .await
            .is_err()
        {
            self.remove_session_if_current(session_id).await;
            return Err(CreateSessionError::Offline);
        }

        // Notify established old browsers directly as well. This is best effort:
        // the Host-side peer close above is the authoritative forced disconnect.
        for (old_id, old_session) in &replaced {
            if let Some(browser) = &old_session.browser_sender {
                let _ = browser.try_send(ServerSignal::SessionClosed {
                    session_id: *old_id,
                    reason: "replaced_by_new_connection".into(),
                });
            }
        }
        if !replaced.is_empty() {
            info!(%device_id, %session_id, replaced = replaced.len(), "new session replaced all older device sessions");
        }
        Ok(CreateSessionResponse {
            session_id,
            session_token,
        })
    }

    async fn remove_session_if_current(&self, session_id: Uuid) {
        self.sessions.write().await.remove(&session_id);
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

#[cfg(test)]
mod tests {
    use remote_protocol::{device::DeviceCapabilities, signaling::ServerSignal};

    use super::*;

    #[tokio::test]
    async fn newest_session_atomically_replaces_every_older_device_session() {
        let state = AppState::new(AuthConfig::for_tests());
        let (device_tx, mut device_rx) = mpsc::channel(16);
        state.devices.write().await.insert(
            "host-1".into(),
            DeviceConnection {
                summary: DeviceSummary {
                    id: "host-1".into(),
                    name: "Test host".into(),
                    online: true,
                    capabilities: DeviceCapabilities::default(),
                },
                sender: Some(device_tx),
            },
        );

        let first = state.create_session("user", "host-1").await.unwrap();
        assert!(matches!(
            device_rx.recv().await,
            Some(ServerSignal::SessionRequested { session_id, .. }) if session_id == first.session_id
        ));
        let (browser_tx, mut browser_rx) = mpsc::channel(4);
        assert!(
            state
                .authorize_offer(first.session_id, "user", &first.session_token, browser_tx)
                .await
        );

        let second = state.create_session("user", "host-1").await.unwrap();
        assert!(matches!(
            device_rx.recv().await,
            Some(ServerSignal::SessionClosed { session_id, reason })
                if session_id == first.session_id && reason == "replaced_by_new_connection"
        ));
        assert!(matches!(
            device_rx.recv().await,
            Some(ServerSignal::SessionRequested { session_id, .. }) if session_id == second.session_id
        ));
        assert!(matches!(
            browser_rx.recv().await,
            Some(ServerSignal::SessionClosed { session_id, reason })
                if session_id == first.session_id && reason == "replaced_by_new_connection"
        ));

        let sessions = state.sessions.read().await;
        assert_eq!(sessions.len(), 1);
        assert!(!sessions.contains_key(&first.session_id));
        assert!(sessions.contains_key(&second.session_id));
        drop(sessions);
        assert!(
            !state
                .route_to_device(first.session_id, "user", ServerSignal::Pong { nonce: 1 })
                .await
        );
    }

    #[tokio::test]
    async fn newest_session_invalidates_an_older_pending_offer() {
        let state = AppState::new(AuthConfig::for_tests());
        let (device_tx, mut device_rx) = mpsc::channel(16);
        state.devices.write().await.insert(
            "host-1".into(),
            DeviceConnection {
                summary: DeviceSummary {
                    id: "host-1".into(),
                    name: "Test host".into(),
                    online: true,
                    capabilities: DeviceCapabilities::default(),
                },
                sender: Some(device_tx),
            },
        );

        let first = state.create_session("user", "host-1").await.unwrap();
        let _ = device_rx.recv().await;
        let second = state.create_session("user", "host-1").await.unwrap();
        let _ = device_rx.recv().await;
        let _ = device_rx.recv().await;
        let (browser_tx, _) = mpsc::channel(1);

        assert!(
            !state
                .authorize_offer(first.session_id, "user", &first.session_token, browser_tx)
                .await
        );
        assert!(state.sessions.read().await.contains_key(&second.session_id));
    }
}
