# Syslog Log Sink — `dev.mcpg.log.syslog`

> class `log_sink` · `native` · package `mcpg-plugin-log-syslog` · artifact `libmcpg_plugin_log_syslog.so` · Apache-2.0

Forwards MCP gateway and plugin log records to a syslog collector. Each record
is rendered as an RFC 5424 line (or RFC 3164 for older collectors) and sent to a
UDP or TCP endpoint — rsyslog, syslog-ng, Fluent Bit, a journald UDP listener —
or written to stdout / stderr. Facility, app-name, hostname, severity floor, and
whether structured fields are appended are all configurable. Reach for it when
your log estate is syslog-shaped and you would rather not run a sidecar just to
reshape the gateway's own output.

## What it does
- Renders RFC 5424 by default: `<PRI>1 <timestamp> <hostname> <app-name> - <msgid> - <message>`,
  with an RFC 3339 UTC timestamp at microsecond precision and the record's
  target as MSGID.
- Renders RFC 3164 on request: `<PRI><Mmm dd hh:mm:ss> <hostname> <tag>: <message>`.
- Computes PRI as `facility * 8 + severity`, mapping levels to severities as
  `error` → 3, `warn` → 4, `info` → 6, and `debug` / `trace` → 7.
- Appends the record's structured fields as a JSON object at the end of the
  message when `include_fields` is on.
- Drops records below `min_level` before formatting, so a noisy floor costs
  nothing downstream.
- Frames TCP with a trailing newline, connects lazily on the first record, and
  reconnects on the next record after a write failure.
- Sends best-effort: a failed write is logged at debug level and the record is
  dropped, never retried and never blocking the caller.
- Declares the `network_outbound` capability, consumed by the UDP and TCP
  destinations.
- Fails closed at load: a malformed config, an unknown field, a facility above
  23, an empty address, or an unbindable socket refuses the plugin instead of
  starting a gateway that silently ships nothing.

## Configuration
Loaded from the flat top-level `plugins:` list, then referenced by id from
`observability.logs.sinks[]`. Both halves are required, and they carry different
things: the `plugins:` entry loads the artifact, grants the capability, and
holds the `config:` block the plugin is built from, while the sinks entry is
purely the routing list that decides which plugin ids receive log records.

```yaml
plugins:
  - id: dev.mcpg.log.syslog
    kind: native
    class: log_sink
    source:
      oci: ghcr.io/mcpg-dev/source-code/plugins/log-syslog:protocol-1
    granted_capabilities:
      - network_outbound
    config:
      destination:
        kind: udp
        address: "10.0.0.5:514"
      format: rfc5424
      facility: 1
      app_name: mcpg-gateway
      hostname: gw-1
      min_level: info
      include_fields: true

observability:
  enabled: true
  logs:
    enabled: true
    level: info
    sinks:
      - kind: stderr
        config:
          format: json
      - kind: dev.mcpg.log.syslog
```

| Field | Type | Default | Description |
|---|---|---|---|
| `destination` | object | *required* | Where lines go — see the table below. |
| `format` | `rfc5424` \| `rfc3164` | `rfc5424` | Syslog wire format. |
| `facility` | integer 0–23 | `1` | Syslog facility; `1` is user-level. Out-of-range values refuse the plugin. |
| `app_name` | string | `mcpg` | APP-NAME in RFC 5424, tag in RFC 3164. |
| `hostname` | string or null | `-` | HOSTNAME field; the nil value `-` when unset. |
| `min_level` | `trace` \| `debug` \| `info` \| `warn` \| `error` | *(none)* | Severity floor. Unset emits every record the signal delivers. |
| `include_fields` | bool | `true` | Append the record's structured fields as JSON to the message. |

### `destination`

| `kind` | Field | Behaviour |
|---|---|---|
| `udp` | `address` (`host:port`) | One datagram per record. |
| `tcp` | `address` (`host:port`) | Newline-framed stream, lazy connect and reconnect. |
| `stdout` | — | Writes the line to stdout. |
| `stderr` | — | Writes the line to stderr. |

Unknown fields are rejected. `destination` has no default: a config block that
omits it refuses the plugin.

An `info` record from target `mcpg::runtime` renders as
`<14>1 2026-06-22T09:00:00.000000Z gw-1 mcpg-gateway - mcpg::runtime - started`.

## Build
The `cdylib-export` feature is on by default, so a standalone build already
produces a loadable artifact; naming the feature explicitly keeps the command
unambiguous:

```bash
cargo build -p mcpg-plugin-log-syslog --features cdylib-export --release   # → target/release/libmcpg_plugin_log_syslog.so
```

## Testing
The unit suite pins the exact line shape for both formats and the PRI
arithmetic, and includes loopback UDP tests that bind an ephemeral socket to
assert both delivery and `min_level` filtering — no external collector needed:

```bash
cargo test -p mcpg-plugin-log-syslog
```

## Sign & load (production)
Sign the artifact, pin/verify via the entry's `signature:` block, and honour
revocations. See <https://mcpg.dev/docs/security/plugin-security>.

## See also
- Observability signals and how sinks fan out: <https://mcpg.dev/docs/reference/configuration>
- Plugin classes and the loading contract: <https://mcpg.dev/docs/plugins/plugins-and-protocol>
- The metrics signal over statsd: `libs/plugins/observability/statsd`
- The traces signal over OTLP: `libs/plugins/observability/otlp`
