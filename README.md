# Dual N-Back 1v1 — peer-to-peer, no central server (v0.2.2)

Solo mode was added in v0.2.1: `cargo run -- solo`. See
"Run it" below. Timestamped inputs and clock sync were
added in v0.2.2 — see "Fairness: timestamps and clock
sync" below. Everything else is v0.2.0 as before.

Two game instances talk directly to each other. One is the
host, one is the joiner. There is no server process running
in the background: the host *is* the server, embedded in
its own game instance, for exactly one match.

Built on the multithreaded web server from *The Rust
Programming Language*, Chapter 20. **Std only, no crates.**
`engine.rs` is unchanged from v0.1.0.

## Run it

### Solo practice (one terminal, no network)

```sh
cargo run -- solo
# UI: http://127.0.0.1:7878
```

The game starts immediately — no peer needed. Same
sequence generator, same scoring (+10 hit / −5 false
alarm), so a solo score is directly comparable to a 1v1
score. At the end you get a final score with hits, false
alarms, and misses, not a win/lose verdict.

Replay an exact game with its seed (printed when it
starts):

```sh
cargo run -- solo --seed 123456789
```

### 1v1 (two terminals)

On one machine, for testing:

```sh
# Terminal 1 — host
cargo run -- host
# UI: http://127.0.0.1:7878

# Terminal 2 — joiner (different web port!)
cargo run -- join 127.0.0.1:7879 --web-port 7880
# UI: http://127.0.0.1:7880
```

On two machines on the same Wi-Fi:

```sh
# Host machine (say its LAN IP is 192.168.1.42)
cargo run -- host

# Joiner machine
cargo run -- join 192.168.1.42:7879 --web-port 7880
```

Each player opens *their own* instance's UI in their own
browser. The game starts as soon as the joiner connects.
Turn sound on: the letter is spoken with
`speechSynthesis`. F = position, J = audio, or click.

Over the internet, the host's router must forward the P2P
port (default 7879), or the joiner cannot reach it. That is
a property of home routers, not of this code. If you want
internet play without port forwarding, the next step is a
NAT-traversal library (e.g. `iroh`, QUIC + relay) instead
of raw TCP — the protocol messages would stay the same.

## How it works

Each instance has two listeners:

