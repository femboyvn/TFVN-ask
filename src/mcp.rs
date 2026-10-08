//! Local stdio MCP server and client for all durable bot state.

use crate::skills;
use crate::storage::{parse_memory_changes, ConversationStore, MemoryStore};
use serde_json::{json, Map, Value};
use std::io;
use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};
use tokio::sync::Mutex;

const PROTOCOL_VERSION: &str = "2025-11-25";
const PUBLIC_TOOLS: &[&str] = &[
    "memory_update",
    "memory_status",
    "memory_forget",
    "memory_off",
    "memory_on",
];
const MEMORY_TOOLS: &[&str] = &[
    "memory_context",
    "conversation_append",
    "conversation_reset",
    "memory_update",
    "memory_status",
    "memory_forget",
    "memory_off",
    "memory_on",
];

fn scope_schema(channel: bool) -> (Map<String, Value>, Vec<String>) {
    let mut properties = Map::new();
    let mut required = Vec::new();
    for name in if channel {
        &["guild_id", "user_id", "channel_id"][..]
    } else {
        &["guild_id", "user_id"][..]
    } {
        properties.insert((*name).to_owned(), json!({"type":"integer","minimum":1}));
        required.push((*name).to_owned());
    }
    (properties, required)
}

fn tool(name: &str, description: &str, extra: Value, channel: bool) -> Value {
    let (mut properties, mut required) = scope_schema(channel);
    if let Some(extra_properties) = extra.get("properties").and_then(Value::as_object) {
        properties.extend(extra_properties.clone());
    }
    if let Some(extra_required) = extra.get("required").and_then(Value::as_array) {
        required.extend(
            extra_required
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_owned),
        );
    }
    json!({
        "name": name,
        "description": description,
        "inputSchema": {"type":"object","properties":properties,"required":required,"additionalProperties":false}
    })
}

pub fn tools_list() -> Vec<Value> {
    let mut tools = vec![
        tool("memory_context", "Get current member preferences and conversation history for this channel.", json!({}), true),
        tool("conversation_append", "Save a completed conversation turn.", json!({
            "properties":{"prompt":{"type":"string"},"response":{"type":"string"}},
            "required":["prompt","response"]
        }), true),
        tool("conversation_reset", "Clear this member's conversation in this channel.", json!({}), true),
        tool("memory_update", "Save or remove a preference only when this member explicitly asks to remember, change, or remove it in their latest message. Only nickname, pronouns, interest, and reply style are allowed. Never save sensitive information.", json!({
            "properties":{"changes":{"type":"array","maxItems":6,"items":{"type":"object","properties":{
                "field":{"type":"string","enum":["nickname","pronouns","interest","reply_style"]},
                "action":{"type":"string","enum":["set","remove","add"]},
                "value":{"type":"string"}
            },"required":["field","action"],"additionalProperties":false}}},
            "required":["changes"]
        }), false),
        tool("memory_status", "Show whether memory is on and which preferences are saved for this member.", json!({}), false),
        tool("memory_forget", "Erase this member's saved preferences and chat history in this server, keeping memory enabled. Use only when they explicitly ask to forget or erase memory.", json!({}), false),
        tool("memory_off", "Erase this member's saved preferences and chat history and stop saving new ones. Use only when they explicitly ask to turn memory off.", json!({}), false),
        tool("memory_on", "Allow new preferences and chat history to be saved. Use only when the member explicitly asks to turn memory on.", json!({}), false),
    ];
    tools.extend(skills::tool_definitions());
    tools
}

pub struct MemoryServer {
    conversations: ConversationStore,
    memories: MemoryStore,
}

impl MemoryServer {
    pub fn new(conversation_path: PathBuf, memory_path: PathBuf) -> Self {
        Self {
            conversations: ConversationStore::load(conversation_path),
            memories: MemoryStore::load(memory_path),
        }
    }

