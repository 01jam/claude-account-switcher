# Claude Account Switcher

> **Fair warning: this thing is entirely vibe coded.**
>
> Not one line of it was typed by hand. It was prompted into existence with
> Claude Code, and reviewed mostly by using it. It works, though — so, honestly:
> who cares.

A desktop app (Tauri 2 + React + React Aria Components) that keeps several
Claude Code accounts saved on Linux and macOS and switches the active one from
its window or from the system tray.

The interface speaks English and Italian: it follows the system language, with
an override in settings.

## How it works

Claude Code keeps its login in two pieces:

- the **OAuth tokens** — on Linux in `~/.claude/.credentials.json`, on macOS in
  the login keychain, under the service `Claude Code-credentials`
- `~/.claude.json` — configuration, of which only a few keys (`oauthAccount`,
  `userID`, …) identify the account

The app copies both into `~/.config/claude-switch/profiles/<id>/` (on macOS
`~/Library/Application Support/claude-switch/`) and restores them when you
switch. The rest of `~/.claude.json` — projects, history, preferences — is left
alone: writes are atomic, and every switch leaves a snapshot in `backups/`.

On macOS the app writes wherever it finds the login: if the keychain already
holds an entry it uses that, otherwise it falls back to the file. Either way the
CLI reads the right account, whichever of the two routes your installed version
takes.

Before every switch the live credentials are copied back into the active
profile, because Claude Code rotates the tokens as it runs.

### Usage and auto-switch

Every account shows two meters: **5-hour session** and **week**. The numbers
come from `GET https://api.anthropic.com/api/oauth/usage`, the same endpoint
Claude Code queries for its own `/usage`, called with that account's OAuth
token. **This is not a public API**: its shape may change without notice, so
every field is treated as optional — when one is missing the meter shows `—` and
the auto-switch does not fire.

Each meter carries a draggable marker: the **threshold** past which the account
counts as spent (100% by default, so only at a full limit). With auto-switch on,
the app checks the active account every 5 minutes and, as soon as **either**
counter reaches its own threshold, moves to the next account **in list order** —
which you rearrange by dragging rows from the handle on the left.

A candidate already past its own thresholds is skipped; if none is available the
app says so and stays put. An account whose usage cannot be read (typically an
expired token) is treated as usable instead: better to attempt the switch than
to stall on a network error.

Usage for accounts that are **not** active is read with the stored token: if it
has expired, those two meters stay empty until you make the account active
again. Only Claude Code renews tokens, and this app does not do it for it.

The endpoint rate-limits, so requests are kept sparse: a response stays valid
for 5 minutes, the periodic check runs every 5 minutes and reuses that cache.
After a `429` the app stops asking for 10 minutes and keeps showing the last
known numbers rather than emptying the meters.

The cache lives on disk (`usage.json`, next to the profiles), cooldown included.
Held only in memory, every restart began blind and re-asked for everything —
the surest way to earn a `429` and then spend ten minutes with no numbers to
decide on, auto-switch stalled.

The first check runs 15 seconds after launch, not after five minutes; and
pressing **Refresh** triggers it too, since they are the same numbers.

In the tray menu each account carries both percentages. When a counter comes
within 5 points of its threshold a `⚠` appears next to the name and the panel
icon takes a warning badge.

### Claude Desktop is not involved

The switch covers the CLI and the VSCode extension, which share the files above.
**Claude Desktop does not**: it is an Electron app that authenticates like a
browser, with the `.claude.ai` `sessionKey` cookie inside the Chromium profile
in `~/.config/Claude/`. No files in common, so it stays on whichever account you
signed it into — a choice, not a bug.

It could be extended to cover it (the ~200 KB of `Cookies`, `Local Storage`,
`Session Storage`, `IndexedDB` and `Preferences` would be enough; the rest of
those 289 MB is cache), but it would need Desktop closed at every switch and it
is sensitive to its updates.

## Requirements

Node 20+ and npm on both platforms.

### Linux