- **Local web UI** (book's ThreadPool, `127.0.0.1:<web-port>`):
  serves `index.html` + `app.js` and the local API
  (`/api/info`, `/api/state`, `/api/respond`). This is the
  only part that still uses the Chapter 20 server, and it
  only ever talks to your own browser on localhost.
- **Peer link** (direct TCP, `0.0.0.0:<p2p-port>` on the host):
  one persistent connection for the whole match, handled
  by a dedicated reader thread.

Handshake:

1. Host generates a seed and a room from it.
2. Host sends `Hello` (`seed`, `n`, `total_trials`, `trial_ms`).
3. Host measures the round-trip time with 3 ping/pong
   exchanges and takes the minimum RTT. It sends `Start`
   with `one_way_ms` (min RTT / 2). The host starts its
   clock when it sends Start; the joiner starts its clock
   `one_way_ms` in the past, so the two clocks agree to
   within a few ms instead of lagging by a full one-way
   trip. If the ping exchange fails, `one_way_ms` is 0.
4. Both sides generate the *same* sequence from the seed
   (`engine::generate_sequence`), so no per-trial stimulus
   messages are needed.
5. After that, the only traffic is timestamped button
   presses:
   `{"type":"input","trial":3,"position":true,"audio":false,"pressed_at_ms":6123}`.
   Each side scores both players locally from the same
   sequence and both sets of presses.

## Fairness: timestamps and clock sync

Before v0.2.2, a press was judged by arrival time and by
each side's own clock, and the joiner's clock lagged the
host's by a full one-way trip. A press made right at a
trial boundary could count on one side and not the other.

Now every press carries `pressed_at_ms` — milliseconds
since game start on the sender's clock, stamped by the
local instance the moment it receives the press (the
browser is on localhost, so ~1ms after the click). The
receiver judges the press by that timestamp
(`server::valid_press`): the trial must match
`pressed_at_ms / 3000`, the press must not arrive more
than 1 second after it was made, and it must not claim a
time more than 250ms in the receiver's future. A press
made on time but delivered late still counts; a press
made late cannot be backdated. One exception: a press
made in the first 250ms of a new trial but tagged with
the previous trial (the browser polls state every 250ms,
so what it shows can lag) is accepted as the previous
trial's press.

Timestamps are self-reported, so this is fairness for a
friendly game, not anti-cheat for a hostile one — the
arrival-time check bounds how far a timestamp can lie,
but a peer that wanted to cheat could still claim an
earlier press within the 1-second window.

Wire format (`src/p2p.rs`): `[4-byte big-endian length]`
followed by UTF-8 JSON. TCP is a byte stream, so the
length prefix is what turns it back into messages.

## The game

Each trial shows a square on a 3x3 grid **and** speaks a
letter, at the same time. With n=2, press **Position** (F)
if the square matches 2 trials ago, and **Audio** (J) if
the letter matches 2 trials ago. Two decisions per trial.

- Hit: +10. False alarm: −5. Miss / correct rejection: 0.
- 20 trials, 3 s each = a 60-second game. Highest score wins.
- The sequence has a ~30% copy bias so matches are not
  vanishingly rare (pure random would be ~1/9 position,
  ~1/8 letter).
- Your presses are recorded locally and forwarded to your
  peer; your peer's presses arrive on the reader thread
  and are recorded as player 2. A peer press that arrives
  just after a trial boundary still counts (see
  `record_peer_response`): it is scored because scoring
  only covers completed trials.
- If the peer disconnects mid-game, their score freezes
  and your game runs to trial 20 anyway. There is no
  rematch button yet: restart both instances to play again.

## What changed from v0.1.0

| v0.1.0 (central server) | v0.2.0 (P2P) |
|---|---|
| One server, two browser tabs | Two instances, each with its own UI |
| `/api/create` + `/api/join`, 4-letter room codes | `host` / `join <ip:port>` on the command line, no room codes |
| HTTP polling to the central server | Persistent TCP peer link, length-prefixed JSON |
| Server generates sequence, scores both | Both generate the same sequence from a shared seed, both score both |

Unchanged: `engine.rs` (pure game logic), `lib.rs`
(ThreadPool), scoring, tuning constants.

## Tune it

Constants in `src/server.rs`: `TRIAL_MS: 3000`,
`TOTAL_TRIALS: 20`, `DEFAULT_N: 2` (host: `--n 1` or
`--n 3`). Scoring in `src/engine.rs`: `score_one`
(+10 / −5 / 0). Match bias in
`engine::generate_sequence` (`< 30`).

## Tests

```sh
cargo test
```

Engine tests cover determinism, match logic, the scoring
table, and match rate. Server tests cover create → join,
the P2P room (same seed → same sequence on both sides,
peer inputs accepted for the current trial, future trials
rejected), and JSON field parsing. P2P tests cover a real
`TcpListener`/`TcpStream` Hello → Start → Input round trip
and malformed-message rejection.

## API (local, per instance — for curl / debugging)

```sh
# Host, terminal 1: cargo run -- host
# Joiner, terminal 2: cargo run -- join 127.0.0.1:7879 --web-port 7880

# Who am I (host UI, port 7878)
curl -s localhost:7878/api/info
# -> {"mode":"host","peer":"peer connected","web_port":7878,"room":"P2P","token":"..."}

# State (token from /api/info)
curl -s 'localhost:7878/api/state?room=P2P&token=<token>'
# -> {"status":"playing","trial":3,"pos":4,"letter":"K",
#     "your_score":10,"opponent_score":5,"peer_connected":true,...}

# Press a button (trial must be the current trial)
curl -s -X POST localhost:7878/api/respond \
  -d '{"room":"P2P","token":"<token>","trial":3,"position":true,"audio":false}'
# -> {"ok":true}
# The press is forwarded to the peer; their opponent_score moves.
```

## What is still not built

- **Rematch:** restart both instances. A rematch protocol
  (new seed + Start over the existing link) is the obvious
  next addition.
- **Internet play without port forwarding:** needs NAT
  traversal (`iroh` / QUIC relay). The message shapes in
  `p2p.rs` would carry over.
- **No neutral referee:** with no central server, the host
  could lie about the score. Fine for a friendly 1v1, not
  for anything competitive.
- **Security:** the peer link is plain TCP, no TLS, no auth
  beyond "you connected to my port". Only play with people
  you trust, and only open the P2P port to them.
