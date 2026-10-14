use crate::storage::ChatMessage;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use serenity::all::{ChannelId, ChannelType, ForumTag, ForumTagId, GuildChannel, Message};
use serenity::http::Http;
use std::collections::{BTreeSet, HashMap};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::sync::Mutex;

const DISCORD_EPOCH_MS: u64 = 1_420_070_400_000;
const MAX_CONTEXT_MESSAGES: usize = 20;
const MAX_CONTEXT_CHARS: usize = 16_000;
const STATE_PATH: &str = ".knownsphere_forum_state.json";
const MAX_FORUM_TAGS: usize = 20;
const MAX_TAG_NAME_CHARS: usize = 20;

pub fn parse_model_tags(
    default_model: &str,
    extra_models: Option<&str>,
) -> Result<Vec<String>, String> {
    let mut models = vec![default_model.to_owned()];
    if let Some(extra_models) = extra_models
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        models.extend(extra_models.split(',').map(|model| model.trim().to_owned()));
    }
    for model in &models {
        if model.is_empty() || model.chars().count() > MAX_TAG_NAME_CHARS {
            return Err(format!(
                "Forum model names must contain 1 to {MAX_TAG_NAME_CHARS} characters: {model:?}"
            ));
        }
    }
    let mut unique = Vec::new();
    for model in models {
        if !unique
            .iter()
            .any(|item: &String| item.eq_ignore_ascii_case(&model))
        {
            unique.push(model);
        }
    }
    if unique.len() > MAX_FORUM_TAGS {
        return Err(format!(
            "A Discord forum supports at most {MAX_FORUM_TAGS} model tags"
        ));
    }
    Ok(unique)
}

fn model_tag_payload(existing: &[ForumTag], models: &[String]) -> Result<Option<Value>, String> {
    let missing = models
        .iter()
        .filter(|model| {
            !existing
                .iter()
                .any(|tag| tag.name.eq_ignore_ascii_case(model))
        })
        .collect::<Vec<_>>();
    if missing.is_empty() {
        return Ok(None);
    }
    if existing.len() + missing.len() > MAX_FORUM_TAGS {
        return Err(format!(
            "The forum has {} tags and needs {} more model tags, exceeding Discord's limit of {MAX_FORUM_TAGS}",
            existing.len(), missing.len()
        ));
    }
    // Include existing IDs, moderation settings, and emoji so Discord keeps those tags intact.
    let mut tags = existing
        .iter()
        .map(|tag| serde_json::to_value(tag).map_err(|error| error.to_string()))
        .collect::<Result<Vec<_>, _>>()?;
    tags.extend(missing.into_iter().map(|model| json!({"name": model})));
    Ok(Some(json!({"available_tags": tags})))
}

pub fn parse_channel_id(value: Option<&str>) -> Result<Option<ChannelId>, String> {
    let Some(value) = value.map(str::trim).filter(|value| !value.is_empty()) else {
        return Ok(None);
    };
    let id = value
        .parse::<u64>()
        .ok()
        .filter(|id| *id > 0)
        .ok_or("FORUM_TYPE_CHANNEL must be a positive Discord channel ID")?;
    Ok(Some(ChannelId::new(id)))
}

fn snowflake_time_ms(id: ChannelId) -> u64 {
    (id.get() >> 22) + DISCORD_EPOCH_MS
}

