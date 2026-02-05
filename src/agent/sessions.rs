//! Persistent conversation sessions for the agent
//!
//! Allows saving and resuming agent conversations to/from disk.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::ollama::tool_use::ChatMessage;

/// A saved conversation session
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Session {
    /// Unique session ID
    pub id: String,
    /// Session creation timestamp
    pub created_at: String,
    /// Last update timestamp
    pub updated_at: String,
    /// The original task that started this session
    pub task: String,
    /// Model used for this session
    pub model: String,
    /// Conversation history
    pub messages: Vec<ChatMessage>,
    /// Total iterations so far
    pub iterations: usize,
    /// Whether the session is complete
    pub complete: bool,
}

impl Session {
    /// Create a new session
    pub fn new(task: impl Into<String>, model: impl Into<String>) -> Self {
        let now = chrono_timestamp();
        Self {
            id: uuid::Uuid::now_v7().to_string(),
            created_at: now.clone(),
            updated_at: now,
            task: task.into(),
            model: model.into(),
            messages: Vec::new(),
            iterations: 0,
            complete: false,
        }
    }

    /// Add a message to the conversation
    pub fn add_message(&mut self, message: ChatMessage) {
        self.messages.push(message);
        self.updated_at = chrono_timestamp();
    }

    /// Mark the session as complete
    pub fn mark_complete(&mut self) {
        self.complete = true;
        self.updated_at = chrono_timestamp();
    }

    /// Increment iteration count
    pub fn increment_iterations(&mut self) {
        self.iterations += 1;
        self.updated_at = chrono_timestamp();
    }
}

/// Session storage manager
pub struct SessionStore {
    /// Base directory for session files
    base_dir: PathBuf,
}

impl SessionStore {
    /// Create a new session store
    pub fn new(base_dir: impl Into<PathBuf>) -> std::io::Result<Self> {
        let base_dir = base_dir.into();
        std::fs::create_dir_all(&base_dir)?;
        Ok(Self { base_dir })
    }

    /// Create with default directory (~/.neurovisor/sessions)
    pub fn default_store() -> std::io::Result<Self> {
        let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
        let base_dir = PathBuf::from(home).join(".neurovisor").join("sessions");
        Self::new(base_dir)
    }

    /// Save a session to disk
    pub fn save(&self, session: &Session) -> std::io::Result<()> {
        let path = self.session_path(&session.id);
        let json = serde_json::to_string_pretty(session)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        std::fs::write(path, json)
    }

    /// Load a session from disk
    pub fn load(&self, session_id: &str) -> std::io::Result<Session> {
        let path = self.session_path(session_id);
        let json = std::fs::read_to_string(path)?;
        serde_json::from_str(&json)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))
    }

    /// List all sessions
    pub fn list(&self) -> std::io::Result<Vec<SessionSummary>> {
        let mut sessions = Vec::new();
        for entry in std::fs::read_dir(&self.base_dir)? {
            let entry = entry?;
            let path = entry.path();
            if path.extension().map(|e| e == "json").unwrap_or(false) {
                if let Ok(session) = self.load_from_path(&path) {
                    sessions.push(SessionSummary {
                        id: session.id,
                        task: truncate(&session.task, 50),
                        created_at: session.created_at,
                        iterations: session.iterations,
                        complete: session.complete,
                    });
                }
            }
        }
        // Sort by creation time (newest first)
        sessions.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        Ok(sessions)
    }

    /// Delete a session
    pub fn delete(&self, session_id: &str) -> std::io::Result<()> {
        let path = self.session_path(session_id);
        std::fs::remove_file(path)
    }

    /// Get the file path for a session
    fn session_path(&self, session_id: &str) -> PathBuf {
        self.base_dir.join(format!("{}.json", session_id))
    }

    /// Load a session from a specific path
    fn load_from_path(&self, path: &PathBuf) -> std::io::Result<Session> {
        let json = std::fs::read_to_string(path)?;
        serde_json::from_str(&json)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))
    }
}

/// Summary of a session for listing
#[derive(Debug, Clone, Serialize)]
pub struct SessionSummary {
    pub id: String,
    pub task: String,
    pub created_at: String,
    pub iterations: usize,
    pub complete: bool,
}

/// Get current timestamp as ISO string
fn chrono_timestamp() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let duration = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    let secs = duration.as_secs();
    // Simple ISO-like format: YYYY-MM-DD HH:MM:SS
    let days = secs / 86400;
    let years = 1970 + days / 365;
    let remaining_days = days % 365;
    let months = remaining_days / 30 + 1;
    let day = remaining_days % 30 + 1;
    let hours = (secs % 86400) / 3600;
    let minutes = (secs % 3600) / 60;
    let seconds = secs % 60;
    format!(
        "{:04}-{:02}-{:02} {:02}:{:02}:{:02}",
        years, months, day, hours, minutes, seconds
    )
}

/// Truncate a string to max length, adding ellipsis if needed
fn truncate(s: &str, max_len: usize) -> String {
    if s.len() <= max_len {
        s.to_string()
    } else {
        format!("{}...", &s[..max_len - 3])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_session_creation() {
        let session = Session::new("Find prime numbers", "llama3.2");
        assert!(!session.id.is_empty());
        assert_eq!(session.task, "Find prime numbers");
        assert_eq!(session.model, "llama3.2");
        assert!(session.messages.is_empty());
        assert_eq!(session.iterations, 0);
        assert!(!session.complete);
    }

    #[test]
    fn test_session_add_message() {
        let mut session = Session::new("Test", "model");
        session.add_message(ChatMessage::user("Hello"));
        assert_eq!(session.messages.len(), 1);
        assert_eq!(session.messages[0].content, "Hello");
    }

    #[test]
    fn test_truncate() {
        assert_eq!(truncate("short", 10), "short");
        assert_eq!(truncate("this is a long string", 10), "this is...");
    }
}
