# blinkterm

A real browser in a terminal pane. Not a text browser: the page is rendered by
a headless Chromium — Blink, the engine the page was made for — and arrives in
the pane as pixels, over the Kitty graphics protocol. Images, video, CSS,
JavaScript, the lot, in a terminal.

```sh
blinkterm https://example.com
```

`blinkterm` starts a Chromium as a child process, drives it over the Chrome
DevTools Protocol on a WebSocket it speaks itself, takes the page's screencast,
decodes each frame, and hands the terminal raw pixels — turning the terminal's
own reports of keys and mouse back into CDP input events. The engine renders;
the terminal displays; this program is the wire between them and nothing else.

## The numbers

Measured at 1280x770 on two cores, no GPU, no display server:

| | |
| --- | --- |
| screencast, JPEG at quality 85 | 57.8 frames a second |
| the same, PNG | 33.8 frames a second |
| `Page.captureScreenshot` in a loop | 10 to 12, in every format CDP offers |
| a frame on the engine's side | about 185 kB |
| decoding one here | 8 ms |

So the frames are **JPEG while the page is moving, PNG when it stops**. After
150 ms with no frame the tab in front is asked for one lossless still and that
is what is left on the screen: text you are reading is always lossless, and the
lossy frames are the ones scrolling past, which nobody reads. A page that never
moves costs one still and then nothing.

The frames are decoded here rather than by the terminal, and go over as raw
pixels (`f=24`, `f=32`) rather than as a PNG the terminal has to decode on its
parse loop. In a tOS pane they go through `/dev/shm` as a name (`t=s`) instead
of base64 in the escape sequence, which is what keeps a 60 fps stream off a PTY
that carries 240 KB/s. Where the terminal does not read shared memory,
`blinkterm` notices the names piling up unread and falls back to sending the
pixels inline: correct, obviously correct, and slow.

## Where it runs

Any terminal that speaks all three of the Kitty graphics protocol, the Kitty
keyboard protocol and SGR mouse reporting — Kitty, WezTerm, Ghostty — and a
[tOS](https://github.com/m96-chan/tOS) pane, which is where it was written.
tOS owns the display: there is no X11 and no Wayland and there never will be,
so no browser can be ported to it in the ordinary sense. But a terminal that
speaks those three protocols is already a screen, a mouse and a keyboard, and
that is the whole of what an engine wants. None of that argument is about tOS,
so the same binary runs in the others. This repository exists because tOS's CI
has no Chromium to test against and this program is nothing without one.

## Installing

Rust 1.75 or newer:

```sh
cargo install --git https://github.com/m96-chan/blinkterm
```

Then a browser engine, which `blinkterm` does not ship — a Chromium is 482 MB
installed, twice a tOS ISO, and a choice about which browser somebody runs. On
Debian or Ubuntu:

```sh
apt-get install -y --no-install-recommends chromium-shell fonts-noto-cjk
```

`chromium-shell` is Debian's `headless_shell`: the same Chromium with no
desktop browser UI compiled in, 76 packages against 112. The CJK fonts are not
optional if you read any; without them every Japanese glyph is a box.

Anything Chromium-shaped will do. `blinkterm` looks at `$BLINKTERM_ENGINE`
first, then on `PATH` for `chromium-shell`, `chromium`, `chromium-browser` and
`google-chrome`, in that order.

```sh
BLINKTERM_ENGINE=/opt/chrome/chrome-headless-shell blinkterm
```

## Keys

| | |
| --- | --- |
| `ctrl+l` | type a url |
| `ctrl+r` | reload |
| `alt+left` / `alt+right` | back and forward |
| `ctrl+t` | a new tab, with the cursor in the url bar |
| `ctrl+w` | close this tab; closing the last one quits |
| `ctrl+tab` / `ctrl+shift+tab` | the next tab, the one before |
| `alt+1` … `alt+9` | the nth tab |
| `ctrl+q` | quit |

Everything else goes to the page, including the mouse. A link that asks for a
new window gets a new tab, and the tab is switched to.

## Tests

`cargo test` runs the unit tests and skips everything that needs an engine.
The tests in `tests/engine.rs` are the ones this repository is for: a real
Chromium, real frames, and `tos_term::Terminal` with the compositor's own
`ImageFiles` installed parsing what would go down the pane's pseudoterminal.
They run only when `BLINKTERM_ENGINE` names the engine to use, and they say so
when they skip — naming the engine is the consent, because a machine with a
Chromium on it did not thereby agree to have it started.

```sh
BLINKTERM_ENGINE=chromium-shell cargo test --release -- --test-threads=1
```

`--release` because several of them assert on timings, and one at a time
because each starts a Chromium of its own: two engines painting at once on a
small machine make the scroll tests measure the machine. `tools/Dockerfile`
builds the bookworm image with the engine and the fonts in it if you would
rather not install a Chromium; `tools/` also holds the Python tools the design
was measured with, and has its own README.

## Licence

MIT. See [LICENSE](LICENSE).