pub fn is_eligible_thread(
    thread: &GuildChannel,
    forum_id: ChannelId,
    activated_at_ms: u64,
) -> bool {
    thread.kind == ChannelType::PublicThread
        && thread.parent_id == Some(forum_id)
        && snowflake_time_ms(thread.id) >= activated_at_ms
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct ForumState {
    forum_id: u64,
    activated_at_ms: u64,
    #[serde(default)]
    titled_threads: BTreeSet<u64>,
}

impl ForumState {
    fn load_or_create(path: &Path, forum_id: ChannelId, now_ms: u64) -> Result<Self, String> {
        let existing = match fs::read(path) {
            Ok(bytes) => Some(
                serde_json::from_slice::<Self>(&bytes)
                    .map_err(|error| format!("Could not parse {}: {error}", path.display()))?,
            ),
            Err(error) if error.kind() == io::ErrorKind::NotFound => None,
            Err(error) => return Err(format!("Could not read {}: {error}", path.display())),
        };
        match existing {
            Some(state) if state.forum_id == forum_id.get() && state.activated_at_ms > 0 => {
                Ok(state)
            }
            _ => {
                let state = Self {
                    forum_id: forum_id.get(),
                    activated_at_ms: now_ms,
                    titled_threads: BTreeSet::new(),
                };
                state
                    .save(path)
                    .map_err(|error| format!("Could not save {}: {error}", path.display()))?;
                Ok(state)
            }
        }
    }

    fn save(&self, path: &Path) -> io::Result<()> {
        let mut temp = path.as_os_str().to_os_string();
        temp.push(".tmp");
        let temp = PathBuf::from(temp);
        let mut bytes = serde_json::to_vec_pretty(self)?;
        bytes.push(b'\n');
        fs::write(&temp, bytes)?;
        fs::rename(temp, path)
    }
}

pub struct ForumRuntime {
    pub forum_id: ChannelId,
    pub activated_at_ms: u64,
    state: Mutex<ForumState>,
    path: PathBuf,
    model_tags: HashMap<ForumTagId, String>,
    default_model_tag: Option<ForumTagId>,
}

impl ForumRuntime {
    pub fn load_or_create(forum_id: ChannelId) -> Result<Self, String> {
        let path = PathBuf::from(STATE_PATH);
        let activated_at_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|error| error.to_string())?
            .as_millis() as u64;
        Self::load_or_create_at(forum_id, path, activated_at_ms)
    }

    fn load_or_create_at(forum_id: ChannelId, path: PathBuf, now_ms: u64) -> Result<Self, String> {
        let state = ForumState::load_or_create(&path, forum_id, now_ms)?;
        Ok(Self {
            forum_id,
            activated_at_ms: state.activated_at_ms,
            state: Mutex::new(state),
            path,
            model_tags: HashMap::new(),
            default_model_tag: None,
        })
    }

    pub async fn init_model_tags(
        &mut self,
        http: &Http,
        forum: GuildChannel,
        models: &[String],
    ) -> Result<(), String> {
        if models.is_empty() {
            return Err("At least one forum model is required".into());
        }
        let updated = match model_tag_payload(&forum.available_tags, models)? {
            Some(payload) => http
                .edit_channel(self.forum_id, &payload, None)
                .await
                .map_err(|error| format!("Could not initialize forum model tags: {error}"))?,
            None => forum,
        };
        self.model_tags.clear();
        for model in models {
            let tag = updated
                .available_tags
                .iter()
                .find(|tag| tag.name.eq_ignore_ascii_case(model))
                .ok_or_else(|| format!("Discord did not return the forum tag for {model}"))?;
            self.model_tags.insert(tag.id, model.clone());
        }
        self.default_model_tag = updated
            .available_tags
            .iter()
            .find(|tag| tag.name.eq_ignore_ascii_case(&models[0]))
            .map(|tag| tag.id);
        Ok(())
    }

    pub fn selected_model(
        &self,
        thread: &GuildChannel,
    ) -> Result<(&str, Option<ForumTagId>), String> {
        let mut selected = thread
            .applied_tags
            .iter()
            .filter_map(|id| self.model_tags.get(id));
        if let Some(model) = selected.next() {
            if selected.next().is_some() {
                return Err("Choose only one model tag for this post.".into());
            }
            return Ok((model, None));
        }
        let default_tag = self
            .default_model_tag
            .ok_or("Forum model tags are not initialized")?;
        let model = self
            .model_tags
            .get(&default_tag)
            .ok_or("Default forum model tag is missing")?;
        Ok((model, Some(default_tag)))
    }

    pub async fn is_titled(&self, thread_id: ChannelId) -> bool {
        self.state
            .lock()
            .await
            .titled_threads
            .contains(&thread_id.get())
    }

    pub async fn mark_titled(&self, thread_id: ChannelId) -> io::Result<()> {
        let mut state = self.state.lock().await;
        if state.titled_threads.insert(thread_id.get()) {
            if let Err(error) = state.save(&self.path) {
                state.titled_threads.remove(&thread_id.get());
                return Err(error);
            }
        }
        Ok(())
    }
}