    fn call(&mut self, name: &str, arguments: &Value) -> Result<Value, String> {
        let guild = arguments
            .get("guild_id")
            .and_then(Value::as_u64)
            .filter(|id| *id > 0)
            .ok_or("Missing guild_id")?;
        let user = arguments
            .get("user_id")
            .and_then(Value::as_u64)
            .filter(|id| *id > 0)
            .ok_or("Missing user_id")?;
        let member_key = format!("{guild}:{user}");
        let conversation_key = if matches!(
            name,
            "memory_context" | "conversation_append" | "conversation_reset"
        ) {
            let channel = arguments
                .get("channel_id")
                .and_then(Value::as_u64)
                .filter(|id| *id > 0)
                .ok_or("Missing channel_id")?;
            Some(format!("{guild}:{channel}:{user}"))
        } else {
            None
        };
        match name {
            "memory_context" => {
                let enabled = self.memories.is_enabled(&member_key);
                Ok(json!({
                    "enabled":enabled,
                    "preferences": if enabled { self.memories.preferences(&member_key) } else { Default::default() },
                    "history": if enabled { self.conversations.get(conversation_key.as_deref().unwrap()) } else { vec![] }
                }))
            }
            "conversation_append" => {
                if !self.memories.is_enabled(&member_key) {
                    return Ok(json!({"saved":false,"reason":"memory is off"}));
                }
                let prompt = arguments
                    .get("prompt")
                    .and_then(Value::as_str)
                    .ok_or("Missing prompt")?;
                let response = arguments
                    .get("response")
                    .and_then(Value::as_str)
                    .ok_or("Missing response")?;
                self.conversations
                    .append_turn(conversation_key.as_deref().unwrap(), prompt, response)
                    .map_err(|e| e.to_string())?;
                Ok(json!({"saved":true}))
            }
            "conversation_reset" => {
                self.conversations
                    .reset(conversation_key.as_deref().unwrap())
                    .map_err(|e| e.to_string())?;
                Ok(json!({"cleared":true}))
            }
            "memory_update" => {
                if !self.memories.is_enabled(&member_key) {
                    return Ok(json!({"saved":false,"reason":"memory is off"}));
                }
                let changes = parse_memory_changes(&arguments.to_string());
                if changes.is_empty() {
                    return Err("No valid preference changes supplied".into());
                }
                self.memories
                    .apply(&member_key, &changes)
                    .map_err(|e| e.to_string())?;
                Ok(json!({"saved":true,"preferences":self.memories.preferences(&member_key)}))
            }
            "memory_status" => Ok(
                json!({"enabled":self.memories.is_enabled(&member_key),"preferences":self.memories.preferences(&member_key)}),
            ),
            "memory_forget" => {
                self.memories
                    .forget(&member_key)
                    .map_err(|e| e.to_string())?;
                self.conversations
                    .reset_member(&member_key)
                    .map_err(|e| e.to_string())?;
                Ok(json!({"forgotten":true,"enabled":self.memories.is_enabled(&member_key)}))
            }
            "memory_off" => {
                self.memories
                    .set_enabled(&member_key, false)
                    .map_err(|e| e.to_string())?;
                self.conversations
                    .reset_member(&member_key)
                    .map_err(|e| e.to_string())?;
                Ok(json!({"enabled":false,"erased":true}))
            }
            "memory_on" => {
                self.memories
                    .set_enabled(&member_key, true)
                    .map_err(|e| e.to_string())?;
                Ok(json!({"enabled":true}))
            }
            _ => Err("Unknown tool".into()),
        }
    }
}

fn rpc_response(id: Value, result: Value) -> Value {
    json!({"jsonrpc":"2.0","id":id,"result":result})
}

fn rpc_error(id: Value, code: i64, message: &str) -> Value {
    json!({"jsonrpc":"2.0","id":id,"error":{"code":code,"message":message}})
}

fn tool_result(result: Result<Value, String>) -> Value {
    match result {
        Ok(value) => {
            json!({"content":[{"type":"text","text":value.to_string()}],"structuredContent":value,"isError":false})
        }
        Err(error) => json!({"content":[{"type":"text","text":error}],"isError":true}),
    }
}

