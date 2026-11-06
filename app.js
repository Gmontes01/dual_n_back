# Run it on your laptop

This is the v0.2.0 peer-to-peer build. No central server, no
account, no install beyond Rust itself.

## 1. Install Rust (once)

If `cargo --version` does not work in a terminal yet:

```sh
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
```

Accept the defaults, then **close and reopen the terminal**
(or run `source "$HOME/.cargo/env"`), and check:

```sh
cargo --version
rustc --version
```

## 2. Unzip and enter the project

Unzip `dual-nback-trpl-0.2.0.zip`, then:

```sh
cd dual-nback-trpl
```

Run from this folder. The server reads `index.html` and
`app.js` from the current directory.

Optional sanity check:

```sh
cargo test
# expect: 13 passed, 0 failed
```

## 3. Play 1v1 on one laptop (two terminals, two tabs)

Terminal 1 — host:

```sh
cargo run -- host
```

Terminal 2 — joiner (note the different web port):

```sh
cargo run -- join 127.0.0.1:7879 --web-port 7880
```

Then open in your browser:

- Host UI: http://127.0.0.1:7878
- Joiner UI: http://127.0.0.1:7880

The game starts as soon as the joiner connects. Turn
sound on — the letter is spoken. Press **F** for Position
(square matches 2 back) and **J** for Audio (letter matches
2 back), or click the buttons. Hit +10, false alarm −5.
20 trials, 3 seconds each.

To play again, stop both with Ctrl-C and start them again.
There is no rematch button yet.

## 4. Play on two laptops (same Wi-Fi)

On the host laptop, find its LAN IP (macOS: System Settings
→ Wi-Fi → Details, or `ipconfig getifaddr en0` in a
terminal; Windows: `ipconfig`, look for IPv4 Address).

Host laptop:

```sh
cargo run -- host
```

Joiner laptop (replace `192.168.1.42` with the host's IP):

```sh
cargo run -- join 192.168.1.42:7879 --web-port 7880
```

Each player opens their **own** laptop's UI:
host → `http://127.0.0.1:7878`, joiner →
`http://127.0.0.1:7880`.

## Troubleshooting

- `cargo: command not found` — reopen the terminal after
  installing Rust, or run `source "$HOME/.cargo/env"`.
- `Could not listen on 127.0.0.1:7878` — an old instance is
  still running. Stop it with Ctrl-C, or use a different
  `--web-port` (e.g. `--web-port 7881`).
- Joiner says `Could not connect` — check the host is
  running first, the IP is the host's LAN IP (not
  127.0.0.1, which means "this machine"), both laptops are
  on the same Wi-Fi, and the host's firewall allows port
  7879.
- No spoken letter — check the browser tab is not muted
  and sound is on. The grid still works without audio.
- Page shows "Waiting for peer" — the joiner has not
  connected yet. Start the host first, then the joiner.