#[derive(Clone, Debug)]
pub struct ForumMessage {
    pub id: u64,
    pub author_id: u64,
    pub author_name: String,
    pub is_bot: bool,
    pub reference_id: Option<u64>,
    pub content: String,
}

fn is_operational_reply(content: &str) -> bool {
    content.starts_with("Please wait ")
        || content.starts_with("I couldn't reply just now:")
        || content == "The command failed. Please try again later."
}

pub fn has_answer_for(message_id: u64, recent: &[ForumMessage], bot_id: u64) -> bool {
    recent.iter().any(|item| {
        item.author_id == bot_id
            && item.reference_id == Some(message_id)
            && !is_operational_reply(&item.content)
    })
}

impl From<&Message> for ForumMessage {
    fn from(message: &Message) -> Self {
        Self {
            id: message.id.get(),
            author_id: message.author.id.get(),
            author_name: message
                .member
                .as_ref()
                .and_then(|member| member.nick.clone())
                .unwrap_or_else(|| message.author.display_name().to_owned()),
            is_bot: message.author.bot,
            reference_id: message
                .message_reference
                .as_ref()
                .and_then(|reference| reference.message_id)
                .map(|id| id.get()),
            content: message.content.trim().to_owned(),
        }
    }
}

pub fn build_context(
    current_id: u64,
    starter: &ForumMessage,
    recent: &[ForumMessage],
    bot_id: u64,
) -> Vec<ChatMessage> {
    let mut items = recent.to_vec();
    if !items.iter().any(|item| item.id == starter.id) {
        items.push(starter.clone());
    }
    items.retain(|item| {
        if item.content.is_empty() || item.id == current_id {
            return false;
        }
        if item.author_id == bot_id {
            return item.reference_id.is_some_and(|id| id < current_id)
                && !is_operational_reply(&item.content);
        }
        !item.is_bot && item.id < current_id
    });
    // Bot replies may have been posted after the next user comment. Place each reply
    // after the question it references so rapid comments still see the right order.
    items.sort_by_key(|item| {
        if item.author_id == bot_id {
            (item.reference_id.unwrap_or(item.id), 1, item.id)
        } else {
            (item.id, 0, item.id)
        }
    });
    items.dedup_by_key(|item| item.id);

    let starter_context = if starter.id < current_id && !starter.content.is_empty() {
        Some(ChatMessage::new(
            "user",
            format!("{}: {}", starter.author_name, starter.content),
        ))
    } else {
        None
    };
    items.retain(|item| item.id != starter.id);
    let mut selected = Vec::new();
    let mut used = starter_context
        .as_ref()
        .map_or(0, |message| message.content.chars().count());
    for item in items.into_iter().rev() {
        if selected.len() + usize::from(starter_context.is_some()) >= MAX_CONTEXT_MESSAGES {
            break;
        }
        let content = if item.author_id == bot_id {
            item.content
        } else {
            format!("{}: {}", item.author_name, item.content)
        };
        let size = content.chars().count();
        if used + size > MAX_CONTEXT_CHARS {
            continue;
        }
        used += size;
        selected.push(ChatMessage::new(
            if item.author_id == bot_id {
                "assistant"
            } else {
                "user"
            },
            content,
        ));
    }
    selected.reverse();
    if let Some(starter) = starter_context {
        selected.insert(0, starter);
    }
    selected
}