async fn handle_rpc(
    server: &mut MemoryServer,
    skill_client: &reqwest::Client,
    initialized: &mut bool,
    request: &Value,
) -> Option<Value> {
    let id = request.get("id")?.clone();
    let method = request.get("method").and_then(Value::as_str).unwrap_or("");
    let result = match method {
        "initialize" => {
            json!({"protocolVersion":PROTOCOL_VERSION,"capabilities":{"tools":{"listChanged":false}},"serverInfo":{"name":"knowsphere-tools","version":env!("CARGO_PKG_VERSION")}})
        }
        "ping" => json!({}),
        "tools/list" if *initialized => json!({"tools":tools_list()}),
        "tools/call" if *initialized => {
            let params = request.get("params").unwrap_or(&Value::Null);
            let name = params.get("name").and_then(Value::as_str).unwrap_or("");
            let arguments = params.get("arguments").unwrap_or(&Value::Null);
            if MEMORY_TOOLS.contains(&name) {
                tool_result(server.call(name, arguments))
            } else {
                tool_result(skills::call(skill_client, name, arguments).await)
            }
        }
        _ => {
            return Some(rpc_error(
                id,
                -32601,
                "Method not found or server not initialized",
            ))
        }
    };
    Some(rpc_response(id, result))
}

pub async fn serve(conversation_path: PathBuf, memory_path: PathBuf) -> io::Result<()> {
    let mut server = MemoryServer::new(conversation_path, memory_path);
    let skill_client = reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(5))
        .build()
        .map_err(io::Error::other)?;
    let mut stdin = BufReader::new(tokio::io::stdin()).lines();
    let mut stdout = tokio::io::stdout();
    let mut initialized = false;
    while let Some(line) = stdin.next_line().await? {
        let response = match serde_json::from_str::<Value>(&line) {
            Ok(request) => {
                if request.get("method").and_then(Value::as_str)
                    == Some("notifications/initialized")
                {
                    initialized = true;
                    None
                } else {
                    handle_rpc(&mut server, &skill_client, &mut initialized, &request).await
                }
            }
            Err(_) => Some(rpc_error(Value::Null, -32700, "Parse error")),
        };
        if let Some(response) = response {
            stdout.write_all(response.to_string().as_bytes()).await?;
            stdout.write_all(b"\n").await?;
            stdout.flush().await?;
        }
    }
    Ok(())
}

struct Transport {
    _child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    next_id: u64,
}

impl Transport {
    async fn send(&mut self, value: &Value) -> Result<(), String> {
        self.stdin
            .write_all(value.to_string().as_bytes())
            .await
            .map_err(|e| e.to_string())?;
        self.stdin
            .write_all(b"\n")
            .await
            .map_err(|e| e.to_string())?;
        self.stdin.flush().await.map_err(|e| e.to_string())
    }

    async fn request(&mut self, method: &str, params: Value) -> Result<Value, String> {
        self.next_id += 1;
        let id = self.next_id;
        self.send(&json!({"jsonrpc":"2.0","id":id,"method":method,"params":params}))
            .await?;
        let mut line = String::new();
        let bytes = tokio::time::timeout(Duration::from_secs(30), self.stdout.read_line(&mut line))
            .await
            .map_err(|_| "MCP server timed out".to_owned())?
            .map_err(|e| e.to_string())?;
        if bytes == 0 {
            return Err("Memory MCP server closed its output".into());
        }
        let value: Value = serde_json::from_str(&line).map_err(|e| e.to_string())?;
        if value.get("id").and_then(Value::as_u64) != Some(id) {
            return Err("Memory MCP response ID mismatch".into());
        }
        if let Some(error) = value.get("error") {
            return Err(error.to_string());
        }
        value
            .get("result")
            .cloned()
            .ok_or("Memory MCP response missing result".into())
    }
}

pub struct MemoryMcp {
    transport: Mutex<Transport>,
    model_tools: Vec<Value>,
    chat_tools: Vec<Value>,
}

fn model_tools_from_available(
    available: &[Value],
    names: &[&str],
    strip_scope: bool,
) -> Result<Vec<Value>, String> {
    let mut model_tools = Vec::new();
    for name in names {
        let definition = available
            .iter()
            .find(|tool| tool.get("name").and_then(Value::as_str) == Some(name))
            .ok_or_else(|| format!("MCP missing tool {name}"))?;
        let mut schema = definition
            .get("inputSchema")
            .cloned()
            .ok_or("MCP tool missing inputSchema")?;
        if strip_scope {
            if let Some(properties) = schema.get_mut("properties").and_then(Value::as_object_mut) {
                properties.remove("guild_id");
                properties.remove("user_id");
                properties.remove("channel_id");
            }
            if let Some(required) = schema.get_mut("required").and_then(Value::as_array_mut) {
                required.retain(|item| {
                    !matches!(item.as_str(), Some("guild_id" | "user_id" | "channel_id"))
                });
            }
        }
        model_tools.push(json!({"type":"function","function":{
            "name":name,
            "description":definition.get("description").and_then(Value::as_str).unwrap_or(""),
            "parameters":schema
        }}));
    }
    Ok(model_tools)
}

