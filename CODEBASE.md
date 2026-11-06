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
| `src/skills.rs` | Weather, quotes, clock, dice, charts, exports, and dispatch into calculator, knowledge, plot, and export |
| `src/calculator.rs` | Arithmetic parser used by the `calculate` and `plot` tools |
| `src/plot.rs` | Line, bar, and scatter sampling. `src/plot.py` draws the PNG with matplotlib |
| `src/export.rs` | Markdown bundle for an Excel sheet, note, text, or source the member asked to download |
| `src/knowledge.rs` | Free Dictionary API and English Wikipedia search |

## Message dispatch

`Handler::handle_message` drops messages with no `guild_id` and messages from bots. Gateway intents are `GUILDS`, `GUILD_MESSAGES`, `MESSAGE_CONTENT`, and `GUILD_MESSAGE_REACTIONS`.

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
7. `retry_in_order` walks `EndpointSet::attempts`. The preferred model is tried first, on each endpoint that lists it, then the other configured models, at most four attempts. `FALLBACK_MODEL`, set to `provider/model`, replaces that other-model walk with that one pair. Transport failures, HTTP errors, empty completions, and tool-loop failures try the next attempt. Chat tools are read-only, so a failed attempt starts again from the original messages.

The Discord reply ends with `provider/model` for the attempt that answered, for example `openai/gpt-4o-mini`. That label is not stored in conversation history. The first Discord chunk replies to the member without pinging them and gets a 🔄 reaction and a Change model button. Later chunks are plain messages. Chunks are at most 1900 characters, split on a newline or space when one sits in the second half of the window. The first chunk also carries any chart images and export files. Export bytes are removed from the tool result before the model sees it, and conversation history stores the caption.

The member who sent the original message can add that 🔄 reaction once. The bot removes its reaction from that answer and posts a new one with its own 🔄. Another try uses the new answer's reaction. The bot's own reaction does not count, and the reaction only works on a message the bot itself marked. If the retry does not post a new answer, the same 🔄 is put back. Cooldown and the per-member lock still apply. In a forum thread the retry skips the one-answer guard and posts another reply. Operational replies, help, and errors do not get the reaction. The bot needs the **Add Reactions** permission.

The same member can press Change model. The bot then sends "Which model should I use?" with a model menu. In a forum thread the menu lists that forum's model tags, showing the short tag and sending the full `provider/model`. Elsewhere it lists up to 25 configured `provider/model` pairs. Choosing one posts another answer with that pair and clears the menu. The post's tags stay as they are. Cooldown and the per-member lock still apply, and the same answer can be changed again after the new one is posted.

On success, and when the router did not set `skip_history`, the bot calls `conversation_append` with the full prompt and the full response. Forum answers do not append. They pass a `history_override` built from the thread, so the JSON conversation file is not the forum transcript. Forum turns still run the memory router, so an explicit preference request in a forum post can update memory. Visible thread text is still sent as context while memory is off, because that text is already in the channel.

Reports do not use tools or memory. They send the model JSON lines of `{author, content, source_url}` from non-bot messages, capped at 16,000 characters, each content clipped to 600 characters. Mentions in those lines are replaced with display names before a second mention strip. The report reply uses the same endpoint retry order and ends with `provider/model`.

## Memory router

`route_memory` is a separate completion with `MEMORY_ROUTER_PROMPT` and the public memory tools. It reads at most six tool calls. `memory_forget` and `memory_off` set `skip_history`, and later calls in that batch are skipped. The router runs on the default model, using the same endpoint retry order. Guild and user ids are injected in the client; the model cannot address another member. A router failure still produces a chat reply, with a note that no memory action was confirmed.

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
| `calculate` | `calculator::evaluate` | `+ - * /`, parentheses, decimals, scientific notation, leading signs, and postfix factorial `!`. Length 1–200. Rejects non-finite results, division by zero, and factorial of a negative integer. |
| `plot` | Matplotlib PNG in the Docker image | `kind` is `line` (default), `bar`, or `scatter`. One to three series (a bar chart takes one), each 1–30 finite points. Or an expression in `x` with `x_min` and `x_max`, sampled 2–30 times (default 21). `x!` is factorial, extended with the gamma function between integers. Python only receives the sampled points. The bot attaches `plot.png` and the model writes a caption. Without `python3` and matplotlib the tool returns "The chart renderer is unavailable." |
| `roll_dice` | `OsRng` | `count` 1–20 (default 1), `sides` 2–1000 (default 6), `modifier` −1000..1000 (default 0). |
| `define_word` | `api.dictionaryapi.dev` | English word, 1–60 letters plus space, hyphen, or apostrophe. Up to five definitions. |
| `search_encyclopedia` | English Wikipedia API | Query 2–120 characters. Up to three intro extracts. User-Agent `KnownSphereBot/{version}`. |
| `export_file` | Local Markdown bundle | One `.md` attachment. Parts are `excel` (a table plus a CSV block), `markdown`, `text`, or `code`, at most 8. The file follows the requested text, up to 8 MiB, and is not clipped to the Discord message limit. A single markdown part is the file body. |