pub fn clean_title(raw: &str, fallback_question: &str) -> String {
    fn normalized(text: &str) -> String {
        let one_line = text.lines().next().unwrap_or("");
        let one_line = one_line
            .trim()
            .trim_start_matches(['#', '*', '-', ' '])
            .trim_start_matches("Title:")
            .trim_matches(['"', '\'', '`', ' ']);
        one_line
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
            .chars()
            .take(80)
            .collect::<String>()
    }
    let title = normalized(raw);
    if title.chars().count() >= 2 {
        title
    } else {
        let fallback = normalized(fallback_question);
        if fallback.chars().count() >= 2 {
            fallback
        } else {
            "New chat".to_owned()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(
        id: u64,
        author_id: u64,
        bot: bool,
        reference_id: Option<u64>,
        content: &str,
    ) -> ForumMessage {
        ForumMessage {
            id,
            author_id,
            author_name: format!("user-{author_id}"),
            is_bot: bot,
            reference_id,
            content: content.into(),
        }
    }

    #[test]
    fn forum_id_must_be_positive() {
        assert!(parse_channel_id(None).unwrap().is_none());
        assert!(parse_channel_id(Some("  ")).unwrap().is_none());
        assert_eq!(parse_channel_id(Some("123")).unwrap().unwrap().get(), 123);
        assert!(parse_channel_id(Some("bad")).is_err());
        assert!(parse_channel_id(Some("0")).is_err());
    }

    #[test]
    fn model_tags_validate_names_and_keep_default_first() {
        assert_eq!(
            parse_model_tags("gpt-6-ultra", Some(" claude-opus-5.5, GPT-6-ULTRA ")).unwrap(),
            ["gpt-6-ultra", "claude-opus-5.5"]
        );
        assert!(
            parse_model_tags("gpt-6-ultra", Some("model-name-over-twenty-characters")).is_err()
        );
        assert!(parse_model_tags("gpt-6-ultra", Some("model,,other")).is_err());
        assert_eq!(
            parse_model_tags("gpt-6-ultra", Some(" ")).unwrap(),
            ["gpt-6-ultra"]
        );
    }

    #[test]
    fn adding_model_tags_preserves_existing_ids_and_settings() {
        let existing: ForumTag = serde_json::from_value(json!({
            "id": "123", "name": "Community", "moderated": true,
            "emoji_id": null, "emoji_name": "🐱"
        }))
        .unwrap();
        let models = vec!["gpt-6-ultra".to_owned(), "claude-opus-5.5".to_owned()];
        let payload = model_tag_payload(&[existing.clone()], &models)
            .unwrap()
            .unwrap();
        let tags = payload["available_tags"].as_array().unwrap();
        assert_eq!(tags.len(), 3);
        assert_eq!(tags[0], serde_json::to_value(&existing).unwrap());
        assert_eq!(tags[1]["name"], "gpt-6-ultra");
        assert!(model_tag_payload(&[existing], &["community".into()])
            .unwrap()
            .is_none());
    }

    #[test]
    fn model_selection_uses_one_tag_or_the_default() {
        let default_tag = ForumTagId::new(1);
        let other_tag = ForumTagId::new(2);
        let runtime = ForumRuntime {
            forum_id: ChannelId::new(10),
            activated_at_ms: 1,
            state: Mutex::new(ForumState {
                forum_id: 10,
                activated_at_ms: 1,
                titled_threads: BTreeSet::new(),
            }),
            path: PathBuf::new(),
            model_tags: HashMap::from([
                (default_tag, "gpt-6-ultra".into()),
                (other_tag, "claude-opus-5.5".into()),
            ]),
            default_model_tag: Some(default_tag),
        };
        let mut thread = GuildChannel::default();
        assert_eq!(
            runtime.selected_model(&thread).unwrap(),
            ("gpt-6-ultra", Some(default_tag))
        );
        thread.applied_tags = vec![other_tag];
        assert_eq!(
            runtime.selected_model(&thread).unwrap(),
            ("claude-opus-5.5", None)
        );
        thread.applied_tags.push(default_tag);
        assert!(runtime.selected_model(&thread).is_err());
    }

    #[test]
    fn only_threads_in_the_forum_created_after_activation_are_eligible() {
        let forum_id = ChannelId::new(10);
        let cutoff = DISCORD_EPOCH_MS + 10_000;
        let mut thread = GuildChannel::default();
        thread.id = ChannelId::new(((cutoff - DISCORD_EPOCH_MS) << 22) + 1);
        thread.parent_id = Some(forum_id);
        thread.kind = ChannelType::PublicThread;
        assert!(is_eligible_thread(&thread, forum_id, cutoff));
        assert!(!is_eligible_thread(&thread, forum_id, cutoff + 1));
        assert!(!is_eligible_thread(&thread, ChannelId::new(11), cutoff));
        thread.kind = ChannelType::Text;
        assert!(!is_eligible_thread(&thread, forum_id, cutoff));
    }

    #[tokio::test]
    async fn activation_and_title_marker_survive_restart_and_reset_for_a_new_forum() {
        let path = std::env::temp_dir().join(format!(
            "knowsphere-forum-test-{}-{}.json",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let first = ForumRuntime::load_or_create_at(ChannelId::new(10), path.clone(), 100).unwrap();
        first.mark_titled(ChannelId::new(20)).await.unwrap();
        let reopened =
            ForumRuntime::load_or_create_at(ChannelId::new(10), path.clone(), 200).unwrap();
        assert_eq!(reopened.activated_at_ms, 100);
        assert!(reopened.is_titled(ChannelId::new(20)).await);
        let changed =
            ForumRuntime::load_or_create_at(ChannelId::new(11), path.clone(), 300).unwrap();
        assert_eq!(changed.activated_at_ms, 300);
        assert!(!changed.is_titled(ChannelId::new(20)).await);
        let _ = fs::remove_file(path);
    }

    #[test]
    fn context_is_shared_and_places_late_bot_replies_after_their_question() {
        let starter = item(10, 1, false, None, "First question");
        let recent = vec![
            item(40, 99, true, Some(20), "First answer"),
            item(30, 3, false, None, "Current comment"),
            item(20, 2, false, None, "Follow-up"),
            starter.clone(),
            item(50, 4, false, None, "Later comment"),
        ];
        let context = build_context(30, &starter, &recent, 99);
        assert_eq!(
            context
                .iter()
                .map(|item| item.role.as_str())
                .collect::<Vec<_>>(),
            ["user", "user", "assistant"]
        );
        assert_eq!(context[0].content, "user-1: First question");
        assert_eq!(context[1].content, "user-2: Follow-up");
        assert_eq!(context[2].content, "First answer");
    }

    #[test]
    fn original_question_stays_in_bounded_history() {
        let starter = item(10, 1, false, None, "Original topic");
        let recent = (11..50)
            .map(|id| item(id, 2, false, None, "More discussion"))
            .collect::<Vec<_>>();
        let context = build_context(50, &starter, &recent, 99);
        assert_eq!(context.len(), MAX_CONTEXT_MESSAGES);
        assert_eq!(context[0].content, "user-1: Original topic");
    }

    #[test]
    fn only_completed_bot_replies_count_as_answers() {
        let recent = vec![
            item(
                21,
                99,
                true,
                Some(10),
                "Please wait 2 more second(s) before your next request.",
            ),
            item(22, 99, true, Some(10), "I couldn't reply just now: timeout"),
            item(23, 98, true, Some(10), "Another bot's answer"),
        ];
        assert!(!has_answer_for(10, &recent, 99));
        let mut answered = recent;
        answered.push(item(24, 99, true, Some(10), "A real answer"));
        assert!(has_answer_for(10, &answered, 99));
    }

    #[test]
    fn title_is_short_and_has_a_safe_fallback() {
        assert_eq!(
            clean_title("# 'A short topic'\nIgnore me", "question"),
            "A short topic"
        );
        assert_eq!(clean_title("x", "What happened?"), "What happened?");
        assert_eq!(clean_title("", "?"), "New chat");
        assert_eq!(
            clean_title(&"a".repeat(200), "question").chars().count(),
            80
        );
    }
}
