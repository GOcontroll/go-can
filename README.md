# go-can

GOcontroll CAN interface configuration tool for Moduline controllers
(L4, M1, HMI1) running on i.MX8MM hardware.

Replaces the legacy `/etc/network/interfaces.d/can.conf` mechanism with
declarative per-interface KEY=VALUE configs in `/etc/gocontroll/can.d/`,
plus a systemd template (`can@.service`) and aggregator (`can-setup.target`)
for clean runtime orchestration.

Designed as the canonical CAN-config interface for both interactive use and
future MCP-server / provisioning-tool integration (stable JSON schema).

## CLI

```sh
go-can list                              # all CAN interfaces + status
go-can show can0                         # config + live state of one iface
go-can set can0 bitrate 500000           # edit conf + apply live
go-can set can0 fd on                    # enable CAN-FD (HMI1)
go-can set can0 data-bitrate 1000000     # FD data-phase bitrate
go-can apply can0                        # systemd entry: read conf, apply
go-can defaults --auto-detect            # firstboot: per-baseboard defaults
go-can defaults --baseboard L4           # force a specific baseboard
go-can detect-baseboard                  # → "L4" / "M1" / "HMI1" / "unknown"
go-can reset can0                        # restore baseboard defaults

# Globale flags:
--json                                   # machine-readable (schema_version=1)
--quiet                                  # suppress non-error output
```

`set` keys: `bitrate`, `data-bitrate`, `fd`, `restart-ms`, `txqueuelen`,
`sample-point`, `triple-sampling`, `loopback`, `listen-only`.

## Config files

`/etc/gocontroll/can.d/<iface>.conf`, KEY=VALUE shell-sourceable:

```sh
# /etc/gocontroll/can.d/can0.conf — managed by go-can
BITRATE=250000
TRIPLE_SAMPLING=on
RESTART_MS=100
TXQUEUELEN=20
LOOPBACK=off
LISTEN_ONLY=off
FD=off
DATA_BITRATE=
SAMPLE_POINT=
```

Empty value = kernel default. One file per interface — modular and
fleet-rsync-friendly. Edits survive package upgrades.

## Per-baseboard defaults

| Baseboard | Number of CAN interfaces | Default config |
|---|---|---|
| Moduline L4 | 4 (can0..3) | 250 kbit/s classic, triple-sampling on |
| Moduline M1 | 2 (can0..1) | 250 kbit/s classic, triple-sampling on |
| Moduline HMI1 | 2 (can0..1) | 250 kbit/s classic by default; FD opt-in |

Baseboard is auto-detected from `/sys/firmware/devicetree/base/hardware`.
Both new (L4/M1/HMI1) and legacy (IV/Mini/Display) DTB strings are matched.

## Systemd integration

`can@.service` (template) is `BindsTo=sys-subsystem-net-devices-%i.device`
and runs `go-can apply %i` when the kernel's CAN netdev appears. The
`can-setup.target` aggregator wants all four can@can[0-3] instances; those
that lack a config file skip via `ConditionPathExists=`.

Other services that depend on CAN should add `After=can-setup.target` to
their unit (or a `Requires=` for stricter coupling).

## JSON schema

All `--json` output begins with `"schema_version": 1`. Bumped on any
breaking change. Stable contract for MCP-server and provisioning use.

```json
$ go-can list --json
{
  "schema_version": 1,
  "baseboard": "L4",
  "interfaces": [
    {"name": "can0", "present": true, "up": true, "configured": true,
     "config_path": "/etc/gocontroll/can.d/can0.conf"},
    ...
  ]
}
```

## Exit codes

| Code | Meaning |
|---|---|
| 0 | Success |
| 1 | User error (bad args, unknown iface, unsupported feature) |
| 2 | System error (no permission, parse failure, ip-link failure) |

## Building locally

```sh
cargo build --release
```

The `.deb` is built by GitHub Actions on `v*` tag push (see
`.github/workflows/build-package.yml`). Releases are auto-published to
[`apt.gocontroll.com`](https://apt.gocontroll.com).

## License

MIT
