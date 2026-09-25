# Security

`blinkterm` points a real browser engine at whatever page you ask for and puts
the result in a terminal. Untrusted input is the normal case, not the edge one.

## Reporting something

Use GitHub's private vulnerability reporting: the **Security** tab of this
repository → **Report a vulnerability**. That opens a private thread with the
maintainer; please use it rather than a public issue for anything that lets a
page reach outside the page.

If private reporting is not available to you, open an issue saying only that
you have something to report and how to reach you, and it will be moved
somewhere private.

There is no release cadence to promise a fix against yet, and one person
maintains this. Expect a first reply rather than a patch.

## What is whose problem

**The engine's.** Everything about parsing and executing the page: HTML, CSS,
JavaScript, images, fonts, TLS, same-origin policy, and the renderer sandbox
that is supposed to contain a bug in any of them. `blinkterm` does not ship an
engine and does not patch one — keep your Chromium updated, and report engine
bugs to Chromium.

With one exception that is ours to say out loud: **as root, the sandbox is
off.** Chromium refuses to start as root without `--no-sandbox`, so `blinkterm`
adds it, which removes the main thing standing between a malicious page and the
machine. It prints a warning when it does. Do not browse as root.

**`blinkterm`'s.** The wire between the engine and the terminal:

- **The DevTools transport.** The engine is started with
  `--remote-debugging-pipe` and driven over two inherited pipes, its
  descriptors 3 and 4, which nobody else holds. It opens no port, and the
  engine tests check that no process in its group is listening on one
  ([#5](https://github.com/m96-chan/blinkterm/issues/5)). That holds for a
  real headless shell — `chrome-headless-shell`, or Chromium itself. It does
  not hold for Debian's `chromium-shell`, which is Chromium's `content_shell`
  and opens its DevTools port whatever it is told; with that engine any local
  process running as you can still attach, and the README says so.

- **What gets written to your terminal.** A terminal executes the bytes it is
  sent, so anything page-derived that reaches the status row is a place where a
  page could try to speak to your terminal instead of to you. `blinkterm` puts
  the page's **title** (`document.title`, which the page sets to whatever it
  likes) and its **url** on that row, and **does not currently strip control
  or escape bytes from either**. Treat that as an open hole, not a solved
  problem. The page *body* is safe in this respect by construction: it arrives
  as decoded pixels and is written as a graphics payload, never as text.

- **`/dev/shm`.** Frames go through POSIX shared memory objects named
  `blinkterm-<pid>-...`, created with your umask and unlinked by the terminal
  as it reads them. On a default umask another local user can read a frame in
  the window between write and unlink — that is a picture of whatever you are
  looking at. `Painter` falls back to inline base64 when `/dev/shm` is not
  usable, but it does not currently tighten the mode.

- **The engine's lifetime.** `blinkterm` starts Chromium in a process group of
  its own and kills the group on exit, on a signal, and from a panic hook.
  A Chromium left running after `blinkterm` has gone — holding your profile,
  and with `chromium-shell` an open debugging port — would be a security
  problem, so failures of that machinery count here.

## Out of scope

- Bugs in Chromium itself — report upstream.
- Anything that needs the attacker to already run code as your user. Such an
  attacker can read the profile directory and ptrace the engine; the pipe
  keeps them from driving it through a port, not from everything.
- Running as root after being told not to.
- The proof-of-concept scripts in `tools/`. `tools/Dockerfile` binds the
  debugging port to `0.0.0.0` and says so in a comment: it is a development
  image and an open CDP port is remote code execution by design. Do not run it
  anywhere reachable.
