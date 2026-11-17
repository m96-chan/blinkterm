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

Under `--alpha` (see [Transparent pages](usage.md#transparent-pages)) the
cast stays what the route makes it. Locally the JPEG frames of a moving page
cannot carry transparency, so the page is painted on a key colour, magenta,
and every frame and still is keyed back to transparency as it is decoded —
RGBA, `f=32` where it was `f=24`, 3.9 MB a 1280x770 frame rather than 2.9,
and 0.9 ms of keying on a 3.5 ms decode. The PNG frames over ssh and in tmux
carry real transparency and go as the engine made them, so there is no key
there, and `--alpha`'s number is not applied.

## What a page is told

A person is at the terminal, so a page is not told a program is driving.
With nothing set, on `chrome-headless-shell` 153 in a `ja_JP.UTF-8` locale:

```
navigator.webdriver   false
navigator.userAgent   Mozilla/5.0 (…) Chrome/153.0.0.0 Safari/537.36 blinkterm/0.4.0
userAgentData.brands  Chromium 153, blinkterm 0.4.0, and a GREASE brand
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
- **The console is heard.** Every page's session has `Runtime` and `Log`
  enabled, for [the console](features.md#the-console). That is how a
  debugger listens, and a page looking for one cannot find it the usual
  ways: measured on `chrome-headless-shell` 153, none of the getters pages
  set to catch an open DevTools — on an error's `stack`, an element's `id`
  or `className`, an accessor, a proxy's traps — is read with the console
  heard or unheard, and the three that are read (`Symbol.toStringTag`, a
  regexp's `toString`, an object's `toString` and `valueOf` under `%s` and
  `%d`) are read either way, by Blink's own formatting. What does differ is
  time: 200 `console.log`s of a 1000-object array took 36 ms unheard and
  53 ms heard, which a page could measure. `console = false` (or
  `--no-console`) leaves `Runtime` and `Log` off.

What still says "headless" is the engine, and which engine. Measured on
2026-10-18 with the program's own flags, on a page the identity override
has reached, `chrome-headless-shell` 153 against Chrome for Testing's
`chrome` 153 and Google Chrome 154, both full browsers under the same
`--headless`:

```
                              chrome-headless-shell   full Chrome / Chromium
window.chrome                 undefined               object
navigator.plugins.length      0                       5
navigator.mimeTypes.length    0                       2
navigator.pdfViewerEnabled    false                   true
speechSynthesis voices        0                       0 (CfT) / 19 (Chrome 154)
window.outerWidth × Height    800 × 600 (= screen)    780 × 580
permissions.query(notif.)     prompt                  denied
Notification.permission       denied                  denied
UNMASKED_RENDERER_WEBGL       SwiftShader             SwiftShader
navigator.webdriver           false                   false
userAgentData.brands          Chromium, blinkterm, GREASE — both
```

The left column is what the shell is: `//content/shell` with the old
headless renderer, which has no `window.chrome`, no plugins, no PDF viewer
and no window to have an outer size. The right column is a shipping
browser with no window, and every one of those reads as a desktop Chrome
does. WebGL is software on both — `--ozone-platform=headless` has no GPU to
offer, and dropping `--disable-gpu` on the full engine changed nothing that
was measured: the same SwiftShader string, the same 14 processes, the same
memory (see `engine::flags`). Nothing here hides any of it. This is a
headless browser, and a site that challenges it on those grounds will keep
doing so. Only the one false claim is corrected. There is no fingerprint
spoofing, and none is planned.

What the gates made of each, the same day, from a residential connection:

- `chatgpt.com` loaded and offered a chat on all three engines, with no
  unsupported-browser banner, no Cloudflare interstitial and no challenge.
  The refusal [#125](https://github.com/m96-chan/blinkterm/issues/125)
  reports was not reproduced; what a gate decides depends on more than the
  browser, and the tells above are what it would read when it does.
- `bot.sannysoft.com`: the shell fails six rows (`window.chrome`, the
  `PluginArray`, the headless `chrome`, `permissions` and `iframe` checks,
  and the SwiftShader renderer) and warns on one; a full engine fails only
  the renderer.
- `nowsecure.nl`'s Turnstile checkbox, clicked as a click from the pane is
  sent (`Input.dispatchMouseEvent`), said "success" on all three. That is
  one widget in test mode, and not a measurement of what a managed
  challenge makes of events that arrive with no mouse movement before them.
- `abrahamjuliot.github.io/creepjs` rates the shell "88% like headless" and
  a full engine "50% like headless", "33% headless" and "0% stealth" for
  both — the last being the point.

One tell is this program's own, on both engines: a service worker is a
target of its own, the identity is sent per page session, so a worker that
reads `navigator.userAgent` sees the engine's `HeadlessChrome/…` while its
page sees `Chrome/…`. A dedicated worker made by the page inherits the
page's (measured, a `Blob` worker). That is for a later issue.

A full Chrome (`--engine`, or `engine =`) is what passes where the shell
does not, and it costs more. Unpacked, Chrome for Testing's `chrome` is
393 MB to the shell's 261 MB. Started, on one blank page: 14 processes and
1.4 GB of resident memory summed over them against the shell's 7 and
0.5 GB; on `chatgpt.com`, 17 and 1.9 GB against 7 and 0.6 GB. It answers
on its pipe in 0.10 s to the shell's 0.03 s. Three times the memory and
twice the processes is why the shell stays the default and what
`--install-engine` fetches; the full engine is already in the search, after
it (see [How the engine is looked for](install.md#how-the-engine-is-looked-for)),
and `--doctor` says which one a run would get.

What you add yourself is told as little. A site style (see
[Site styles and scripts](features.md#site-styles-and-scripts)) is one entry
more in `document.adoptedStyleSheets` per file that fits, and nothing in
`document.styleSheets` or on `window`. A site script is invisible to the page
— it runs in a world of its own that shares only the DOM — unless its first
line asked for the page's world, and then it is the page's own code.
