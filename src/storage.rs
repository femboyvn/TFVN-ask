use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

const MAX_HISTORY_MESSAGES: usize = 10;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatMessage {
    pub role: String,
    pub content: String,
}

impl ChatMessage {
    pub fn new(role: &str, content: impl Into<String>) -> Self {
        Self {
            role: role.into(),
            content: content.into(),
        }
    }
}

fn read_json(path: &Path) -> Option<Value> {
    match fs::read(path) {
        Ok(data) => match serde_json::from_slice(&data) {
            Ok(value) => Some(value),
            Err(error) => {
                eprintln!("Could not load {}: {error}", path.display());
                None
            }
        },
        Err(error) if error.kind() == io::ErrorKind::NotFound => None,
        Err(error) => {
            eprintln!("Could not load {}: {error}", path.display());
            None
        }
    }
}

fn save_json<T: Serialize>(path: &Path, data: &T) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut temp = path.as_os_str().to_os_string();
    temp.push(".tmp");
    let temp = PathBuf::from(temp);
    let mut bytes = serde_json::to_vec_pretty(data)?;
    bytes.push(b'\n');
    fs::write(&temp, bytes)?;
    fs::rename(temp, path)
}

pub struct ConversationStore {
    path: PathBuf,
    histories: BTreeMap<String, Vec<ChatMessage>>,
}

impl ConversationStore {
    pub fn load(path: PathBuf) -> Self {
        let mut histories = BTreeMap::new();
        if let Some(Value::Object(data)) = read_json(&path) {
            for (key, value) in data {
                let Some(items) = value.as_array() else {
                    continue;
                };
                if items.len() % 2 != 0 {
                    continue;
                }
                let mut messages = Vec::with_capacity(items.len());
                for (index, item) in items.iter().enumerate() {
                    let expected_role = if index % 2 == 0 { "user" } else { "assistant" };
                    let Some(role) = item.get("role").and_then(Value::as_str) else {
                        break;
                    };
                    let Some(content) = item.get("content").and_then(Value::as_str) else {
                        break;
                    };
                    if role != expected_role || content.trim().is_empty() {
                        break;
                    }
                    messages.push(ChatMessage::new(role, content));
                }
                if messages.len() == items.len() {
                    histories.insert(
                        key,
                        messages
                            .into_iter()
                            .rev()
                            .take(MAX_HISTORY_MESSAGES)
                            .collect::<Vec<_>>()
                            .into_iter()
                            .rev()
                            .collect(),
                    );
                }
            }
        }
        Self { path, histories }
    }

    pub fn get(&self, key: &str) -> Vec<ChatMessage> {
        self.histories.get(key).cloned().unwrap_or_default()
    }

    pub fn append_turn(&mut self, key: &str, prompt: &str, response: &str) -> io::Result<()> {
        let messages = self.histories.entry(key.to_owned()).or_default();
        messages.push(ChatMessage::new("user", prompt));
        messages.push(ChatMessage::new("assistant", response));
        if messages.len() > MAX_HISTORY_MESSAGES {
            messages.drain(..messages.len() - MAX_HISTORY_MESSAGES);
        }
        save_json(&self.path, &self.histories)
    }

    pub fn reset(&mut self, key: &str) -> io::Result<()> {
        self.histories.remove(key);
        save_json(&self.path, &self.histories)
    }

