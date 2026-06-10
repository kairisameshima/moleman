# moleman 🐹

A terminal UI for managing AWS tunnels and SSO sessions in one place — SSM
port-forwards to ECS/Cloud Map services, an RDS instance picker, and `ssh -L`
tunnels to the Temporal UIs. See which profiles are authenticated, bring up the
tunnels you need, and re-login when a token expires, all without leaving the
terminal.

## What it does

- **Profiles panel** — every profile in `~/.aws/config` with live SSO token
  validity (`● valid 3h12m` / `▲ expiring 8m` / `✕ expired` / `○ login`). Token
  state is read straight from `~/.aws/sso/cache/`, keyed by `sha1(session_name)`
  the same way the AWS CLI does.
- **Tunnels panel**, grouped:
  - **Services** — discovered live from AWS Cloud Map; each tunnels through the
    services bastion via SSM. Endpoints are resolved at start time.
  - **Databases** — press `d` to discover RDS/Aurora instances for the active
    profile and pick one; it tunnels through that account's bastion on the next
    free local port.
  - **Temporal** — `ssh -L` through the bastions using your PEM keys.
- **One-key SSO login** — `l` runs `aws sso login --sso-session <name>` (opens a
  browser); the badge flips to valid when it completes.
- Clean teardown: quitting kills every spawned tunnel (and the
  `session-manager-plugin`/`ssh` children) via process-group signals.

## Prerequisites

- Rust (stable) — `cargo build`
- [`aws` CLI v2](https://docs.aws.amazon.com/cli/latest/userguide/getting-started-install.html)
  configured with your SSO profiles
- [`session-manager-plugin`](https://docs.aws.amazon.com/systems-manager/latest/userguide/session-manager-working-with-install-plugin.html)
  (required for SSM port-forwarding)
- `ssh` and your Temporal PEM keys (default location `~/Downloads/`, mode `0600`)

## Run

```sh
cargo run            # debug
cargo build --release && ./target/release/moleman
```

On first run it writes a config to `~/.config/moleman/config.toml` seeded with
the known service ports, RDS bastions (per SSO session), and Temporal entries.
Edit that file to add tunnels, remap ports, or point at moved PEM keys.

## Keys

| Key | Action |
|-----|--------|
| `Tab` | switch focus between Profiles and Tunnels |
| `↑/↓` or `k/j` | move selection |
| `Enter` | Profiles: make active · Tunnels: start selected |
| `s` / `x` | start / stop selected tunnel |
| `S` / `X` | start / stop all tunnels in the selected group |
| `d` | discover & pick an RDS instance to tunnel (uses active profile) |
| `l` | `aws sso login` for the selected/active profile's session |
| `r` | refresh (rescan SSO + re-discover Cloud Map services) |
| `q` | quit (tears down all tunnels) |

## Config

`~/.config/moleman/config.toml`:

- `[services]` — `profile` + `bastion` used for Cloud Map discovery and
  port-forwarding; `[services.ports]` pins conventional local ports; `fallback`
  is shown if discovery returns nothing.
- `[rds]` — `local_port_base` and `[rds.bastions]` mapping each SSO-session name
  to the bastion that can reach that account's databases.
- `[[temporal]]` — one block per Temporal tunnel (`ssh -L` via PEM).

## Notes

- ssh refuses keys readable by group/other; moleman checks PEM permissions and
  tells you to `chmod 600` rather than failing cryptically.
- A tunnel whose local port is already bound (e.g. you ran a script manually) is
  shown as `external` rather than being started a second time.
- Temporal EC2 host IPs are public and may rotate — edit them in the config if a
  connection starts refusing.
