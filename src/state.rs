use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Serialize, Clone)]
pub struct Event {
    pub id: u64,
    /// system|reasoning|text|tool_call|tool_result|subagent|injection|usage|note
    pub t: String,
    /// system|model|tool|subagent|context
    pub source: String,
    pub text: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub extra: Option<Value>,
}

impl Default for Event {
    fn default() -> Self {
        Event {
            id: 0,
            t: String::new(),
            source: String::new(),
            text: String::new(),
            extra: None,
        }
    }
}

#[derive(Serialize)]
pub struct SessionView {
    pub id: u64,
    pub title: String,
    pub running: bool,
    pub finished: bool,
    pub event_count: usize,
    pub workspace: Option<String>,
}

pub struct Session {
    pub id: u64,
    pub title: String,
    pub events: Vec<Event>,
    pub running: bool,
    pub finished: bool,
    pub workspace: Option<String>,
    /// call_id we are currently blocked on (Standard permission preset)
    pub awaiting: Option<String>,
    pub approved: Arc<AtomicBool>,
    pub event_seq: u64,
}

impl Session {
    pub fn new(id: u64, title: String) -> Self {
        Session {
            id,
            title,
            events: Vec::new(),
            running: false,
            finished: false,
            workspace: None,
            awaiting: None,
            approved: Arc::new(AtomicBool::new(false)),
            event_seq: 0,
        }
    }

    pub fn push(&mut self, t: &str, source: &str, text: String, extra: Option<Value>) {
        self.event_seq += 1;
        self.events.push(Event {
            id: self.event_seq,
            t: t.to_string(),
            source: source.to_string(),
            text,
            extra,
        });
    }

    pub fn view(&self) -> SessionView {
        SessionView {
            id: self.id,
            title: self.title.clone(),
            running: self.running,
            finished: self.finished,
            event_count: self.events.len(),
            workspace: self.workspace.clone(),
        }
    }
}

#[derive(Serialize, Deserialize, Clone)]
pub struct Provider {
    pub name: String,
    pub base_url: String,
    #[serde(default)]
    pub api_key: String,
    #[serde(default)]
    pub models: Vec<String>,
}

#[derive(Serialize, Deserialize, Clone)]
pub struct Settings {
    #[serde(default = "default_model")]
    pub default_model: String,
    #[serde(default)]
    pub providers: Vec<Provider>,
}

fn default_model() -> String {
    "deepseek-v3.2".to_string()
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            default_model: default_model(),
            providers: Vec::new(),
        }
    }
}

pub struct AppState {
    pub sessions: Vec<Session>,
    pub next_id: u64,
    pub settings: Settings,
    pub settings_path: String,
    pub token: String,
}

impl AppState {
    pub fn new(settings_path: String, token: String) -> Self {
        AppState {
            sessions: Vec::new(),
            next_id: 1,
            settings: Settings::default(),
            settings_path,
            token,
        }
    }

    pub fn create_session(&mut self, title: String) -> &mut Session {
        let s = Session::new(self.next_id, title);
        let id = s.id;
        self.sessions.push(s);
        self.next_id += 1;
        self.sessions.iter_mut().find(|x| x.id == id).unwrap()
    }

    pub fn session(&mut self, id: u64) -> Option<&mut Session> {
        self.sessions.iter_mut().find(|s| s.id == id)
    }

    pub fn fork(&mut self, id: u64) -> Option<u64> {
        let src: Vec<Event> = self
            .sessions
            .iter()
            .find(|s| s.id == id)
            .map(|s| s.events.clone())?;
        let title: String = self
            .sessions
            .iter()
            .find(|s| s.id == id)
            .map(|s| format!("{} (copy)", s.title))
            .unwrap_or_default();
        let ws: Option<String> = self
            .sessions
            .iter()
            .find(|s| s.id == id)
            .and_then(|s| s.workspace.clone());
        let mut s = Session::new(self.next_id, title);
        s.events = src;
        s.event_seq = src.len() as u64;
        s.workspace = ws;
        s.finished = true;
        let fid = s.id;
        self.sessions.push(s);
        self.next_id += 1;
        Some(fid)
    }
}
