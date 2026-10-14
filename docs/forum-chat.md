# Forum chat setup

Set `FORUM_TYPE_CHANNEL` in `.env` to the numeric ID of a Discord **Forum** channel:

```dotenv
FORUM_TYPE_CHANNEL=123456789012345678
OPENAI_MODEL=gpt-6-ultra
FORUM_MODELS=claude-opus-5.5
```

`FORUM_TYPE_CHANNEL` is optional. When it is absent, the bot keeps its existing command, mention, wake-name, and reply behavior. When present, the bot checks that it can access the channel and that it is a Forum channel before starting. The bot adds one tag for `OPENAI_MODEL` and one for each comma-separated model ID in `FORUM_MODELS`. The example model IDs are placeholders; configure only IDs supported by your `OPENAI_API_BASE_URL` and `OPENAI_API_KEY`. All forum models use that same endpoint and key. `FORUM_MODELS` may be omitted for a single-model forum.

On startup, the bot preserves existing forum tags and their IDs and adds missing model tags. Discord allows at most 20 forum tags, with names of at most 20 characters. Give the bot **View Channel**, **Read Message History**, **Send Messages in Threads**, **Manage Threads**, and **Manage Channels** permissions for the forum. Enable the **Message Content** privileged intent in the Discord developer portal; the bot already requests this gateway intent.

Members start a chat by making a new forum post with any title and a text question in its body. They can select model tags to choose the model for that post. If several model tags are selected, the bot uses the first matching model listed from left to right in `FORUM_MODELS`, regardless of the tags' order in Discord. `OPENAI_MODEL` has the lowest priority unless it is also listed in `FORUM_MODELS`. If no model tag is selected, the bot adds the `OPENAI_MODEL` tag and uses that model. The bot comments with an answer and then replaces the post title with a short summary. Any member can continue by sending a plain text comment in that post. Changing the model tags changes the model for subsequent answers. Existing posts in the forum are not enrolled; the bot records the first activation time in `.knownsphere_forum_state.json`, so eligible posts continue working after restarts. Changing `FORUM_TYPE_CHANNEL` starts a new activation time for the newly configured forum.

The bot reads recent visible messages from each post for context. Edits and deletions in Discord affect later answers. Existing chat behavior outside eligible forum posts continues as before. To start a fresh forum conversation, create another post.
