# KnownSphere

KnownSphere is a Discord bot for server chat. Members talk to it with a command, a mention, a reply, or a wake name. It answers through an OpenAI-compatible chat API, looks up weather, quotes, definitions, and encyclopedia summaries, and can remember a few preferences per member.

It also runs in one forum channel when you configure one. Each new post there is a chat thread, and the post's tags choose the model. See [Forum chat](docs/forum-chat.md).

The bot responds in guild channels. It ignores direct messages and messages from other bots.

## Discord app

Create an application and a bot in the Discord developer portal. Invite the bot to your server with the `bot` scope.

Enable the **Message Content** privileged intent. The process requests the `GUILDS`, `GUILD_MESSAGES`, and `MESSAGE_CONTENT` gateway intents.

For ordinary channels, allow **View Channel**, **Send Messages**, and **Read Message History**. Forum mode also needs **Send Messages in Threads**, **Manage Threads**, and **Manage Channels** on that forum. Details are in [docs/forum-chat.md](docs/forum-chat.md).

## Configure

Copy the variables below into `.env` in the working directory, or export them in the environment. The process loads `.env` when that file exists. `.env` is gitignored.

| Variable | Required | Default | Purpose |
| --- | --- | --- | --- |
| `DISCORD_TOKEN` | yes | | Bot token |
| `OPENAI_API_KEY` | yes | | Bearer token for the chat API |
| `OPENAI_MODEL` | no | `gpt-4o-mini` | Model id sent on each completion |
| `OPENAI_API_BASE_URL` | no | `https://api.openai.com/v1` | API origin. `OPENAI_BASE_URL` is used when this is unset |
| `COMMAND_PREFIX` | no | `!knowsphere` | Command prefix |
| `WAKE_NAMES` | no | `KnownSphere,TransGPT` | Up to 12 comma-separated names |
| `FORUM_TYPE_CHANNEL` | no | | Forum channel id. Omit to disable forum mode |
| `FORUM_MODELS` | no | | Extra forum model tags, comma-separated. See the forum guide |
| `CONVERSATION_STORE_PATH` | no | `.knownsphere_conversations.json` | Channel history file, read by the memory process |
| `MEMORY_STORE_PATH` | no | `.knownsphere_memories.json` | Preference file, read by the memory process |

A base URL that already ends in `/chat/completions` is used unchanged. Any other base receives that suffix. Every configured model uses the same base URL and API key. Forum tag names are limited to 20 characters.

Wake names are 2–32 characters: letters, numbers, spaces, hyphens, or underscores.

## Run

Install a stable Rust toolchain, then:

```bash
cargo run --locked --release
```

The bot logs in and prints `KnownSphere Bot is ready` on stderr. Stop it with Ctrl-C.

Docker builds a release binary and runs it as `knowsphere` (uid 10001) with the data directory `/data`:

```bash
docker build -t knowsphere .
docker run --rm --env-file .env -v knowsphere-data:/data knowsphere
```

The image sets `CONVERSATION_STORE_PATH` and `MEMORY_STORE_PATH` under `/data`. Forum state is written in the working directory, which is `/data` in the image, so it stays on the volume too.

Pushes to `main` publish `ghcr.io/<repository>:main` and `ghcr.io/<repository>:sha-<commit>`.

## Chat

The default prefix is `!knowsphere`. A message that is only the prefix shows help.

| Command | Action |
| --- | --- |
| `!knowsphere ask <message>` | Chat, including the reply chain |
| `!knowsphere <message>` | Same chat path |
| `!knowsphere new` | Clear this member's history in this channel |
| `!knowsphere summarize [1-100]` | Summarize recent user messages. Default 10 |
| `!knowsphere actions [1-100]` | List explicit action items. Default 30 |
| `!knowsphere decisions [1-100]` | List decisions and open questions. Default 30 |
| `!knowsphere help` | Show help in the channel |

These also start a chat:

