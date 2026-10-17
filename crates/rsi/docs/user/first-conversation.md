# Your first conversation

[中文](first-conversation.zh.md)

RSI is a local AI workspace with a browser interface and an optional terminal
interface. You bring a model provider account and API key. The provider may charge
for requests. RSI is pre-release; saved formats can change between revisions.

## Install and open

Follow [Install from source](source-install.md), then run the Web command from
your checkout. Keep that terminal open while using RSI. The browser opens a
one-time sign-in link automatically. If opening fails, copy the complete link
printed in the terminal. Treat it as a password; do not share it.

You should see the conversation workspace. An empty model list is normal on
first launch. You do not need to edit a Profile or JSON file.

## Connect a model

Open **Settings → Models & access**. [Configure a model](model-setup.md) walks
through its three separate saves: credential, provider, and default model.
A saved API key alone does not configure a model.

## Send a first message

Choose **New conversation** and a local working directory. On Windows, type its
absolute path, for example `D:\Projects\my-project`; the directory browser is
unavailable. Use a fixed local NTFS directory.

Start with a text-only message, such as “Explain how you can help me review a
project.” Send it and wait for the response to finish. The conversation appears
in history; reopening it shows its saved messages.

Windows currently supports conversation and native file access, but restricted
shell commands are unavailable. A command refusal is not a model connection
failure. See [platform limits and troubleshooting](troubleshooting.md).

## Leave and return

Closing a browser tab leaves the service running. To stop RSI, press Ctrl+C in
the terminal that started it and wait for exit. Start Web again and reopen a
conversation from history. On Windows, stop Web before starting the
[terminal interface](terminal.md) or using the offline device manager.

Continue with [daily use](daily-use.md) once the first response succeeds.

