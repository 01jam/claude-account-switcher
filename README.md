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

### Starting on the freest account

**Settings → Start on the freest account** (off by default) puts the app on the
account with the most week left, once, at launch. "Most" is a ratio, not a
percentage: what the weekly window has left, over the days it still has to
cover.

```
room per day = (100 - week used) / days until the week resets
```

So 20 points to make six days last (3.3 a day) is a tighter week than 70 points
for one day (70), and the second account wins. Below a day the divisor stops
falling — a window resetting in ten minutes is not a reason to start on an
account with nothing left in it right now.

A window with no reset date is read by what is in it. At nothing, the week has
not started: its seven days begin when you pick the account up rather than now,
so there is nothing yet to spread and the account ranks ahead of every week
already running. With something spent in it, the date is one the endpoint did
not send, and the whole seven is the cautious reading of that.

It runs once per launch, before the window is up, and it is deliberately quiet
about the cases it declines: an account whose usage cannot be read is not a
candidate (a comparison against a blank is not one, and that holds for the
account already in place), a login the app has never saved is left alone until
you save it, and being on the freest account already means nothing happens.
When it does move, the window says which account it landed on and why.

### Where the app says it

The window spends its life in the tray, so a strip inside it is the wrong place
for something that happens at three in the afternoon and gets read at six. The
app decides per message, and never sends the same one twice:

- **window open and in front** — a toast over the account list, gone in a few
  seconds and dismissible with a click;
- **anywhere else** — a **desktop notification**, through whatever the system
  runs. That covers the auto-switch, the "nothing left to switch to" case and a
  token that will not renew: everything that happens on its own.

A launch landing on the freest account is the exception, and stays inside the
window: a notification at every login would be something to dismiss rather than
something to read. So is anything answering a button you just pressed — Refresh,
or an update install — which belongs where your finger is.

A desktop with nowhere to put a notification (a Linux session running no
notification daemon, a macOS install where the app was refused the permission)
fails quietly and tells nobody, so the message is kept instead and shown as a
toast the next time the window comes up.

### Asking without being refused

The endpoint publishes no budget, and this app spent a version finding its edge
the hard way: polling every account once a minute — sixty requests an hour each
— earned exactly the refusals that rate predicts.

The constants it now paces itself by are not ours. They come from the
measurements written down in `poll_policy.py` of
[realiti4/claude-swap](https://github.com/realiti4/claude-swap), which probed the
endpoint deliberately and recorded the method and the dates. What that work
found: roughly **28-30 requests an hour per identity**, over a **trailing
60-minute window** rather than a bucket that refills — so a burst saturates for
up to a full hour, and pausing does not hand the headroom back early. The
identity is the account (or, under another refusal regime, the token); planning
for the account is the conservative reading and the one taken here.

So each account carries its own schedule, in `pace.rs`:

| when | how often |
|---|---|
| floor, and the cache's serve-fresh window | **3 min** — about 20 requests an hour against a cap near 30 |
| active account, idle | decays to **5 min** |
| another saved account, idle | decays to **10 min** |
| spent account | **10 min** — slow, but never abandoned: a grant can free it early |
| active, within 15 points of its threshold **and** moving | **1 min** |
| after a refusal | **6 min** floor for the hour it takes to age out, backing off ×1.5 toward **30 min** while they recur |

The minute is the case the whole thing exists for, and it is bounded by
construction rather than by a timer: either the threshold is crossed and the
auto-switch moves away, or the movement stops and the next plan decays back to
the floor. Movement is a point of change on the window nearest its threshold, so
consumption on another machine tightens the cadence here too.

The backoff is multiplicative because the budget is shared with every other
machine watching the same account, none of them can see the others, and the
endpoint reports no remaining count — the same bargain TCP makes, for the same
reason. Each interval carries ±10% of jitter so two processes drift apart
instead of arriving together.

Those numbers can age: the endpoint is undocumented and Anthropic can retune it
any day. What would mean this needs revisiting is refusals appearing at these
rates.

The schedule is persisted with the cache, so a restart does not put every
account back in the queue at once — which is a burst, and a burst is the thing
that saturates the hour. After a `429` that account is left alone and keeps
showing its last known numbers rather than emptying its meters.

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

Once a reading is more than five minutes old, cards carry its age ("numbers from
3h ago") and when the app will ask again — and so does the tray menu, which is
where a frozen number is easiest to mistake for a live one, since the menu is
rebuilt synchronously and can only ever show what the cache already holds. Kept numbers that look freshly
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
anyone. The app says so once — the same failure comes back every 20 seconds and
is not news each time — and leaves a red mark on that account's card for as long
as it lasts, carrying the reason in its tooltip; pressing it tries again. That
account needs a fresh `claude` login.

### Finding a new version

Every six hours, and once at launch, the app asks GitHub for the newest release
of this repository — one unauthenticated request, no token and nothing sent but
a user agent. When the tag is a version above the running one, a red dot appears
on the gear in the title bar and the update turns up at the top of Settings,
with what is out, what is running, and a link to the release notes.

Tags are compared as three numbers. Anything else — a `-beta.1`, a `nightly` —
never counts as newer, because guessing where it sorts is how an app talks
someone into a downgrade.

**Installing** a `.deb` or `.rpm` runs the system package manager under
`pkexec`: polkit puts up its own password dialog and takes the password itself,
and this app never sees it. `apt-get install` rather than `dpkg -i`, so an
upgrade that pulls in a new dependency is apt's problem to solve; for an `.rpm`
it is `dnf`, `zypper` or `rpm`, whichever is installed. Dismissing the password
dialog is reported as cancelled, not as a failure — you answered, and the answer
was no.

Handing the file to the desktop was the first attempt at this and it does not
work where it matters: on Ubuntu the default handler for a `.deb` is the Snap
Store, which will not install a local package at all, so the update simply died
there.

What no package manager takes is still handed over. A `.dmg` is mounted and
dragged; an AppImage is shown in its folder rather than opened, since opening one
would only start a second copy of the app. Either way the download lands in your
Downloads folder, where you can check it or delete it, and its size is compared
against what the release says before anything is run as root.

The format is picked from how this copy was installed: an AppImage says so
through its own environment, otherwise the choice is `.deb` or `.rpm` by
distribution. A release with no package in that format still gets announced, and
the button opens the release page instead of guessing.

Installing does not replace the copy already running — quit from the tray menu
and start it again to be on the new one.

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