fn chat_tools_from_available(available: &[Value]) -> Result<Vec<Value>, String> {
    let names = available
        .iter()
        .filter_map(|tool| tool.get("name").and_then(Value::as_str))
        .filter(|name| !MEMORY_TOOLS.contains(name))
        .collect::<Vec<_>>();
    model_tools_from_available(available, &names, false)
}

fn scoped_arguments(
    mut arguments: Value,
    guild: u64,
    user: u64,
    channel: Option<u64>,
) -> Result<Value, String> {
    let object = arguments
        .as_object_mut()
        .ok_or("MCP tool arguments must be an object")?;
    object.insert("guild_id".into(), json!(guild));
    object.insert("user_id".into(), json!(user));
    if let Some(channel) = channel {
        object.insert("channel_id".into(), json!(channel));
    }
    Ok(arguments)
}

impl MemoryMcp {
    pub async fn connect() -> Result<Self, String> {
        let exe = std::env::current_exe().map_err(|e| e.to_string())?;
        let mut child = Command::new(exe)
            .arg("--memory-mcp")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .kill_on_drop(true)
            .spawn()
            .map_err(|e| e.to_string())?;
        let stdin = child.stdin.take().ok_or("Missing memory MCP stdin")?;
        let stdout = child.stdout.take().ok_or("Missing memory MCP stdout")?;
        let mut transport = Transport {
            _child: child,
            stdin,
            stdout: BufReader::new(stdout),
            next_id: 0,
        };
        let initialized = transport
            .request(
                "initialize",
                json!({
                    "protocolVersion":PROTOCOL_VERSION,
                    "capabilities":{},
                    "clientInfo":{"name":"knowsphere-bot","version":env!("CARGO_PKG_VERSION")}
                }),
            )
            .await?;
        if initialized.get("protocolVersion").and_then(Value::as_str) != Some(PROTOCOL_VERSION) {
            return Err("Memory MCP protocol version mismatch".into());
        }
        transport
            .send(&json!({"jsonrpc":"2.0","method":"notifications/initialized"}))
            .await?;
        let listed = transport.request("tools/list", json!({})).await?;
        let available = listed
            .get("tools")
            .and_then(Value::as_array)
            .ok_or("Memory MCP tools/list returned no tools")?;
        let model_tools = model_tools_from_available(available, PUBLIC_TOOLS, true)?;
        let chat_tools = chat_tools_from_available(available)?;
        Ok(Self {
            transport: Mutex::new(transport),
            model_tools,
            chat_tools,
        })
    }

    pub fn model_tools(&self) -> &[Value] {
        &self.model_tools
    }

    pub fn chat_tools(&self) -> &[Value] {
        &self.chat_tools
    }

    pub async fn call(
        &self,
        name: &str,
        arguments: Value,
        guild: u64,
        user: u64,
        channel: Option<u64>,
    ) -> Result<Value, String> {
        let arguments = scoped_arguments(arguments, guild, user, channel)?;
        self.call_tool(name, arguments).await
    }

    pub async fn call_chat_tool(&self, name: &str, arguments: Value) -> Result<Value, String> {
        if !self
            .chat_tools
            .iter()
            .any(|tool| tool["function"]["name"] == name)
        {
            return Err("Unknown chat tool".into());
        }
        if !arguments.is_object() {
            return Err("MCP tool arguments must be an object".into());
        }
        self.call_tool(name, arguments).await
    }

