# GameFlow Bevy Multiplayer Example: Pac-Man 1v1

A working example of wiring a real-time multiplayer game to
[GameFlow](https://gameflow.gg): a dedicated game server per match, queue
matchmaking, and skill rating. The game is a Pac-Man 1v1 duel written in Rust
with [Bevy](https://bevy.org); the interesting part is the integration shape, not
the maze.

If you are building your own game on GameFlow, this repo is the shape to copy:
where the API key lives, how the server reports results, how the client reaches
the platform, and what you configure in the dashboard.

> **How it plays (briefly):** two Pac-Man share one maze and race for the same
> pellets. Difficulty rises on a clock until the ghosts turn lethal, so a match
> lasts a few minutes. The power pellet lets you eat your rival and steal score.
> Higher score when both players are out wins.

## Architecture

Three processes, plus a pure-logic crate they share. The dividing line is strict:
`shared` knows nothing about networking, and `backend` knows nothing about the
game.

| Piece | Crate | What it is | Holds the API key? |
|---|---|---|---|
| **Game client** | `client` | the desktop app: renders, predicts its own Pac-Man, talks only to your backend | no |
| **Backend (BFF)** | `backend` | a thin `axum` server you host; the only holder of the GameFlow API key. Brokers every GameFlow call for client and server | **yes** |
| **Game server** | `server` | headless Bevy authority, packaged and run **by GameFlow** (allocated per match) | no |
| **Simulation** | `shared` | the whole game as plain Rust (maze, movement, ghosts, pellets, scoring, wire protocol); no Bevy, no networking | n/a |

The security rule from GameFlow: **only the backend holds the API key.** The
client never sees it, and the server reports its result back *through* the backend
instead of calling the platform itself.

**Netcode.** The server runs the whole simulation at a fixed 30Hz and is the only
authority. The client predicts nothing but its own Pac-Man and interpolates
everything else. Because grid movement is exact, replaying an input lands in the
same place on both ends, so reconciliation normally corrects nothing. A full
snapshot is ~150 bytes, so there is no delta compression.

**Result + rating flow.** At game over the server POSTs the raw result to its
backend at `/internal/match-result` (authenticated with a shared backend API token).
The backend forwards it as `POST /v1/skill-rating/matches:report`. The client
reads its rank via the backend, which calls
`POST /v1/skill-rating/player-rating:resolve`. GameFlow resolves the skill model
from the live matchmaker, so the game never carries a model id.

## Project structure

```
.
├── crates/
│   ├── shared/    maze, movement, ghosts, difficulty, pellets, sim, protocol
│   ├── server/    GameFlow lifecycle, renet transport, tick loop, result reporting
│   ├── client/    identity, prediction, rendering, screen flow
│   └── backend/   guest identity, matchmaking queue, result reporting, rating
├── Dockerfile               # multi-stage (cargo-chef); GameFlow builds the server from this
├── scripts/package-server.sh
└── rust-toolchain.toml      # pins the toolchain; cargo picks it up automatically
```

## Building

Rust is pinned by `rust-toolchain.toml`, so plain `cargo` uses the right version.

### Game server: GameFlow builds it, not you

GameFlow builds the server from an uploaded zip. A `Dockerfile` at the root of
the zip wins over any engine template, which is the whole reason a Rust server
needs no platform change. Produce the zip and upload it (see
[GameFlow setup](#gameflow-setup)):

```bash
scripts/package-server.sh          # writes pacman-server.zip (excludes .env, target, etc.)
```

The Dockerfile is a multi-stage build with `cargo-chef`, so a code change does not
recompile Bevy from scratch every time. To sanity-check it compiles locally:
`cargo build -p pacman-server --release`.

### Backend (BFF): you host this

```bash
cp crates/backend/.env.example crates/backend/.env   # then fill it in (see below)
cargo build -p pacman-backend --release              # binary at target/release/pacman-backend
```

Run it anywhere reachable by your players' clients. It listens on `PORT` (default
`8080`) and is the only process that holds `GAMEFLOW_API_KEY`.

### Client: each player builds and runs it

```bash
cargo build -p pacman-client --release               # binary at target/release/pacman-client
```

Bevy needs a GPU and a windowing stack. On Debian/Ubuntu, install the runtime
libraries first:

```bash
sudo apt install -y libxkbcommon-x11-0 libvulkan1 mesa-vulkan-drivers
```

## GameFlow setup

What you configure once in the dashboard for the platform side to work.

1. **Create the game** and generate an org-scoped **API key**
   (Settings → API keys). It goes only in the backend's `.env`.
2. **Upload the server build**: run `scripts/package-server.sh` and upload
   `pacman-server.zip`. Confirm it landed:
   `GET /v1/images/builds?game_id=<id>` → `status: success`, `isCurrent: true`.
3. **Matchmaker** (required for the queue), published for game mode `1v1`:
   ```
   Ticket Input → Skill Model → Skill Rule → Expansion Rule → Team Composition → Output
   ```
   Team Composition is **2 teams of 1 player**. Click **Publish** after any edit;
   the match function keeps running the old plan until you do.
4. **Skill rating** (required for the rating to update and for skill-based
   pairing): create an org-scoped **skill model** (2 teams × 1 player, engine
   e.g. `plackett_luce`) and select it on the **Skill Model** node. The game
   never carries the model id; GameFlow resolves it from the live matchmaker.
   - Without a Skill Rule (or with a neutral model) the queue is plain **FIFO**.
   - The **Skill Model + Skill Rule** together are what turn on skill-based
     matchmaking and make results update ratings. The **Expansion Rule** widens
     the acceptable skill gap as a ticket waits, so nobody is stuck in queue.
5. **Inject the server env** through the game's `config.environment_variables`:
   `GAME_BACKEND_URL` (where the server POSTs results) and
   `GAME_BACKEND_API_TOKEN` (must match the backend's).

## Running it

GameFlow runs the game **server**, one allocated per match. You run the other
two: the **backend** (hosted) and the **client** (desktop).

```bash
# 1. Backend. Fill crates/backend/.env with your GameFlow credentials first.
set -a && . crates/backend/.env && set +a
cargo run -p pacman-backend                          # or run the release binary

# 2. Client — point it at your backend with PACMAN_BACKEND_URL.
PACMAN_BACKEND_URL=http://127.0.0.1:8080 cargo run -p pacman-client
```

A 1v1 needs two clients. On two separate machines, just run the command above on
each. To run both on **one** machine, give each its own config directory so they
get distinct guest identities. Otherwise both read the same identity and can't
be matched against each other:

```bash
# player 1
XDG_CONFIG_HOME=/tmp/pac1 PACMAN_BACKEND_URL=http://127.0.0.1:8080 cargo run -p pacman-client
# player 2
XDG_CONFIG_HOME=/tmp/pac2 PACMAN_BACKEND_URL=http://127.0.0.1:8080 cargo run -p pacman-client
```

Use `cargo run` (not the built binary directly) so Bevy resolves the client's
`assets/`. The identity lives at `$XDG_CONFIG_HOME/pacman-1v1/identity.json`;
delete that directory to start over as a fresh guest. Press play in each: the
client enqueues a ticket, GameFlow matches the two players, allocates a server
for the match, and both clients connect to it.

### Environment

Authoritative names and comments are in the three `crates/*/.env.example` files.
In short:

- **backend** (holds the key): `GAMEFLOW_API_URL`, `GAMEFLOW_API_KEY`,
  `GAMEFLOW_GAME_ID`, `GAMEFLOW_GAME_MODE=1v1`, `GAMEFLOW_REGION`,
  `GAME_BACKEND_API_TOKEN`, `JWT_SECRET`, `PORT`.
- **server** (no key; GameFlow injects these into the pod): `GAME_BACKEND_URL`,
  `GAME_BACKEND_API_TOKEN`.
- **client** (never any key): `PACMAN_BACKEND_URL`.

`GAME_BACKEND_API_TOKEN` must be byte-for-byte identical in the backend and
the server: it signs the session token the server checks at connect time.

## Tests

```bash
cargo test --workspace
```

Most of the coverage is in `shared`, which runs without a display or a socket
because it has no Bevy and no networking.

## Disclaimer

This is an unofficial, fan-made project built for learning and as a GameFlow
integration example. It is not affiliated with, endorsed by, or sponsored by
Bandai Namco Entertainment. "PAC-MAN" and its characters are trademarks of
Bandai Namco. The sprite art in `crates/client/assets/` is placeholder fan art;
replace it with your own or properly licensed assets before distributing widely.