Skill HTTP calls time out after 10 seconds. Each chat attempt uses a 90-second read budget plus 10 seconds of client timeout. The router and forum titles use 30 seconds per attempt. The MCP client waits 30 seconds for one response line.

## Forum mode

Operator steps live in `docs/forum-chat.md`. Runtime behavior:

- `parse_model_tags` orders `FORUM_MODELS` first, then appends `OPENAI_MODEL` unless it is already present (ASCII case-insensitive). Each requested name is 1–20 characters. `.` and `-` match. A unique longer catalog id matches when it continues after that name with a hyphen. `forum_tags` registers one Discord tag per endpoint that lists that model. The tag is `provider/model`. When that is longer than 20 characters, the tag drops a leading word shared with the provider and a trailing `YYYYMMDD`. The selected value stays the full `provider/model`. The expanded list cannot exceed 20 tags.
- Startup replaces the forum's available tags with the environment list. Tags that are not in that list are deleted. When the names already match in that order, the existing tag ids stay. Discord's 20-tag channel limit is checked before the edit.
- `selected_model` walks that tag order against the post's applied tags. Several model tags resolve to the earliest one. No model tag selects the default `provider/model`, and the bot adds that tag when the post has fewer than five applied tags. The selected tag is tried first. When that provider is not the endpoint named `openai`, failure continues on the `openai` endpoint, or on the first endpoint when no endpoint has that name: the same model, then that endpoint's other models. `FALLBACK_MODEL` replaces those other models with its `provider/model` pair. Other providers are not tried. A bare model name, used outside forum tags, still walks every endpoint that lists it, then the same fallback.
- A thread is eligible when it is a public thread under the configured forum and its snowflake timestamp is at least `activated_at_ms`. The timestamp is `(id >> 22) + 1_420_070_400_000`.
- `.knownsphere_forum_state.json` stores `forum_id`, `activated_at_ms`, and `titled_threads`. The same forum id reuses the activation time, so posts created after the first successful start stay eligible across restarts. A different forum id replaces the file and starts a new cutoff. Posts older than the cutoff are never enrolled.
- A forum message is handled when it has text, attachments, or both. Text, images, and files can be combined. Images (PNG, JPEG, GIF, WebP) and PDFs are downloaded from Discord CDN URLs and sent as `image_url` and `file` content parts. Text files are inlined and labeled untrusted data. Other types are named in the transcript. Caps: 4 images, 2 PDFs, 4 text files, 12 MB total; per file, 5 MB, 8 MB, and 256 KB. Text files are clipped to 8,000 characters. Newer attachments win when a cap is hit. Plain text stays a string content field.
- The memory router sees only the member's typed text. An attachment-only message does not run the router. File bytes are not a memory request.
- Context is the starter plus recent messages: at most 20 chat messages and 16,000 characters. Text-file bodies count toward that character cap; image and PDF bytes do not. The starter stays even when the cap trims the middle. Messages with only an attachment stay in context. Bot messages sort after the message they reference. Messages from other bots are dropped. Operational replies do not count as answers and do not enter context: prefixes `Please wait `, `I couldn't reply just now:`, and `Rewriting that answer with `, plus the exact texts `The command failed. Please try again later.` and `Which model should I use?`
- An opening post with no text is titled from its attachment names, without a model call.
- The channel cache remembers eligibility for up to 2048 channel ids, then clears.
- After the first answer, when the opening message has text, the default model writes a title (80 characters, one line, surrounding quotes and a leading `Title:` removed). The thread id is recorded so later messages do not rename it. Title failure is logged and does not fail the answer.
- The answer itself uses the thread's selected `provider/model` tag, then the OpenAI fallback described above. The reply ends with `provider/model`. Pressing Change model posts another answer with the picked `provider/model` and does not change the post's tags. Title generation and the memory router stay on the default model. The title stays plain text.

