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
go-can set mb_can1 data-bitrate 2000000  # the same on a Multibus module bus
go-can apply can0                        # systemd entry: read conf, apply
go-can defaults --auto-detect            # firstboot: per-baseboard defaults
go-can defaults --baseboard L4           # force a specific baseboard
go-can detect-baseboard                  # â†’ "L4" / "M1" / "HMI1" / "unknown"
go-can reset can0                        # restore baseboard defaults
go-can migrate [--dry-run]               # take over a legacy can.conf (see below)

# Globale flags:
--json                                   # machine-readable (schema_version=1)
--quiet                                  # suppress non-error output
```

`set` keys: `bitrate`, `data-bitrate`, `fd`, `restart-ms`, `txqueuelen`,
`sample-point`, `triple-sampling`, `loopback`, `listen-only`.

## CAN FD and the two bitrates

A CAN FD bus has two bitrates: `bitrate` for the arbitration phase and for
classic frames, and `data-bitrate` for the data phase of an FD frame.

- `fd on` without a data bitrate stores 2 Mbit/s (`DATA_BITRATE=2000000`), since
  the kernel refuses CAN FD without one. A data bitrate that was chosen before
  is kept.
- `fd off` really switches the interface back to classic CAN. A network
  interface keeps CAN FD over a down and an up, so up to 0.1.13, which simply
  left `fd` out, a bus stayed in FD until the controller was rebooted.
- The data bitrate may not be below the bitrate; the kernel refuses the pair.
- A bus without a config file that already runs CAN FD - a Multibus bus raised by
  go-multibus in mode `canfd` - keeps FD and its data bitrate when its first
  config is written, so a first `set bitrate` does not turn it into a classic bus.

The UI (`go-can` without arguments) lists every bus with its bitrate and, for a
CAN FD bus, its data bitrate. Enter on a classic-only bus opens the bitrate
picker (125, 250, 500, 1000 kbit/s). Enter on a bus whose controller does
CAN FD opens its settings: `Bitrate`, `CAN FD` and, with FD on, `Data bitrate`.
The data bitrate picker lists 500 kbit/s to 8 Mbit/s, less whatever is below
the bitrate or the controller cannot make exactly from its clock - the kernel
would settle for a rate a few percent off, which a CAN FD data phase does not
forgive. A Multibus module (170 MHz) gets 500, 1000, 2000 and 5000 kbit/s.

**Multibus module buses** (`mb_can1`, `mb7_can1`): whether such a bus is classic
CAN or CAN FD is its interface mode and is chosen in go-multibus
(`go-multibus set 1 canfd`), which sets go-can's `FD` line to match whenever the
module comes up. The UI shows `CAN FD` there as `chosen in go-multibus`;
`go-can set mb_can1 fd on` works until the next bring-up and says so. The
bitrates of a module bus are set here like any other.

## Config files

`/etc/gocontroll/can.d/<iface>.conf`, KEY=VALUE shell-sourceable:

```sh
# /etc/gocontroll/can.d/can0.conf â€” managed by go-can
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

Empty value = kernel default. One file per interface â€” modular and
fleet-rsync-friendly. Edits survive package upgrades.

## Migrating from can.conf (older images)

Images up to 2026-04-26 set up the CAN buses with ifupdown, from
`/etc/network/interfaces.d/can.conf`. With go-can installed next to it, both
bring the same buses up at boot, and which settings a bus then gets is
unpredictable. From 0.1.13 the package's `postinst` runs `go-can migrate`
whenever that file exists, on a first install and on an upgrade:

- The settings of every bus that ifupdown brings up at boot (`auto` or
  `allow-hotplug`) are written to `/etc/gocontroll/can.d/<iface>.conf`. They
  are taken over literally; what can.conf leaves out gets the kernel default
  (txqueuelen, which go-can always sets, gets go-can's 20).
- A bus that already has a go-can config keeps it; go-can reports when can.conf
  differs. A bus without `auto` gets no config, so it stays down, as before.
- can.conf is moved to `/etc/gocontroll/can.conf.migrated` (`.1`, `.2`, â€¦ when
  that exists). It has to leave `interfaces.d`: ifupdown sources that directory
  with a wildcard, so a renamed copy there would still run.
- The running buses are not touched. From the next boot `can@.service` sets
  them up, with the settings they run with now.
- **All or nothing.** When can.conf holds anything go-can cannot express â€” an
  interface that is not `canN`, an ifupdown option other than the `ip link set
  $IFACE â€¦` and `up`/`down` lines, or a CAN parameter outside `bitrate`,
  `dbitrate`, `sample-point`, `restart-ms`, `triple-sampling`, `loopback`,
  `listen-only` and `fd` â€” nothing is changed, the install still succeeds, and
  go-can says which line stopped it. Then apply the bitrate of every bus with
  go-can and remove can.conf by hand.

`go-can migrate --dry-run` shows what would happen without changing anything;
`--from <file>` reads another file. A missing file is not an error (exit 0).

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
the GOcontroll apt repository (`apt.gocontroll.tech`, with `apt.gocontroll.com`
as fallback; both serve the same bucket).

## License

MIT
