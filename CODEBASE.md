# Codebase

KnownSphere is a guild Discord companion. Members talk to it with a prefix, a mention, a reply, or a wake name. It can also own one forum channel, where each new post is a chat thread bound to a model tag. Preferences and a short per-channel history are stored as JSON and reached through a local [MCP](https://modelcontextprotocol.io) stdio server.

The Cargo package is `knowsphere` (edition 2021). The crate is a binary only: `src/main.rs` declares every module. Repository: `https://github.com/kienfssc/TFVN-ask`.

## Processes

```
Discord gateway
      |
      v
knowsphere                 OpenAI-compatible POST /chat/completions
(serenity event loop) ---- read-only HTTPS tools (weather, quotes, dictionary, Wikipedia)
      |
      | stdio JSON-RPC, child is `knowsphere --memory-mcp`
      v
MemoryServer  --->  conversations JSON + memories JSON
```

`MemoryMcp::connect` in `src/mcp.rs` spawns `std::env::current_exe()` with `--memory-mcp`, performs `initialize`, sends `notifications/initialized`, and caches `tools/list`. Protocol version is `2025-11-25`. The child is killed when the parent drops it. The parent's `reqwest::Client` is for the chat API. The child builds its own client for skill tools.

If `FORUM_TYPE_CHANNEL` is set, startup resolves the channel, requires `ChannelType::Forum`, ensures model tags exist, and keeps a `ForumRuntime` on `State`. A bad forum id or a non-forum channel aborts the process.

## Modules

| File | Role |
| --- | --- |
| `src/main.rs` | Config, prompts, Discord handler, chat and report orchestration, message splitting |
| `src/forum.rs` | Forum tags, eligibility, shared thread context, title cleanup, activation file |
| `src/mcp.rs` | MCP server and client, tool catalog, scope injection |
| `src/storage.rs` | Conversation and memory JSON stores, preference validation |
| `src/skills.rs` | Weather, quotes, clock, dice, and dispatch into calculator and knowledge |
| `src/calculator.rs` | Arithmetic parser used by the `calculate` tool |
| `src/knowledge.rs` | Free Dictionary API and English Wikipedia search |

## Message dispatch

`Handler::handle_message` drops messages with no `guild_id` and messages from bots. Gateway intents are `GUILDS`, `GUILD_MESSAGES`, and `MESSAGE_CONTENT`.

1. When forum mode is on, take the lock `forum:{channel_id}`. If the channel is an eligible thread, `answer_forum_message` handles it and returns.
2. A message equal to `COMMAND_PREFIX` shows help.
3. A message that starts with `COMMAND_PREFIX` plus a space is a command.
4. A mention of the bot chats, with mention markup removed.
5. A reply to the bot chats. An empty reply becomes "Continue from the message I replied to." and skips the memory router.
6. A wake name at the start, optionally after hey/hi/hello/yo/ê/này/alo, chats. Matching is case-insensitive and requires a boundary.

Anything else is ignored.

Commands in `handle_command`:

| Command | Behavior |
| --- | --- |
| `help` | List commands, wake names, and memory phrases |
| `ask <text>` | Chat, including the reply chain as context |
| `new` | `conversation_reset` for this guild, channel, and user |
| `summarize [1-100]` | Report, default 10 messages |
| `actions [1-100]` | Explicit action items, default 30 |
| `decisions [1-100]` | Decisions and open questions, default 30 |
| anything else | Chat, same path as `ask` |

`!knowsphere` is the default prefix. Default wake names are `KnownSphere` and `TransGPT` (at most 12 names, each 2–32 letters, numbers, spaces, hyphens, or underscores).

Normal chat holds a mutex keyed by `{guild}:{user}` so one member's turns cannot interleave. Forum turns hold the thread lock instead, then share the same per-member cooldown of 8 seconds.

## Chat pipeline

`answer_prompt` builds a prompt from the reply chain (up to 8 referenced messages, oldest first) and the member's text. It runs the memory router when `remember` is true, then `generate_chat_response`.

`generate_chat_response`:

1. System prompt (`SYSTEM_PROMPT`).
2. `memory_context` for this guild, user, and channel. When memory is on and no history override is passed, stored history is included (at most 10 messages).
3. Saved preferences as a user message labeled untrusted data, when memory is on and any field is set.
4. Memory-router notes, when present.
5. The member prompt.
6. Up to four model rounds with `chat_tools`. Each round may contain at most four tool calls. Tool errors are returned to the model as `{"error":...}`. A finished answer that used `weather` and omitted the word `open-meteo` gets an Open-Meteo line appended.

The first Discord chunk replies to the member without pinging them. Later chunks are plain messages. Chunks are at most 1900 characters, split on a newline or space when one sits in the second half of the window.

On success, and when the router did not set `skip_history`, the bot calls `conversation_append` with the full prompt and the full response. Forum answers do not append. They pass a `history_override` built from the thread, so the JSON conversation file is not the forum transcript. Forum turns still run the memory router, so an explicit preference request in a forum post can update memory. Visible thread text is still sent as context while memory is off, because that text is already in the channel.

Reports do not use tools or memory. They send the model JSON lines of `{author, content, source_url}` from non-bot messages, capped at 16,000 characters, each content clipped to 600 characters. Mentions in those lines are replaced with display names before a second mention strip.

## Memory router

`route_memory` is a separate completion with `MEMORY_ROUTER_PROMPT` and the public memory tools. It reads at most six tool calls. `memory_forget` and `memory_off` set `skip_history`, and later calls in that batch are skipped. The router runs on the default `OPENAI_MODEL`. Guild and user ids are injected in the client; the model cannot address another member. A router failure still produces a chat reply, with a note that no memory action was confirmed.

Public tools and their effects:

| Tool | Effect |
| --- | --- |
| `memory_update` | Apply up to six validated changes. No-op if memory is off. |
| `memory_status` | Return `enabled` and the stored preferences. |
| `memory_forget` | Delete preferences if memory is on. If it is off, keep it off. Erase this member's history in this guild. |
| `memory_off` | Store `enabled: false`, clear preferences, erase this member's history in this guild. |
| `memory_on` | Allow saving again. An empty preference row is removed. |

Internal tools:

| Tool | Effect |
| --- | --- |
| `memory_context` | Preferences and channel history, or empties when memory is off. |
| `conversation_append` | Push a user/assistant pair. Refuses while memory is off. |
| `conversation_reset` | Drop one channel history. Used by the `new` command. |

`reset_member` drops keys of the form `{guild}:{channel}:{user}` for that guild and user only.

## Stores

Both files are pretty-printed JSON objects with a trailing newline. A missing file loads as empty. A file that is not a JSON object is logged and treated as empty. The next successful mutation rewrites it. Individual bad rows are skipped.

`CONVERSATION_STORE_PATH` defaults to `.knownsphere_conversations.json`. Keys are `{guild}:{channel}:{user}`. Values are alternating `user` and `assistant` messages with non-empty content. Odd lengths and role mismatches drop the whole key. On load, only the newest 10 messages are kept.

`MEMORY_STORE_PATH` defaults to `.knownsphere_memories.json`. Keys are `{guild}:{user}` of decimal snowflakes. Each value is `{enabled, preferences}`. Missing `preferences` is allowed so an older `{enabled:false}` file still opts the member out. Absence of a row means memory is on.

Preference limits:

| Field | Actions | Max |
| --- | --- | --- |
| `nickname` | set, remove | 40 characters |
| `pronouns` | set, remove | 50 |
| `interest` | add, remove | 80 each, at most 10, case-insensitive uniqueness |
| `reply_style` | set, remove | 120 |

`validate_memory_value` rejects empty strings, ASCII controls, the private-data regex (URLs, mentions, five-digit runs), and the sensitive-topic regex (sexual, medical, self-harm, politics, religion, trauma, abuse, address, phone, email).

## Chat tools

`skills::tool_definitions` is appended to `tools/list`. `chat_tools_from_available` exposes every tool whose name is not in `MEMORY_TOOLS`. Current skill tools:

| Tool | Source | Notes |
| --- | --- | --- |
| `weather` | Open-Meteo geocoding and forecast | City required. Optional `day` (`today`, `tomorrow`, or `YYYY-MM-DD`) and `hour` (0–23, requires `day`). Seven daily rows. Clothing notes and a planning note. Not an official warning. |
| `quote` | `https://dumbapis.com/quote` or `/daily/quote` | `kind` is `random` (default) or `daily`. No topic filter. Text capped at 800 characters. |
| `current_time` | Local clock | UTC RFC 3339, unix seconds, and a Discord `<t:unix:F>` timestamp. |
| `calculate` | `calculator::evaluate` | `+ - * /`, parentheses, decimals, scientific notation, leading signs. Length 1–200. Rejects non-finite results and division by zero. |
| `roll_dice` | `OsRng` | `count` 1–20 (default 1), `sides` 2–1000 (default 6), `modifier` −1000..1000 (default 0). |
| `define_word` | `api.dictionaryapi.dev` | English word, 1–60 letters plus space, hyphen, or apostrophe. Up to five definitions. |
| `search_encyclopedia` | English Wikipedia API | Query 2–120 characters. Up to three intro extracts. User-Agent `KnownSphereBot/{version}`. |

Skill HTTP calls time out after 10 seconds. Chat completions use a 90-second read budget plus 10 seconds of client timeout. The router and forum titles use 30 seconds. The MCP client waits 30 seconds for one response line.

## Forum mode

Operator steps live in `docs/forum-chat.md`. Runtime behavior:

- `parse_model_tags` orders `FORUM_MODELS` first, then appends `OPENAI_MODEL` unless it is already present (ASCII case-insensitive). Each name is 1–20 characters. The list cannot exceed 20 names.
- Startup preserves existing forum tags, including ids, moderation, and emoji, and appends missing model tags. Discord's 20-tag channel limit is checked before the edit.
- `selected_model` walks that environment order against the post's applied tags. Several model tags resolve to the earliest one in the environment list. No model tag selects `OPENAI_MODEL`, and the bot adds that tag when the post has fewer than five applied tags.
- A thread is eligible when it is a public thread under the configured forum and its snowflake timestamp is at least `activated_at_ms`. The timestamp is `(id >> 22) + 1_420_070_400_000`.
- `.knownsphere_forum_state.json` stores `forum_id`, `activated_at_ms`, and `titled_threads`. The same forum id reuses the activation time, so posts created after the first successful start stay eligible across restarts. A different forum id replaces the file and starts a new cutoff. Posts older than the cutoff are never enrolled.
- A forum message is handled when it has text, attachments, or both. Text, images, and files can be combined. Images (PNG, JPEG, GIF, WebP) and PDFs are downloaded from Discord CDN URLs and sent as `image_url` and `file` content parts. Text files are inlined and labeled untrusted data. Other types are named in the transcript. Caps: 4 images, 2 PDFs, 4 text files, 12 MB total; per file, 5 MB, 8 MB, and 256 KB. Text files are clipped to 8,000 characters. Newer attachments win when a cap is hit. Plain text stays a string content field.
- The memory router sees only the member's typed text. An attachment-only message does not run the router. File bytes are not a memory request.
- Context is the starter plus recent messages: at most 20 chat messages and 16,000 characters. Text-file bodies count toward that character cap; image and PDF bytes do not. The starter stays even when the cap trims the middle. Messages with only an attachment stay in context. Bot messages sort after the message they reference. Messages from other bots are dropped. Operational replies do not count as answers and do not enter context: prefixes `Please wait ` and `I couldn't reply just now:`, and the exact text `The command failed. Please try again later.`
- An opening post with no text is titled from its attachment names, without a model call.
- The channel cache remembers eligibility for up to 2048 channel ids, then clears.
- After the first answer, when the opening message has text, the default model writes a title (80 characters, one line, surrounding quotes and a leading `Title:` removed). The thread id is recorded so later messages do not rename it. Title failure is logged and does not fail the answer.
- The answer itself uses the thread's selected model. Title generation does not.

## Configuration

`Config::load` reads the environment. dotenvy loads `.env` from the working directory and ignores a missing file.

| Variable | Required | Default |
| --- | --- | --- |
| `DISCORD_TOKEN` | yes | |
| `OPENAI_API_KEY` | yes | |
| `OPENAI_MODEL` | no | `gpt-4o-mini` |
| `OPENAI_API_BASE_URL` | no | `https://api.openai.com/v1` |
| `OPENAI_BASE_URL` | no | fallback if `OPENAI_API_BASE_URL` is unset |
| `COMMAND_PREFIX` | no | `!knowsphere` |
| `WAKE_NAMES` | no | `KnownSphere,TransGPT` |
| `FORUM_TYPE_CHANNEL` | no | unset disables forum mode |
| `FORUM_MODELS` | no | only the default model tag |
| `CONVERSATION_STORE_PATH` | no | `.knownsphere_conversations.json` (MCP child) |
| `MEMORY_STORE_PATH` | no | `.knownsphere_memories.json` (MCP child) |

A base URL that already ends in `/chat/completions` is used as-is. Any other base gains that suffix. The API key is sent as a bearer token. HTTP 401, 403, 404, and 429 map to a one-line hint; other failures stay generic. Response bodies are not shown to the member.

## Packaging

`Dockerfile` builds with `rust:1.94-bookworm` and `cargo build --release --locked`, then copies the binary onto `debian:bookworm-slim`. The image installs CA certificates, creates user `knowsphere` (uid 10001, home `/data`), and sets both store paths under `/data`. The volume is `/data`. `.dockerignore` sends only the manifest, lockfile, Dockerfile, and `src/`.

`.github/workflows/build.yml` on `main` runs fmt, clippy (`-D warnings`), and `cargo test --locked`. Pushes also build Windows and macOS release artifacts and push `ghcr.io/<repo>:main` and `:sha-<commit>`.

Git ignores `.env`, the three `.knownsphere_*.json` files, `*.json.tmp`, and `target/`.