```bash
# toolchain
sudo apt install -y build-essential curl file libssl-dev pkg-config
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
# Tauri 2 runtime + tray, Ubuntu 24.04+
sudo apt install -y libwebkit2gtk-4.1-dev libgtk-3-dev librsvg2-dev \
  libayatana-appindicator3-dev
```

**GNOME**: the top bar shows no tray icons at all without the
[AppIndicator and KStatusNotifierItem Support](https://extensions.gnome.org/extension/615/appindicator-support/)
extension. Without it the app still works — you just only get the window.

**Fractional scaling**: GTK 3 cannot do it on Wayland. With
`scale-monitor-framebuffer` on, the compositor and the toolkit disagree about
how big the window is: clicks land beside the buttons rather than on them, and
dragging the window onto a monitor with a different scale leaves WebKitGTK
repainting into a buffer of the old size — the window comes back half drawn. On
a session like that the app moves itself to XWayland, which does the scaling
instead. To keep the native backend: `CLAUDE_SWITCH_KEEP_WAYLAND=1`.

**NVIDIA**: WebKitGTK's DMA-BUF renderer and the proprietary driver do not get
along. Where the `nvidia` module is loaded the app sets
`WEBKIT_DISABLE_DMABUF_RENDERER=1` itself, unless you have already set it.

### macOS

```bash
xcode-select --install
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
```

Nothing else: WebKit and the menu bar are the system's. The menu-bar icon uses
the glyph as a *template*, so it follows light and dark mode.

## Development

```bash
npm install
npm run app        # tauri dev
npm run app:build  # .deb / AppImage / .rpm on Linux, .app / .dmg on macOS,
                   # under src-tauri/target/release/bundle
```

Two side scripts:

```bash
python3 scripts/generate-icons.py     # regenerate the tray and app icons
./scripts/install-desktop-entry.sh    # a real dock icon while developing
```

`generate-icons.py` draws everything with no imaging library involved. The
arrows glyph is Tabler's `arrows-exchange`, redrawn from its paths: terracotta
on transparent for the tray, white and tilted 15° at the centre of a terracotta
squircle for the app icon. Nothing is borrowed from anyone else's mark, so the
script runs on any machine.

`install-desktop-entry.sh` is Linux-only and development-only: `tauri dev`
launches a bare binary and GNOME, with no matching `.desktop`, shows a generic
icon. The package built by `app:build` ships its own and does not need it.

If `npm run app` fails with *OS file watch limit reached*, the system's inotify
watches are exhausted (Dropbox and friends eat tens of thousands):

```bash
echo 'fs.inotify.max_user_watches=524288' | sudo tee /etc/sysctl.d/60-inotify.conf
sudo sysctl --system
```

## Using it

1. You are already signed in with one account: open the app and press **Save**
   in the "Login not saved" banner.
2. **Add account** → sign out, open a terminal, log in with the second account,
   come back and press **Save account**.
3. From then on: click an account in the list, or pick one from the tray menu.

The window carries no system decorations: the bar at the top is drawn by the app
— you drag it from there, and on the right sit settings, minimise and close.
There is no maximise button: a list of two or three accounts has nothing to do
with a full screen.

Closing the window leaves the app running in the tray; you quit it from the tray
menu. On macOS, clicking the Dock icon brings the window back.

The language is changed under **Settings → Language** (automatic, Italian,
English) and applies straight away, window and menus included. The strings live
in `locales/*.yml`, shared between frontend and backend: to add a language, copy
one of the two files and register the tag in `src/i18n.ts` and
`src-tauri/src/i18n.rs`.

## Caveats

- Switch accounts with Claude Code **closed**: a running session can rewrite
  `~/.claude.json` and overwrite the switch.
- On Linux the tokens are stored in the clear (as Claude Code itself does) with
  `0600` permissions: this is not a keyring. On macOS the live tokens sit in the
  keychain, but the copies in the app's profiles are still `0600` files — so the
  same caveat applies there too.

## Licence

MIT — see [LICENSE](LICENSE).

Claude Account Switcher is an independent tool. It is not affiliated with,
endorsed by, or supported by Anthropic; Claude and Claude Code are theirs. It
reads an undocumented usage endpoint that may change or stop working without
notice.
