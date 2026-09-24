# The proof-of-concept tools

Before there was a Rust client there were these: the Python experiments that
established that a headless Chromium renders with no display server anywhere,
that its frames can be carried into a pane as Kitty graphics, and how much each
of those costs. They are kept because a measurement nobody can repeat is a
claim, and because they are still the quickest way to look at one frame.

The reasoning, the measurements and the decisions they led to are in tOS's
[`docs/design/browser.md`](https://github.com/m96-chan/tOS/blob/main/docs/design/browser.md).
Nothing in here needs a change to any terminal: the converter is an ordinary
program writing escape sequences to stdout, which is what a pane is for.

| | |
| --- | --- |
| `Dockerfile` | headless Chromium on bookworm, pinned, with the CJK fonts |
| `testpage.html` | a page whose every element makes one kind of failure visible |
| `cdp.py` | a Chrome DevTools Protocol client, standard library only |
| `bench.py` | how fast frames come out, and how big they are |
| `kitty_stream.py` | CDP screencast → Kitty graphics commands |
| `run.sh` | start a browser, measure it, stop it |

`cdp.py` speaks enough WebSocket to talk to Chromium and nothing more. That is
deliberate, and it is the same reason `src/` has no dependencies: a browser
experiment should not open by asking for a `pip install`.

## Running it

With Docker, which needs nothing installed:

```sh
./run.sh --docker --png frame.png
```

With a browser already on the machine — `chrome-headless-shell`,
`chromium-shell`, or `$CHROME_HEADLESS_SHELL` as the CI image sets it:

```sh
./run.sh --png frame.png
```

Either prints the numbers and leaves a PNG to look at. Look at it: a wrong
frame and a right frame both exit zero, and missing CJK fonts turn every
Japanese glyph into a box that only a human notices.

## Putting it in a pane

`kitty_stream.py` converts the screencast into Kitty graphics commands. The
default sends each frame inline as base64, which works in any terminal that
speaks the protocol and is too slow to stream — the PTY carries about 240 KB/s
into a tOS pane, and a 60 fps PNG stream wants seventeen times that.

`--shm` sends the pixels through a POSIX shared memory object and puts only its
name on the PTY, which is the transport `src/graphics.rs` settled on:

```sh
# start a browser first, e.g. ./run.sh --docker in another pane
python3 kitty_stream.py --shm --frames 300
```

Through tOS, end to end, with no display server anywhere — from a checkout of
[tOS](https://github.com/m96-chan/tOS), with this directory beside it:

```sh
cargo build --release
./target/release/tos --backend headless --warmup 25 --screenshot /tmp/tos.ppm \
    -e /bin/sh -c 'cd blinkterm/tools && python3 kitty_stream.py --shm --frames 40'
```

`--warmup` matters. A screenshot renders after that many frames, and the pane
needs a few of them to read the picture in; the default of one will photograph
an empty pane and tell you nothing is working when something is.

## Checking it without a terminal

The byte stream can be saved and checked on a machine with no terminal at all,
which is what CI is:

```sh
python3 kitty_stream.py --frames 60 --save frames.kitty
python3 kitty_stream.py --verify frames.kitty
```

`--verify` parses the commands back out, reassembles the base64 and checks that
each frame really is a PNG of the size it claims. It reads the bytes rather
than calling the encoder, so it would catch the encoder being wrong.