## Configuration

`Config::load` reads the environment. dotenvy loads `.env` from the working directory and ignores a missing file.

| Variable | Required | Default |
| --- | --- | --- |
| `DISCORD_TOKEN` | yes | |
| `ENDPOINTS_CONFIG` | no | `endpoints.json` when that file exists; otherwise the `OPENAI_*` variables |
| `OPENAI_API_KEY` | yes, unless the JSON config supplies keys | |
| `OPENAI_MODEL` | no | `gpt-4o-mini`, or the first listed model when the JSON config is used and this variable is unset |
| `FALLBACK_MODEL` | no | the other configured models, up to four attempts |
| `OPENAI_API_BASE_URL` | no | `https://api.openai.com/v1` |
| `OPENAI_BASE_URL` | no | fallback if `OPENAI_API_BASE_URL` is unset |
| `OPENAI_PROVIDER` | no | host label, used only for the single env endpoint |
| `COMMAND_PREFIX` | no | `!knowsphere` |
| `WAKE_NAMES` | no | `KnownSphere,TransGPT` |
| `FORUM_TYPE_CHANNEL` | no | unset disables forum mode |
| `FORUM_MODELS` | no | only the default model tag |
| `CONVERSATION_STORE_PATH` | no | `.knownsphere_conversations.json` (MCP child) |
| `MEMORY_STORE_PATH` | no | `.knownsphere_memories.json` (MCP child) |

`endpoints.json`, or the file named by `ENDPOINTS_CONFIG`, lists ordered endpoints. `endpoints.example.json` is the shape. Each endpoint has `name`, `base_url`, `models`, and exactly one of `api_key` or `api_key_env`. Names are 1–40 letters, digits, dots, underscores, or hyphens. Model ids are 1–128 visible ASCII characters without spaces. At most 8 endpoints and 64 models each. A base URL that already ends in `/chat/completions` is used as-is. Any other base gains that suffix. The API key is sent as a bearer token. When `OPENAI_MODEL` is set, that model must be listed and becomes the default. When the file is absent, one endpoint is built from the `OPENAI_*` variables, and its provider name is `OPENAI_PROVIDER` or the host label (`api.openai.com` becomes `openai`). Each forum model must be listed on some endpoint. Its tag is `provider/model`. A failed backup tag continues on the endpoint named `openai`, then that endpoint's other models. `FALLBACK_MODEL` must be a listed `provider/model` pair and replaces those other models. In Docker the working directory is `/data`, so the file on the volume is picked up there.

HTTP 401, 403, 404, and 429 map to a one-line hint; other failures stay generic. Response bodies are not shown to the member. A single failed attempt returns that hint. When more than one attempt was tried, the member sees `Every configured endpoint failed. Last error: ...`. Startup logs the attempt labels and no keys.

## Packaging

`Dockerfile` builds with `rust:1.94-bookworm` and `cargo build --release --locked`, then copies the binary onto `debian:bookworm-slim`. The image installs CA certificates, Python 3, matplotlib, and DejaVu fonts, creates user `knowsphere` (uid 10001, home `/data`), and sets both store paths under `/data`. `MPLBACKEND` is `Agg`. The volume is `/data`. `.dockerignore` sends only the manifest, lockfile, Dockerfile, and `src/`. The chart script is compiled into the binary from `src/plot.py`.

`.github/workflows/build.yml` on `main` runs fmt, clippy (`-D warnings`), and `cargo test --locked`. Pushes also build Windows and macOS release artifacts and push `ghcr.io/<repo>:main` and `:sha-<commit>`.

Git ignores `.env`, `endpoints.json`, the three `.knownsphere_*.json` files, `*.json.tmp`, and `target/`.
