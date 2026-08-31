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
the app checks the active account every minute and, as soon as **either**
counter reaches its own threshold, moves to the next account **in list order** —
which you rearrange by dragging rows from the handle on the left.

A candidate already past its own thresholds is skipped; if none is available the
app says so and stays put. An account whose usage cannot be read (typically an
expired token) is treated as usable instead: better to attempt the switch than
to stall on a network error.

Usage for accounts that are **not** active is read with the stored token — and
the app keeps that token alive itself, so the meters no longer go blank on an
account nobody has run `claude` under lately. See below.

The endpoint rate-limits, so the request rate is fixed rather than left to
chance: the periodic check runs every minute and forces past the cache, making
it exactly one request per account per minute. The cache — a reading stays valid
for a minute — is there for everything else, so that a window being used, a tray
rebuild or a switch adds no requests of its own. After a `429` that account is
left alone for a while and keeps showing its last known numbers rather than
emptying its meters.

That check is also what the open window listens to: each round's numbers are
pushed to it rather than left for it to come and fetch. A timer in the window
would be the webview's, and the webview is hidden in the tray for most of this
app's life, where timers are throttled or stopped outright — which is how meters
could sit on a stale reading until someone pressed **Refresh**.

**That account, not all of them.** The endpoint answers for the token it was
handed, so a refusal is about the account behind it — typically one sitting at
its own limit, which is exactly the account the user is about to switch away
from. Pausing the others blanks the meters they would be switching *to*.

The cooldown is also capped at 15 minutes however long `Retry-After` asks for.
The endpoint answers refusals with a stock `retry-after: 3600` and is then
perfectly willing twenty minutes later; and past 15 minutes the numbers are too
old for the auto-switch to act on anyway, so that is as long as it is worth
sitting blind before spending one request to find out. Which is the answer to
"how does it get out of a loop where every first request fails": it never waits
more than a quarter of an hour before testing reality again, and a refusal costs
one request per account.

The cache lives on disk (`usage.json`, next to the profiles), cooldowns
included. Held only in memory, every restart began blind and re-asked for
everything — the surest way to earn a `429` and then spend the cooldown with no
numbers to decide on, auto-switch stalled.

The first check runs 15 seconds after launch, not a full interval later; and
pressing **Refresh** renews whatever tokens are due and then re-reads the
numbers, since one is what buys the other. It also lifts a running cooldown —
at most once every 3 minutes, so a held button cannot become a stream of refused
requests. Someone looking at four-hour-old numbers knows something a blanket
`Retry-After` does not: whether the reason for the refusal still applies. When
the press is too soon, the notice says when the next attempt is due rather than
leaving the unchanged numbers to speak for themselves.

Once a reading is more than five minutes old — several rounds missed, which in
practice means a cooldown — cards carry its age ("numbers from 3h ago") and when
the app will ask again. Kept numbers that look freshly
loaded are how a spent account appears to have room left.

The auto-switch will not act on a reading older than 15 minutes. Showing an old
number is fine — the card says how old it is — but a five-hour window that has
since reset still reads as full in the cache, and rotating accounts on that is a
switch nobody asked for.

In the tray menu each account carries both percentages. When a counter comes
within 5 points of its threshold a `⚠` appears next to the name and the panel
icon takes a warning badge.

### Renewing the tokens

Claude Code owns the login *flow*, but not the renewal: the refresh token it
stores was issued to its own public client, so the same grant works from here
and produces exactly the credentials the CLI would have written. The app runs a
pass every 20 seconds — a few expiry dates read off disk, and no traffic at all
unless something is actually due:

- a **stored** account is renewed 30 minutes before it expires. Nothing else
  reads that file, so there is nobody to race;
- the **live** login is renewed only within 5 minutes of expiry, the same window
  Claude Code would refresh in. A renewal rotates the refresh token, and a
  session that has one in memory re-reads the file before using it — but there
  is no reason to make it do that while its token is still good.

Renewing the live login **does not require Claude Code to be closed**. The write
goes through the very lock the CLI takes for its own — `proper-lockfile`
semantics on `~/.claude/.storage-write.lock`, a directory whose `mkdir` decides
who holds it — and the file is re-read *inside* the lock: if a session renewed
first, this app stands down rather than overwriting. A lock whose mtime has
stopped moving for 15 seconds is treated as abandoned and cleared, which is also
the cure for Claude Code's own

> Failed to refresh OAuth token: another Claude Code process is refreshing it or
> exited mid-refresh

left behind by a session that died mid-write. Switching accounts and signing out
take the same lock.

If a session is holding it right now, the renewal is not forced: it is deferred
and retried on the next pass, seconds later. And **Renew token** in an account's
own menu does it immediately, whatever the expiry says, clearing that account's
cooldown with it.

**Save current login**, at the bottom of the window, is a different thing and no
longer pretends otherwise: it copies whatever Claude Code is signed into right
now over the active account's saved copy, name and plan included. Switching,
renewing and Refresh all do that on their own — this is the button for when you
signed in outside the app and want the copy brought up to date this second.

A refresh token that has been revoked or already spent cannot be renewed by
anyone — the app says so once, and that account needs a fresh `claude` login.

### Finding a new version

Every six hours, and once at launch, the app asks GitHub for the newest release
of this repository — one unauthenticated request, no token and nothing sent but
a user agent. When the tag is a version above the running one, a red dot appears
on the gear in the title bar and the update turns up at the top of Settings,
with what is out, what is running, and a link to the release notes.

Tags are compared as three numbers. Anything else — a `-beta.1`, a `nightly` —
never counts as newer, because guessing where it sorts is how an app talks
someone into a downgrade.

**Installing is not automatic, and cannot honestly be.** Every format this
project ships needs either root or a gesture from the user: a `.deb` or `.rpm`
goes through the system's installer, a `.dmg` gets dragged. So the button
downloads the package into your Downloads folder and opens it with whatever the
desktop uses for that file — the installer's password prompt is the installer's,
not this app's, and nothing is written outside the download until you answer it.
The format is picked from how this copy was installed: an AppImage says so
through its own environment, otherwise the choice is `.deb` or `.rpm` by
distribution. A release with no package in that format still gets announced, and
the button opens the release page instead of guessing.

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

Launching it again while it is already running does not start a second copy: the
new process hands over to the one already there, which brings its window back,
and then exits. That is what makes clicking the launcher a way to reopen the
window rather than a way to end up with two tray icons. On GNOME under Wayland
the compositor has the last word on whether a window may raise itself: a window
hidden in the tray comes back, but one that is merely behind another may only
flash in the dock rather than jump to the front.

The language is changed under **Settings → Language** (automatic, Italian,
English) and applies straight away, window and menus included. The strings live
in `locales/*.yml`, shared between frontend and backend: to add a language, copy
one of the two files and register the tag in `src/i18n.ts` and
`src-tauri/src/i18n.rs`.

## Caveats

- Switch accounts with Claude Code **closed**: the credentials are written under
  the CLI's own lock, but `~/.claude.json` is not covered by it, and a running
  session can rewrite the account keys there and undo the switch.
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