    async fn call_tool(&self, name: &str, arguments: Value) -> Result<Value, String> {
        let result = self
            .transport
            .lock()
            .await
            .request("tools/call", json!({"name":name,"arguments":arguments}))
            .await?;
        if result.get("isError").and_then(Value::as_bool) == Some(true) {
            let message = result
                .get("content")
                .and_then(Value::as_array)
                .and_then(|items| items.first())
                .and_then(|item| item.get("text"))
                .and_then(Value::as_str)
                .unwrap_or("Memory MCP tool failed");
            return Err(message.to_owned());
        }
        result
            .get("structuredContent")
            .cloned()
            .ok_or("Memory MCP result missing structuredContent".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chat_tools_follow_mcp_catalog_without_exposing_memory() {
        let mut available = tools_list();
        available.push(json!({
            "name":"future_lookup",
            "description":"An additional read-only lookup",
            "inputSchema":{"type":"object","properties":{},"additionalProperties":false}
        }));
        let tools = chat_tools_from_available(&available).unwrap();
        let names = tools
            .iter()
            .map(|tool| tool["function"]["name"].as_str().unwrap())
            .collect::<Vec<_>>();
        assert_eq!(
            names,
            [
                "weather",
                "quote",
                "current_time",
                "calculate",
                "roll_dice",
                "define_word",
                "search_encyclopedia",
                "future_lookup"
            ]
        );
        assert_eq!(
            tools[0]["function"]["parameters"]["properties"]["day"]["type"],
            "string"
        );
    }

    #[test]
    fn model_cannot_choose_another_members_scope() {
        let tools = model_tools_from_available(&tools_list(), PUBLIC_TOOLS, true).unwrap();
        assert_eq!(tools.len(), PUBLIC_TOOLS.len());
        for tool in tools {
            let schema = &tool["function"]["parameters"];
            for name in ["guild_id", "user_id", "channel_id"] {
                assert!(schema["properties"].get(name).is_none());
                assert!(!schema["required"]
                    .as_array()
                    .unwrap()
                    .contains(&json!(name)));
            }
        }
        let args = scoped_arguments(
            json!({"guild_id":999,"user_id":999,"changes":[]}),
            1,
            2,
            None,
        )
        .unwrap();
        assert_eq!(args["guild_id"], 1);
        assert_eq!(args["user_id"], 2);
    }

    #[tokio::test]
    async fn rpc_lists_tools_and_scopes_memory() {
        let conversation_path = std::env::temp_dir().join(format!(
            "knowsphere-mcp-{}-history.json",
            std::process::id()
        ));
        let memory_path =
            std::env::temp_dir().join(format!("knowsphere-mcp-{}-memory.json", std::process::id()));
        let _ = std::fs::remove_file(&conversation_path);
        let _ = std::fs::remove_file(&memory_path);
        let mut server = MemoryServer::new(conversation_path.clone(), memory_path.clone());
        let skill_client = reqwest::Client::new();
        let mut initialized = false;
        let init = handle_rpc(
            &mut server,
            &skill_client,
            &mut initialized,
            &json!({"id":1,"method":"initialize"}),
        )
        .await
        .unwrap();
        assert_eq!(init["result"]["protocolVersion"], PROTOCOL_VERSION);
        initialized = true;
        let tools = handle_rpc(
            &mut server,
            &skill_client,
            &mut initialized,
            &json!({"id":2,"method":"tools/list"}),
        )
        .await
        .unwrap();
        assert_eq!(tools["result"]["tools"].as_array().unwrap().len(), 15);
        assert!(tools["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .any(|tool| tool["name"] == "quote"));
        let time = handle_rpc(
            &mut server,
            &skill_client,
            &mut initialized,
            &json!({"id":5,"method":"tools/call","params":{"name":"current_time","arguments":{}}}),
        )
        .await
        .unwrap();
        assert_eq!(time["result"]["isError"], false);
        assert!(time["result"]["structuredContent"]["unix_seconds"].is_i64());
        let invalid_lookup = handle_rpc(
            &mut server,
            &skill_client,
            &mut initialized,
            &json!({"id":6,"method":"tools/call","params":{"name":"define_word","arguments":{"word":"bad/word"}}}),
        )
        .await
        .unwrap();
        assert_eq!(invalid_lookup["result"]["isError"], true);
        let bad_quote = handle_rpc(&mut server, &skill_client, &mut initialized, &json!({"id":4,"method":"tools/call","params":{"name":"quote","arguments":{"kind":"unknown"}}})).await.unwrap();
        assert_eq!(bad_quote["result"]["isError"], true);
        let update = handle_rpc(&mut server, &skill_client, &mut initialized, &json!({"id":3,"method":"tools/call","params":{"name":"memory_update","arguments":{"guild_id":1,"user_id":2,"changes":[{"field":"nickname","action":"set","value":"Momo"}]}}})).await.unwrap();
        assert_eq!(update["result"]["structuredContent"]["saved"], true);
        let own = server
            .call("memory_status", &json!({"guild_id":1,"user_id":2}))
            .unwrap();
        let other = server
            .call("memory_status", &json!({"guild_id":1,"user_id":3}))
            .unwrap();
        assert_eq!(own["preferences"]["nickname"], "Momo");
        assert!(other["preferences"].get("nickname").is_none());
        server
            .call("memory_off", &json!({"guild_id":1,"user_id":2}))
            .unwrap();
        assert_eq!(
            server
                .call("memory_status", &json!({"guild_id":1,"user_id":2}))
                .unwrap()["enabled"],
            false
        );
        let _ = std::fs::remove_file(conversation_path);
        let _ = std::fs::remove_file(memory_path);
    }

    #[test]
    fn memory_opt_out_clears_all_member_channels_and_survives_restart() {
        let conversation_path = std::env::temp_dir().join(format!(
            "knowsphere-mcp-{}-opt-out-history.json",
            std::process::id()
        ));
        let memory_path = std::env::temp_dir().join(format!(
            "knowsphere-mcp-{}-opt-out-memory.json",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&conversation_path);
        let _ = std::fs::remove_file(&memory_path);

        let mut server = MemoryServer::new(conversation_path.clone(), memory_path.clone());
        let member = json!({"guild_id":1,"user_id":2});
        server
            .call(
                "memory_update",
                &json!({
                    "guild_id":1,"user_id":2,
                    "changes":[{"field":"nickname","action":"set","value":"Momo"}]
                }),
            )
            .unwrap();
        for channel in [10, 11] {
            assert_eq!(
                server
                    .call(
                        "conversation_append",
                        &json!({
                            "guild_id":1,"user_id":2,"channel_id":channel,
                            "prompt":"hello","response":"hi"
                        })
                    )
                    .unwrap()["saved"],
                true
            );
        }
        server
            .call(
                "conversation_append",
                &json!({
                    "guild_id":1,"user_id":3,"channel_id":10,
                    "prompt":"other member","response":"still here"
                }),
            )
            .unwrap();

        assert_eq!(
            server.call("memory_status", &member).unwrap()["preferences"]["nickname"],
            "Momo"
        );
        for channel in [10, 11] {
            assert_eq!(
                server
                    .call(
                        "memory_context",
                        &json!({
                            "guild_id":1,"user_id":2,"channel_id":channel
                        })
                    )
                    .unwrap()["history"]
                    .as_array()
                    .unwrap()
                    .len(),
                2
            );
        }

        assert_eq!(server.call("memory_off", &member).unwrap()["erased"], true);
        assert_eq!(
            server
                .call(
                    "conversation_append",
                    &json!({
                        "guild_id":1,"user_id":2,"channel_id":10,
                        "prompt":"do not save","response":"ok"
                    })
                )
                .unwrap()["saved"],
            false
        );
        drop(server);

        let mut reopened = MemoryServer::new(conversation_path.clone(), memory_path.clone());
        for channel in [10, 11] {
            let context = reopened
                .call(
                    "memory_context",
                    &json!({
                        "guild_id":1,"user_id":2,"channel_id":channel
                    }),
                )
                .unwrap();
            assert_eq!(context["enabled"], false);
            assert_eq!(context["preferences"], json!({}));
            assert_eq!(context["history"], json!([]));
        }
        let other = reopened
            .call(
                "memory_context",
                &json!({
                    "guild_id":1,"user_id":3,"channel_id":10
                }),
            )
            .unwrap();
        assert_eq!(other["history"][0]["content"], "other member");

        reopened.call("memory_on", &member).unwrap();
        let context = reopened
            .call(
                "memory_context",
                &json!({
                    "guild_id":1,"user_id":2,"channel_id":10
                }),
            )
            .unwrap();
        assert_eq!(context["enabled"], true);
        assert_eq!(context["preferences"], json!({}));
        assert_eq!(context["history"], json!([]));

        let _ = std::fs::remove_file(conversation_path);
        let _ = std::fs::remove_file(memory_path);
    }
}