- A mention of the bot.
- A reply to one of the bot's messages.
- A message that starts with a wake name, optionally after `hey`, `hi`, `hello`, `yo`, `ê`, `này`, or `alo`. Matching ignores case. `TransGPT, what should I wear?` works. `I called TransGPT yesterday` does not.

Each member can send a request every 8 seconds. Long answers are split into messages of at most 1900 characters. The bot replies without pinging the member.

`summarize`, `actions`, and `decisions` read earlier messages in the channel. They cite Discord message links and skip bot messages.

Outside forum threads, the model is `OPENAI_MODEL`.

## What it can look up

The chat model can call these tools. It is instructed to use them for live facts and to keep each source's wording and link.

| Ask for | Tool | Source |
| --- | --- | --- |
| Weather, clothes, travel, or an outdoor plan | `weather` | [Open-Meteo](https://open-meteo.com/) |
| A random quote or the quote of the day | `quote` | [Dumb APIs](https://dumbapis.com/) public-domain excerpts |
| The current time | `current_time` | The host clock, shown as UTC and as a Discord timestamp |
| Arithmetic | `calculate` | A local parser (`+`, `-`, `*`, `/`, parentheses, decimals) |
| A dice roll | `roll_dice` | The host CSPRNG. Default is one six-sided die |
| An English definition | `define_word` | [Free Dictionary API](https://dictionaryapi.dev/) |
| Background facts | `search_encyclopedia` | [English Wikipedia](https://en.wikipedia.org/) |

Weather results are model forecasts, not official warnings. The quote API has no topic filter. Dictionary and Wikipedia results can be incomplete. Give a city for weather. The bot does not guess a member's location.

The bot needs outbound HTTPS to Discord, your chat API, and the sources above.

## Memory

Memory is on until a member asks to turn it off. The bot saves a preference only when that member explicitly asks to remember, change, or remove it. Saying "I like crochet" does not store an interest.

Stored fields are a nickname, pronouns, up to 10 interests, and a reply style. Contact details, links, and sensitive topics are rejected. Members can ask what is saved, ask to forget it, or turn memory off and on.

History is the last 10 messages for that member in that channel, and only for non-forum chats. `!knowsphere new` clears that channel. Forgetting or turning memory off clears that member's history in the whole server and leaves everyone else untouched. Turning memory back on starts from an empty history.

Forum threads use the visible post as context. They do not write the conversation file. A fresh forum conversation is a new post.

Files, all gitignored:

| File | Contents |
| --- | --- |
| `.knownsphere_conversations.json` | Per-channel history |
| `.knownsphere_memories.json` | Preferences and the on/off flag |
| `.knownsphere_forum_state.json` | Forum activation time and renamed posts |

The bot keeps these in a child process of the same binary (`knowsphere --memory-mcp`) and talks to it over stdio. Back up the files if you want to keep memory across machines. A missing file starts empty.

## Forum channel

Set `FORUM_TYPE_CHANNEL` to a forum's numeric id. On startup the bot checks that the channel exists and is a forum, then adds one tag per model. Members open a post, pick a model tag, and continue in the thread. A post or reply can include text, images, and files. Posts created before the first successful start stay inactive. Changing the forum id starts a new activation time.

Prefix commands do not run inside an enrolled forum thread. Elsewhere, chat works as before.

Full behavior, tag order, and the permission list are in [docs/forum-chat.md](docs/forum-chat.md).

## Development

The crate is a single Rust binary, `knowsphere`, edition 2021. CI runs:

```bash
cargo fmt --all -- --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked
```

Tests do not call Discord or the chat API.

| Doc | Contents |
| --- | --- |
| [AGENTS.md](AGENTS.md) | Rules for changing the bot |
| [CODEBASE.md](CODEBASE.md) | Process layout, modules, and data formats |
| [CODING_CONVENTION.md](CODING_CONVENTION.md) | Rust style used in this repo |
| [docs/forum-chat.md](docs/forum-chat.md) | Forum setup for operators |
