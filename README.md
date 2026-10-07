# KnownSphere

A cozy Discord companion for a femboy-friendly server. Members can mention the bot,
reply to it, use its configured command prefix, or start a message with a wake name such
as “TransGPT, what's the weather in Da Nang?” It does not answer other ordinary channel
messages or DMs.

## Run

Install Rust, configure `DISCORD_TOKEN` and `OPENAI_API_KEY` in `.env`, then run
`cargo run --release`. The bot uses an OpenAI-compatible Chat Completions endpoint
that supports function tool calls.
`OPENAI_API_BASE_URL`, `OPENAI_MODEL`, `COMMAND_PREFIX`, and `WAKE_NAMES` are optional.
The default prefix is `!knowsphere`. `WAKE_NAMES` is a comma-separated list; it defaults
to `KnownSphere,TransGPT`. For other names, set something like
`WAKE_NAMES=KnownSphere,TransGPT,Mochi` in `.env`. Set `WAKE_NAMES=` to disable wake names.
Names match at the start of a message, case-insensitively, with optional greetings such
as “hey,” “hi,” “ê,” “này,” or “alo” before them. For example, “ê TransGPT có gì hot”
works. Enable Discord's Message Content intent for the bot.

The old `ALLOWED_ROLE_ID` setting is ignored; any server member can chat with the bot.
Each AI reply has an eight-second per-member cooldown. The bot uses the member's latest
message to decide whether to call a memory tool before replying. Preferences are saved
only when the member explicitly asks.

## Commands

- `!knowsphere ask <message>` or `!knowsphere <message>`: chat.
- `!knowsphere new`: clear your recent conversation in this channel.
- `!knowsphere summarize [1-100]`: summarize recent channel messages.
- `!knowsphere help`: show help in Discord.

Memory is controlled by normal messages to the bot, such as “remember my nickname is
Mochi,” “what do you remember about me?”, “forget me,” or “turn memory off.” The bot
does not answer other ordinary channel messages, so mention it, reply to it, use a wake
name, or use the chat prefix when sending these requests. Memory requests are scoped to
the member's server.

Preferences are stored per server member in `.knownsphere_memories.json`. Recent
conversation turns are stored per member and channel in `.knownsphere_conversations.json`.
Both paths can be changed with `MEMORY_STORE_PATH` and `CONVERSATION_STORE_PATH`.
The bot starts a local stdio MCP server automatically. That server owns both JSON stores;
the bot accesses preferences and conversation history through MCP tools. It also exposes
these chat tools through the same MCP server:

- **Weather and plans:** Ask “What should I wear in Da Nang, Vietnam today?”,
  “Will it rain in London tomorrow?”, or “I have a meeting in London tomorrow at 3 PM;
  should I leave early?” The bot checks the named city using
  [Open-Meteo](https://open-meteo.com/en/docs), then uses current model-based conditions,
  a seven-day daily forecast, and an hourly forecast when a plan's day and hour are known.
  It can point out forecast rain, storms, snow, or strong wind in its reply and suggest
  practical steps for clothes, travel, and outdoor events. It does not schedule later
  notifications or claim to provide official severe-weather alerts.
  Include a city or city and country; the bot does not infer your location. Open-Meteo's
  free API is for noncommercial use, so check its [terms](https://open-meteo.com/en/terms)
  before commercial deployment.
- **Quote:** Ask “Share a quote” or “What's today's quote?” The bot fetches an
  attributed excerpt from the free, keyless [Dumb APIs quote endpoint](https://dumbapis.com/docs/quote/).
  Its collection is described as public-domain book excerpts. The API offers random
  and daily quotes, but no topic filter or original-work field; the bot does not invent
  either. The free API has a shared rate limit of 120 requests per minute per IP.

Both tools are called from normal chat messages; there are no separate commands. Only the
member's latest direct message is used to choose memory actions. Summaries and quoted
messages are never used to choose memory actions.

At startup, the bot reads the MCP server's `tools/list` response and sends every
non-memory tool to the chat model with `tool_choice: "auto"` on each normal reply.
The model decides when to call weather, quote, or another listed chat tool. Memory tools
stay in their separate member-scoped request flow.

Run offline checks with `cargo test`.
