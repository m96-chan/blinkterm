# How it works

`blinkterm` starts a Chromium as a child process, drives it over the Chrome
DevTools Protocol on a pipe only the two of them hold — no port, so nothing
else on the machine can drive the browser — takes the page's screencast,
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

On Linux and macOS, in any terminal that speaks all three of the Kitty
graphics protocol, the Kitty keyboard protocol and SGR mouse reporting —
Kitty, WezTerm, Ghostty — and in a [tOS](https://github.com/m96-chan/tOS)
pane, which is where it was written.
tOS owns the display: there is no X11 and no Wayland and there never will be,
so no browser can be ported to it in the ordinary sense. But a terminal that
speaks those three protocols is already a screen, a mouse and a keyboard, and
that is the whole of what an engine wants. None of that argument is about tOS,
so the same binary runs in the others. This repository exists because tOS's CI
has no Chromium to test against and this program is nothing without one.

On a Mac there is no `/dev/shm`, so the frames go through `shm_open(3)`
shared memory objects instead, which Kitty and Ghostty read; a terminal that
does not gets the inline fallback, as anywhere else.

The terminal is asked before the engine is started. One that does not
answer the graphics query gets a sentence in the shell saying why and what
to do, not a blank pane; `--no-probe` skips the question for a terminal
that draws and does not answer.

**Inside tmux** it works with `set -g allow-passthrough on` in `tmux.conf`
and a terminal behind tmux that speaks the protocol. tmux eats graphics
commands otherwise, so the picture goes wrapped in tmux's passthrough and is
drawn as Kitty's Unicode placeholders — text tmux can see, move and redraw —
with the engine's PNG as the frames (at most 297 columns of picture, and
30 frames a second). Two things are lost inside tmux: the Kitty keyboard
protocol (tmux re-encodes keys, so there are no key releases and `ctrl+i` is
`tab`) and mouse positions finer than a cell. `--tmux on|off` overrides
the detection.

**Over ssh** (`$SSH_CONNECTION` set) the frames are the engine's PNG as it
sent them, inline: no `/dev/shm` on the far side, and raw pixels would be
3.9 MB a frame. Each frame is acknowledged to the engine only when it has
gone to the terminal, so the frame rate is what the link carries, every
frame current; frames that wait on the link make the page cast at half, then
three-eighths, of the pane until the link catches up, and the still of a
page at rest is always full size. `--fps <n>` caps it (15 by default over
ssh), and `--frames raw|png` overrides the choice.

Under `--alpha` (see [Transparent pages](usage.md#transparent-pages)) the frames stay
as they are: locally the JPEG frames of a moving page cannot carry the
transparency and show it black, while the PNG frames over ssh and in tmux do
carry it.

## What a page is told

A person is at the terminal, so a page is not told a program is driving.
With nothing set, on `chrome-headless-shell` 153 in a `ja_JP.UTF-8` locale:

```
navigator.webdriver   false
navigator.userAgent   Mozilla/5.0 (…) Chrome/153.0.0.0 Safari/537.36 blinkterm/0.2.0
userAgentData.brands  Chromium 153, blinkterm 0.2.0, and a GREASE brand
navigator.languages   ja-JP, ja, en
```

- **`navigator.webdriver` is `false`.** Blink sets it to say the browser is
  under automation, which is the first thing most bot checks read, and here
  it would be untrue. `--disable-blink-features=AutomationControlled` turns it
  off.
- **The user agent is the engine's own, without `HeadlessChrome`,** and with
  `blinkterm/<version>` on the end, the way a browser built on somebody
  else's engine names itself. The client hints say the same.
- **The languages come from the locale** — `LC_ALL`, then `LC_MESSAGES`, then
  `LANG` — with English last, and `en-US, en` when it is unset or `C`.
  `--engine-arg --accept-lang=…` (or `engine-arg = --accept-lang=…`) wins
  over the locale.
- **`--user-agent` is taken whole:** nothing is appended, and no client hints
  are sent beside it. `navigator.userAgentData.brands` then comes back
  empty, since nothing this program could write there would match a string it
  did not write.

Three things still say "headless", and nothing here changes them:
`navigator.plugins` is empty, `window.chrome` is missing, and
`Notification.permission` is `denied`. That is not something to hide. This
is a headless browser, and a site that challenges it on those grounds will
keep doing so. Only the one false claim is corrected. There is no
fingerprint spoofing, and none is planned.

A full Chrome (`engine = …`) reports those three the way a desktop Chrome
does. It also costs five to six times the memory: 13 processes and 1.6 GB
against the headless shell's 4 and 282 MB, measured on one page. That is why
the headless shell stays first in the search. The measurements are in
[#48](https://github.com/m96-chan/blinkterm/issues/48).