    pub fn reset_member(&mut self, member_key: &str) -> io::Result<()> {
        let Some((guild, user)) = member_key.split_once(':') else {
            return Ok(());
        };
        self.histories.retain(|key, _| {
            let mut parts = key.split(':');
            !matches!((parts.next(), parts.next(), parts.next(), parts.next()),
                (Some(g), Some(_), Some(u), None) if g == guild && u == user)
        });
        save_json(&self.path, &self.histories)
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Preferences {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub nickname: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pronouns: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub interests: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reply_style: Option<String>,
}

impl Preferences {
    pub fn is_empty(&self) -> bool {
        self.nickname.is_none()
            && self.pronouns.is_none()
            && self.interests.is_empty()
            && self.reply_style.is_none()
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct MemoryEntry {
    enabled: bool,
    preferences: Preferences,
}

pub struct MemoryStore {
    path: PathBuf,
    entries: BTreeMap<String, MemoryEntry>,
}

impl MemoryStore {
    pub fn load(path: PathBuf) -> Self {
        let mut entries = BTreeMap::new();
        if let Some(Value::Object(data)) = read_json(&path) {
            for (key, value) in data {
                if !valid_member_key(&key) {
                    continue;
                }
                let Some(enabled) = value.get("enabled").and_then(Value::as_bool) else {
                    continue;
                };
                let empty = serde_json::Map::new();
                let raw = match value.get("preferences") {
                    Some(Value::Object(raw)) => raw,
                    None => &empty,
                    _ => continue,
                };
                let mut preferences = Preferences::default();
                for field in ["nickname", "pronouns", "reply_style"] {
                    if let Some(clean) =
                        validate_memory_value(field, raw.get(field).unwrap_or(&Value::Null))
                    {
                        match field {
                            "nickname" => preferences.nickname = Some(clean),
                            "pronouns" => preferences.pronouns = Some(clean),
                            _ => preferences.reply_style = Some(clean),
                        }
                    }
                }
                if let Some(interests) = raw.get("interests").and_then(Value::as_array) {
                    preferences.interests = interests
                        .iter()
                        .take(10)
                        .filter_map(|item| validate_memory_value("interest", item))
                        .collect();
                }
                entries.insert(
                    key,
                    MemoryEntry {
                        enabled,
                        preferences,
                    },
                );
            }
        }
        Self { path, entries }
    }

    pub fn is_enabled(&self, key: &str) -> bool {
        self.entries.get(key).is_none_or(|entry| entry.enabled)
    }

    pub fn preferences(&self, key: &str) -> Preferences {
        self.entries
            .get(key)
            .map(|entry| entry.preferences.clone())
            .unwrap_or_default()
    }

    pub fn apply(&mut self, key: &str, changes: &[MemoryChange]) -> io::Result<()> {
        if !self.is_enabled(key) || changes.is_empty() {
            return Ok(());
        }
        let entry = self
            .entries
            .entry(key.to_owned())
            .or_insert_with(|| MemoryEntry {
                enabled: true,
                preferences: Preferences::default(),
            });
        let before = entry.preferences.clone();
        for change in changes {
            let preferences = &mut entry.preferences;
            match (change.field.as_str(), change.action.as_str()) {
                ("interest", "add") => {
                    if preferences.interests.len() < 10
                        && !preferences
                            .interests
                            .iter()
                            .any(|item| item.to_lowercase() == change.value.to_lowercase())
                    {
                        preferences.interests.push(change.value.clone());
                    }
                }
                ("interest", "remove") => preferences
                    .interests
                    .retain(|item| item.to_lowercase() != change.value.to_lowercase()),
                ("nickname", "set") => preferences.nickname = Some(change.value.clone()),
                ("nickname", "remove") => preferences.nickname = None,
                ("pronouns", "set") => preferences.pronouns = Some(change.value.clone()),
                ("pronouns", "remove") => preferences.pronouns = None,
                ("reply_style", "set") => preferences.reply_style = Some(change.value.clone()),
                ("reply_style", "remove") => preferences.reply_style = None,
                _ => {}
            }
        }
        if entry.preferences != before {
            save_json(&self.path, &self.entries)?;
        }
        Ok(())
    }

    pub fn forget(&mut self, key: &str) -> io::Result<()> {
        if self.is_enabled(key) {
            self.entries.remove(key);
        } else {
            self.entries.insert(
                key.to_owned(),
                MemoryEntry {
                    enabled: false,
                    preferences: Preferences::default(),
                },
            );
        }
        save_json(&self.path, &self.entries)
    }

    pub fn set_enabled(&mut self, key: &str, enabled: bool) -> io::Result<()> {
        if enabled {
            if let Some(entry) = self.entries.get_mut(key) {
                entry.enabled = true;
                if entry.preferences.is_empty() {
                    self.entries.remove(key);
                }
            }
        } else {
            self.entries.insert(
                key.to_owned(),
                MemoryEntry {
                    enabled: false,
                    preferences: Preferences::default(),
                },
            );
        }
        save_json(&self.path, &self.entries)
    }
}

fn valid_member_key(key: &str) -> bool {
    key.split_once(':').is_some_and(|(guild, user)| {
        !guild.is_empty()
            && !user.is_empty()
            && guild.bytes().all(|b| b.is_ascii_digit())
            && user.bytes().all(|b| b.is_ascii_digit())
    })
}

fn sensitive_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"(?i)\b(?:nsfw|porn|kink|fetish|sexual|diagnos\w*|medical|suicid\w*|politic\w*|religio\w*|trauma|abuse|address|phone|email)\b").unwrap())
}

fn private_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"(?i)https?://|www\.|@|<[@#]|\b\d{5,}\b").unwrap())
}

