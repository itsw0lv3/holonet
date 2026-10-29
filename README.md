# holonet

A keyboard-driven text browser. It fetches pages over HTTPS, lays out readable HTML, and runs [Brave’s `adblock` engine](https://github.com/brave/adblock-rust) (MPL-2.0) in process. There is no JavaScript engine and no pixel framebuffer. A page that is only an empty app shell stays blank until a later Chromium snapshot exists.

The binary is `holonet`.

## Status

The first cut is done. One process, one crate. Modules: `ui`, `tabs`, `load`, `block`, `dom`, `layout`.

| Piece | What it does |
| --- | --- |
| Load | rustls GET and POST, charset decode, manual redirects (10 hops), in-memory cookie jar |
| Layout | Headings, paragraphs, lists, links, pre, simple tables, form fields. Images are alt text |
| Browse | Scroll, focus, follow, back, reload, open a URL or search, find-in-page, link hints |
| Tabs | Per-tab history and scroll. New, close, switch. Session is memory only |
| Unload | `:unload`, `:unloadothers`, `:ttl`. Focusing an unloaded tab reloads its URL |
| Adblock | Network checks on the document and each redirect. Cosmetic hide rules. `.dat` cache |

`:js` answers `javascript rendering is not built`. A layout with no text says `no text content`.

Accepted limits of this cut:

- Client-rendered sites stay blank.
- No forward history, bookmarks, mouse, password store, or author CSS.
- Cosmetic rules that `scraper` cannot parse are skipped. Scriptlets, procedural filters (`:has-text`, `:upward`), and `$redirect` replacements are not applied. Those need a JS engine.
- The cookie jar is not written to disk. Form text is not a password store.
- Background tabs unload within about 30 seconds of their ttl, not on the exact minute.
- Filter lists download on first launch and on `:update`. They are not in the repo.

## Install

Rust 1.85 or newer (edition 2024).

```sh
cargo build --release
./target/release/holonet
./target/release/holonet example.org
```

`cargo run --release -- rust-lang.org` works the same way. One optional argument is a URL or a search query.

On Linux the files are:

- `~/.config/holonet/config` — settings
- `~/.local/share/holonet/engine.dat` — compiled filter engine

If those XDG dirs cannot be resolved, both fall back to `$TMPDIR/holonet`.

The first start with no cache downloads the filter lists and compiles them off the UI thread. The status line says `updating filters` while that runs. The next start loads `engine.dat`.

## Usage

`j` / `k` move a line cursor. Enter acts on whatever is focused on that line: a link, a form field, or a submit button. `?` or `:help` opens the same list inside the program. `q` or `Ctrl+c` quits. `Esc` leaves a prompt, hint mode, or the help overlay.

| Key | Action |
| --- | --- |
| `j` `k` / arrows | Move the line cursor |
| `PgUp` `PgDn` | Page |
| `g` `G` | Top and bottom |
| `Tab` / `Shift+Tab` | Next or previous focus on the cursor line |
| `Enter` | Follow, edit a field, or submit |
| `Shift+Enter` or `T` | Open the focused link in a new tab |
| `o` | Open a URL or search. `Esc` cancels, `Enter` goes |
| `/` `n` `N` | Find, next, previous |
| `t` `W` | New tab, close tab |
| `]` `[` | Next and previous tab |
| `Ctrl+Tab` / `Ctrl+Shift+Tab` | Next and previous tab |
| `1`–`9` | Jump to a tab |
| `b` / `Backspace` | Back |
| `r` | Reload |
| `f` `F` | Hint labels, hint into a new tab. Type `a`–`z`, `Esc` cancels |
| `:` | Command line |

Commands:

| Command | Action |
| --- | --- |
| `:help` | Key list |
| `:lists` | Filter list titles and last update |
| `:update` | Download and recompile the lists |
| `:unload` | Drop this tab’s page. The URL stays |
| `:unloadothers` | Drop every other tab’s page |
| `:ttl N` | Unload background tabs after N minutes. `0` is off. Saved to the config file |
| `:js` | Reserved. Not built |

An address with `://`, or a host with a dot and no spaces, is opened as a URL (`https://` is added when the scheme is missing). Anything else is a search. The default template is `https://lite.duckduckgo.com/lite/?q={q}`. DuckDuckGo’s lite front end returns HTML.

`~/.config/holonet/config` is two lines, written by `:ttl` and read at startup:

```
tab_ttl=0
search=https://lite.duckduckgo.com/lite/?q={q}
```

`search` must contain `{q}`. Edit that line to point at another HTML search page.

## What comes next

Build these only when empty app shells are actually in the way. Bookmarks, Kitty images, and `auto_js` wait behind them. Do not turn Chromium into the default loader.

Every navigation stays on the HTTP loader. A tab moves to Chromium only when that page needs JavaScript. Docs and articles stay in the text browser. One shared Chrome process, one target per tab, and the process exits when the last target closes. `:unload`, `:unloadothers`, and `:ttl` are that exit ramp.

### 1. Read-only snapshot

`:js` reloads the current tab’s URL through Chromium. [`chromiumoxide`](https://docs.rs/chromiumoxide) drives a Chrome or Chromium already on `PATH` over the DevTools Protocol. Navigate, wait until the network is quiet and mutations pause (with a timeout), then `page.content()` and hand that HTML to the existing DOM and layout path. The TUI never draws Chromium’s pixels.

After a load whose layout has no text, the status line should mention `:js`. A short real page stays on HTTP because the user did not ask. Following a link inside a Chromium tab stays on Chromium. A new tab goes back to HTTP.

Adblock uses the same engine. CDP `Fetch.requestPaused` checks every subresource and continues or fails it. Cosmetic rules go in as a stylesheet so Chromium hides those nodes before the snapshot. Scriptlets become possible here and are not part of this cut.

Prove `page.content()` on one SPA first. Then settle, timeout, crash restart, and request interception. Chromium must be installed. One browser is on the order of 150–400 MB. Some sites refuse headless Chrome.

### 2. Live interaction

Same settle-and-snapshot helper. The document becomes Chrome’s accessibility tree (`Accessibility.getFullAXTree`): each node already has a role and a name. Focus moves over those nodes. Enter calls `click()`. Typing uses `Input.insertText`. Scrolling past the snapshot sends a wheel event, which is what makes infinite scroll work. The URL bar follows `Page.frameNavigated`. After every action, settle again and replace the tree.

Hover menus, canvas, drag-and-drop, and rich text editing stay unsupported. Do not map terminal cells onto Chrome’s pixels.

### After that

- `auto_js` presses `:js` itself when the body is an empty shell. It does not guess from scripts or frameworks.
- Kitty images. The image node is already a placeholder (`alt` text and source URL). Nothing paints it yet.
- Bookmarks.
- Cookie and annoyance filter lists, still off until asked.
- Author CSS, once color and `display:none` from the page itself matter. Cosmetic hiding does not need it.

## Memory

These ranges are from public headless and desktop Chrome measurements, not a benchmark of this program. RSS double-counts memory shared between Chrome processes.

- Text browser, no Chrome: the binary, the compiled lists, and a handful of article tabs should sit around 50–150 MB. Idle CPU is about zero. Unload drops the document without killing a renderer.
- Chrome running: one shared browser is roughly 150–300 MB before the first site. Each open site adds a renderer, from tens of megabytes for a Wikipedia-class page to a few hundred for a web app. Five mixed tabs is commonly 0.5–1.5 GB. With no targets left, Chrome exits.
