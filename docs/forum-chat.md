# Forum chat setup

Set `FORUM_TYPE_CHANNEL` in `.env` to the numeric ID of a Discord **Forum** channel:

```dotenv
FORUM_TYPE_CHANNEL=123456789012345678
```

The setting is optional. When it is absent, the bot keeps its existing command, mention, wake-name, and reply behavior. When present, the bot checks that it can access the channel and that it is a Forum channel before starting. Give the bot **View Channel**, **Read Message History**, **Send Messages in Threads**, and **Manage Threads** permissions for the forum. Enable the **Message Content** privileged intent in the Discord developer portal; the bot already requests this gateway intent.

Members start a chat by making a new forum post with any title and a text question in its body. The bot comments with an answer and then replaces the post title with a short summary. Any member can continue by sending a plain text comment in that post. Existing posts in the forum are not enrolled; the bot records the first activation time in `.knownsphere_forum_state.json`, so eligible posts continue working after restarts. Changing `FORUM_TYPE_CHANNEL` starts a new activation time for the newly configured forum.

The bot reads recent visible messages from each post for context. Edits and deletions in Discord affect later answers. Existing chat behavior outside eligible forum posts continues as before. To start a fresh forum conversation, create another post.