fn validate_memory_value(field: &str, value: &Value) -> Option<String> {
    let limit = match field {
        "nickname" => 40,
        "pronouns" => 50,
        "interest" => 80,
        "reply_style" => 120,
        _ => return None,
    };
    let text = value.as_str()?.trim();
    if text.is_empty()
        || text.chars().count() > limit
        || text.chars().any(|c| (c as u32) < 32)
        || private_regex().is_match(text)
        || sensitive_regex().is_match(text)
    {
        return None;
    }
    Some(text.to_owned())
}

#[derive(Clone, Debug)]
pub struct MemoryChange {
    pub field: String,
    pub action: String,
    pub value: String,
}

pub fn parse_memory_changes(text: &str) -> Vec<MemoryChange> {
    let Ok(Value::Object(payload)) = serde_json::from_str::<Value>(text) else {
        return vec![];
    };
    let Some(items) = payload.get("changes").and_then(Value::as_array) else {
        return vec![];
    };
    items
        .iter()
        .take(6)
        .filter_map(|item| {
            let field = item.get("field")?.as_str()?;
            let action = item.get("action")?.as_str()?;
            let value = match (field, action) {
                ("interest", "add" | "remove")
                | ("nickname" | "pronouns" | "reply_style", "set") => {
                    validate_memory_value(field, item.get("value")?)?
                }
                ("nickname" | "pronouns" | "reply_style", "remove") => String::new(),
                _ => return None,
            };
            Some(MemoryChange {
                field: field.to_owned(),
                action: action.to_owned(),
                value,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn path(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!("knowsphere-{}-{name}", std::process::id()))
    }

    #[test]
    fn reads_existing_history_and_forgets_only_own_server() {
        let file = path("conversations.json");
        fs::write(&file, r#"{"1:10:2":[{"role":"user","content":"hi"},{"role":"assistant","content":"hello"}],"1:11:2":[{"role":"user","content":"hi"},{"role":"assistant","content":"hello"}],"2:10:2":[{"role":"user","content":"hi"},{"role":"assistant","content":"hello"}],"bad":[{"role":"user","content":"hi"}]}"#).unwrap();
        let mut store = ConversationStore::load(file.clone());
        assert_eq!(store.get("1:10:2").len(), 2);
        assert!(store.get("bad").is_empty());
        store.reset_member("1:2").unwrap();
        let reopened = ConversationStore::load(file.clone());
        assert!(reopened.get("1:10:2").is_empty());
        assert!(reopened.get("1:11:2").is_empty());
        assert_eq!(reopened.get("2:10:2").len(), 2);
        let _ = fs::remove_file(file);
    }

    #[test]
    fn memory_opt_out_and_validation_survive_restart() {
        let file = path("memories.json");
        let _ = fs::remove_file(&file);
        let mut store = MemoryStore::load(file.clone());
        let changes = parse_memory_changes(
            r#"{"changes":[{"field":"nickname","action":"set","value":"Momo"},{"field":"interest","action":"add","value":"crochet"},{"field":"interest","action":"add","value":"http://private.example"}]}"#,
        );
        store.apply("1:2", &changes).unwrap();
        assert_eq!(
            MemoryStore::load(file.clone())
                .preferences("1:2")
                .nickname
                .as_deref(),
            Some("Momo")
        );
        assert_eq!(store.preferences("1:2").interests, ["crochet"]);
        store.set_enabled("1:2", false).unwrap();
        store.apply("1:2", &changes).unwrap();
        let reopened = MemoryStore::load(file.clone());
        assert!(!reopened.is_enabled("1:2"));
        assert!(reopened.preferences("1:2").is_empty());
        let _ = fs::remove_file(file);
    }

    #[test]
    fn loads_legacy_opt_out_without_preferences_field() {
        let file = path("legacy-opt-out.json");
        fs::write(&file, r#"{"1:2":{"enabled":false}}"#).unwrap();
        let store = MemoryStore::load(file.clone());
        assert!(!store.is_enabled("1:2"));
        let _ = fs::remove_file(file);
    }
}
