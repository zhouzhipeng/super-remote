use std::{
    fs,
    path::PathBuf,
    sync::Mutex,
    time::{SystemTime, UNIX_EPOCH},
};

use serde::Serialize;
use uuid::Uuid;

use crate::config::HostConfig;

#[derive(Clone, Serialize)]
struct HostSnapshot {
    host_pid: u32,
    online: bool,
    connection_state: String,
    session_id: Option<Uuid>,
    capture_active: bool,
    width: u32,
    height: u32,
    fps: u16,
    bitrate: u32,
    encoder: String,
    monitor_index: usize,
    updated_at_unix_ms: u128,
}

pub struct ControlStatus {
    path: Option<PathBuf>,
    snapshot: Mutex<HostSnapshot>,
}

impl ControlStatus {
    pub fn new(config: &HostConfig) -> Self {
        let status = Self {
            path: config.control_status_path.clone(),
            snapshot: Mutex::new(HostSnapshot {
                host_pid: std::process::id(),
                online: false,
                connection_state: "starting".into(),
                session_id: None,
                capture_active: false,
                width: config.width,
                height: config.height,
                fps: config.fps,
                bitrate: config.bitrate,
                encoder: config.ffmpeg_encoder.clone(),
                monitor_index: config.monitor_index,
                updated_at_unix_ms: now_ms(),
            }),
        };
        status.write();
        status
    }

    pub fn online(&self) {
        self.update(|snapshot| {
            snapshot.online = true;
            snapshot.connection_state = "waiting".into();
        });
    }

    pub fn preparing(&self, session_id: Uuid, config: &HostConfig) {
        self.update(|snapshot| {
            snapshot.session_id = Some(session_id);
            snapshot.connection_state = "connecting".into();
            snapshot.capture_active = false;
            snapshot.width = config.width;
            snapshot.height = config.height;
            snapshot.bitrate = config.bitrate;
        });
    }

    pub fn connected(&self, session_id: Uuid) {
        self.update_for_session(session_id, |snapshot| {
            snapshot.connection_state = "connected".into();
            snapshot.capture_active = true;
        });
    }

    pub fn disconnected(&self, session_id: Uuid) {
        self.update_for_session(session_id, |snapshot| {
            snapshot.connection_state = "waiting".into();
            snapshot.session_id = None;
            snapshot.capture_active = false;
        });
    }

    pub fn offline(&self) {
        self.update(|snapshot| {
            snapshot.online = false;
            snapshot.connection_state = "offline".into();
            snapshot.session_id = None;
            snapshot.capture_active = false;
        });
    }

    fn update_for_session(&self, session_id: Uuid, update: impl FnOnce(&mut HostSnapshot)) {
        self.update(|snapshot| {
            if snapshot.session_id == Some(session_id) {
                update(snapshot);
            }
        });
    }

    fn update(&self, update: impl FnOnce(&mut HostSnapshot)) {
        if let Ok(mut snapshot) = self.snapshot.lock() {
            update(&mut snapshot);
            snapshot.updated_at_unix_ms = now_ms();
        }
        self.write();
    }

    fn write(&self) {
        let Some(path) = &self.path else {
            return;
        };
        let Ok(snapshot) = self.snapshot.lock() else {
            return;
        };
        let Ok(json) = serde_json::to_vec_pretty(&*snapshot) else {
            return;
        };
        if let Some(parent) = path.parent() {
            let _ = fs::create_dir_all(parent);
        }
        let _ = fs::write(path, json);
    }
}

impl Drop for ControlStatus {
    fn drop(&mut self) {
        self.offline();
    }
}

fn now_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
}
