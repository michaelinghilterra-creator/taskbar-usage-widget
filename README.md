# Taskbar Usage Widget

See Claude and Codex quota pace plus live Spark inference status on the Windows taskbar.

A fork of [Claude Usage Widget for Windows Taskbar](https://github.com/AbdoSarmini/claude-usage-widget-for-windows-taskbar) by Abdo Sarmini, extended with Codex limits, local AI servers, a headroom router and an API value view.

<p>
  <img src="docs/screenshot-light.png" alt="Widget on a light taskbar" width="490"><br>
  <img src="docs/screenshot-dark.png" alt="Widget on a dark taskbar" width="490">
</p>

### ⬇️ [Download taskbar-usage-widget.exe](https://github.com/michaelinghilterra-creator/taskbar-usage-widget/releases/latest/download/taskbar-usage-widget.exe)

One small file (about 1 MB). No installer, nothing else to download.

## Features

- Always visible in the taskbar, not hidden in the tray.
- Tiny: about 5 MB of memory and near-zero CPU.
- Turns orange above 75% and red above 95%, like Claude Code.
- Hover for a live, pinnable details flyout with forecasts, token totals, and Spark telemetry.
- Follows your light or dark Windows theme.
- Multiple Claude Code accounts at once.
- Customizable size, icon, labels and rings.
- Shows all Claude quota types, Codex 5-hour/weekly windows, pace markers and forecasts.
- Shows Spark throughput, GPU activity, busy/down state, and a ten-minute sparkline.

## Requirements

- Windows 11.
- [Claude Code](https://code.claude.com/docs/en/setup) signed in with a Claude subscription (Pro, Max, Team or Enterprise). Run `claude` once and sign in; it doesn't need to stay open.

API keys and the Claude desktop app / claude.ai alone aren't supported.

## Install

1. Put `taskbar-usage-widget.exe` somewhere permanent, e.g. `%LOCALAPPDATA%\Programs\TaskbarUsageWidget\`.
2. Double-click it. The widget appears left of the tray icons.
3. Right-click it and tick **Start with Windows**.

> **"Windows protected your PC"?** The exe isn't code-signed. Click **More info → Run anyway**, or [build it yourself](#build-from-source).

## Usage

<img src="docs/screenshot-menu.png" alt="The widget's right-click menu" width="377">

- **Hover** for a non-activating preview. **Left-click** toggles a focused details card that stays open until you click the widget again, press **Esc**, click its close button, or switch to another window. Pin the card to keep it open when focus changes.
- Click a provider heading inside the card to open its usage page (or the configured Spark host).
- **Right-click** to show or hide providers, set the Spark address, refresh, customize the look, manage Claude accounts, start with Windows, or exit.

### Multiple accounts

If you run Claude Code with another config folder (e.g. `CLAUDE_CONFIG_DIR=$HOME\.claude-work`), right-click → **Accounts → Add account…** and pick that folder. Each account gets its own row.

## Troubleshooting

| Problem | Fix |
|---|---|
| Numbers dimmed, tooltip says to sign in | Run `claude` and sign in again. |
| `–` instead of numbers | Check your internet, or hover for the error. |
| Widget overlaps app icons | Make it narrower under **Customize** (hide labels or icon, smaller font). |
| Not on my second monitor | Only the main taskbar is supported. |

## Privacy

Claude usage is read from `api.anthropic.com` and `platform.claude.com`. Codex usage is read from `chatgpt.com`, with local `token_count` events in `%CODEX_HOME%\sessions` (or `%USERPROFILE%\.codex\sessions`) as a fallback; conversation text is never copied. The Codex CLI login is read only and is never refreshed or written by the widget. Spark stats are requested only from the LAN host you configure. The widget writes `%LOCALAPPDATA%\TaskbarUsageWidget\state.json` and `history.jsonl`, plus settings under `HKEY_CURRENT_USER\Software\TaskbarUsageWidget`. OAuth tokens are never logged or stored in those files. There are no analytics or update checks.

MCP mode reads the widget's local `state.json`, local Codex rollout metadata, and the configured Spark host only. It never reads Claude credentials or refreshes Claude tokens, and it labels Claude logins as `account 1`, `account 2`, and so on instead of exposing email addresses or paths.

## MCP setup

```
claude mcp add --scope user usage-widget -- "C:\path\taskbar-usage-widget.exe" --mcp
```

## Uninstall

Untick **Start with Windows**, click **Exit**, and delete the exe. Settings live in `HKEY_CURRENT_USER\Software\TaskbarUsageWidget`.

## Build from source

Needs [Rust](https://rustup.rs/) (MSVC toolchain) and the Windows SDK.

```
git clone https://github.com/michaelinghilterra-creator/taskbar-usage-widget
cd taskbar-usage-widget
cargo build --release
```

## Disclaimer

Unofficial community project, not affiliated with or endorsed by Anthropic. "Claude" is a trademark of Anthropic.

## License

[MIT](LICENSE). The original copyright notice is kept in LICENSE.

Based on [Claude Usage Widget for Windows Taskbar](https://github.com/AbdoSarmini/claude-usage-widget-for-windows-taskbar) by Abdo Sarmini, used under the MIT License.
