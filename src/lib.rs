//! Syslog `log_sink` plugin (`dev.mcpg.log.syslog`).
//!
//! Formats each gateway/plugin log record as an RFC 5424 (default) or RFC 3164
//! syslog line and emits it to a UDP / TCP collector or to stdout / stderr.
//! Best-effort, like every log sink (drop-on-error is acceptable). The
//! formatter is pure; the emit path is plain `std::net` socket I/O. Fails closed
//! on bad config (an invalid facility / destination refuses to load).

use std::io::Write;
use std::net::{TcpStream, UdpSocket};
use std::sync::Mutex;

use chrono::{DateTime, SecondsFormat, Utc};
use mcpg_plugin_protocol::capability::Capability;
use mcpg_plugin_protocol::logs::{LogLevel, LogRecord};
use mcpg_plugin_protocol::{PluginManifest, firstparty_manifest};
use mcpg_plugin_sdk::ffi::SyncLogSink;
use serde::Deserialize;

const PLUGIN_ID: &str = "dev.mcpg.log.syslog";
const MAX_FACILITY: u8 = 23;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "snake_case")]
enum SyslogFormat {
    #[default]
    Rfc5424,
    Rfc3164,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum Destination {
    /// UDP datagram per record (`address` = `host:port`, e.g. `127.0.0.1:514`).
    Udp {
        address: String,
    },
    /// TCP stream, newline-framed (RFC 6587 non-transparent framing).
    Tcp {
        address: String,
    },
    Stdout,
    Stderr,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SyslogConfig {
    destination: Destination,
    #[serde(default)]
    format: SyslogFormat,
    /// Syslog facility (0–23). Default 1 (user-level).
    #[serde(default = "default_facility")]
    facility: u8,
    /// APP-NAME (5424) / tag (3164). Default `mcpg`.
    #[serde(default = "default_app_name")]
    app_name: String,
    /// HOSTNAME field. Default `-` (nil).
    #[serde(default)]
    hostname: Option<String>,
    /// Drop records below this level. Default: emit everything.
    #[serde(default)]
    min_level: Option<LogLevel>,
    /// Append the record's structured `fields` (as JSON) to the message.
    #[serde(default = "default_true")]
    include_fields: bool,
}

fn default_facility() -> u8 {
    1
}
fn default_app_name() -> String {
    "mcpg".to_owned()
}
fn default_true() -> bool {
    true
}

/// Syslog severity for a log level (RFC 5424 §6.2.1).
fn severity(level: LogLevel) -> u8 {
    match level {
        LogLevel::Error => 3,
        LogLevel::Warn => 4,
        LogLevel::Info => 6,
        LogLevel::Debug | LogLevel::Trace => 7,
    }
}

enum Emitter {
    Udp {
        socket: UdpSocket,
        address: String,
    },
    Tcp {
        address: String,
        conn: Mutex<Option<TcpStream>>,
    },
    Stdout,
    Stderr,
}

pub struct SyslogSink {
    manifest: PluginManifest,
    emitter: Emitter,
    format: SyslogFormat,
    facility: u8,
    app_name: String,
    hostname: String,
    min_level: Option<LogLevel>,
    include_fields: bool,
}

/// RFC 3339 (UTC, microseconds) for a unix-nanosecond timestamp.
fn rfc3339(ts_ns: u64) -> String {
    let secs = (ts_ns / 1_000_000_000) as i64;
    let nsec = (ts_ns % 1_000_000_000) as u32;
    DateTime::<Utc>::from_timestamp(secs, nsec)
        .unwrap_or_default()
        .to_rfc3339_opts(SecondsFormat::Micros, true)
}

/// BSD (RFC 3164) timestamp `Mmm dd hh:mm:ss` for a unix-nanosecond timestamp.
fn bsd_timestamp(ts_ns: u64) -> String {
    let secs = (ts_ns / 1_000_000_000) as i64;
    DateTime::<Utc>::from_timestamp(secs, 0)
        .unwrap_or_default()
        .format("%b %e %H:%M:%S")
        .to_string()
}

/// A printable single token (no whitespace), or `-` when empty.
fn token_or_nil(s: &str, max: usize) -> String {
    if s.is_empty() {
        return "-".to_owned();
    }
    let t: String = s
        .chars()
        .map(|c| if c.is_whitespace() { '_' } else { c })
        .take(max)
        .collect();
    if t.is_empty() { "-".to_owned() } else { t }
}

impl SyslogSink {
    /// SDK factory. Fails closed: a bad config or an unbindable UDP socket
    /// panics (→ null handle → boot Err).
    pub fn from_config_json(config_json: &str) -> Self {
        let cfg: SyslogConfig = serde_json::from_str(config_json)
            .unwrap_or_else(|err| panic!("log-syslog: config JSON failed to parse: {err}"));
        if cfg.facility > MAX_FACILITY {
            panic!(
                "log-syslog: facility {} out of range (0–{MAX_FACILITY})",
                cfg.facility
            );
        }
        let emitter = match &cfg.destination {
            Destination::Udp { address } => {
                if address.is_empty() {
                    panic!("log-syslog: udp address must not be empty");
                }
                let socket = UdpSocket::bind("0.0.0.0:0")
                    .unwrap_or_else(|e| panic!("log-syslog: failed to bind UDP socket: {e}"));
                Emitter::Udp {
                    socket,
                    address: address.clone(),
                }
            }
            Destination::Tcp { address } => {
                if address.is_empty() {
                    panic!("log-syslog: tcp address must not be empty");
                }
                Emitter::Tcp {
                    address: address.clone(),
                    conn: Mutex::new(None),
                }
            }
            Destination::Stdout => Emitter::Stdout,
            Destination::Stderr => Emitter::Stderr,
        };

        Self {
            manifest: firstparty_manifest! {
                id: PLUGIN_ID,
                name: "Syslog Log Sink",
                class: LogSink,
                capabilities: [Capability::NetworkOutbound],
            },
            emitter,
            format: cfg.format,
            facility: cfg.facility,
            app_name: cfg.app_name,
            hostname: cfg.hostname.unwrap_or_else(|| "-".to_owned()),
            min_level: cfg.min_level,
            include_fields: cfg.include_fields,
        }
    }

    fn message(&self, record: &LogRecord) -> String {
        let mut msg = record.message.clone();
        if self.include_fields
            && !record.fields.is_empty()
            && let Ok(j) = serde_json::to_string(&record.fields)
        {
            msg.push(' ');
            msg.push_str(&j);
        }
        msg
    }

    /// Render a record into a single syslog line (no trailing newline).
    fn render(&self, record: &LogRecord) -> String {
        let pri = (self.facility as u16) * 8 + severity(record.level) as u16;
        let msg = self.message(record);
        match self.format {
            SyslogFormat::Rfc5424 => {
                let msgid = token_or_nil(&record.target, 32);
                format!(
                    "<{pri}>1 {ts} {host} {app} - {msgid} - {msg}",
                    ts = rfc3339(record.timestamp_ns),
                    host = self.hostname,
                    app = self.app_name,
                )
            }
            SyslogFormat::Rfc3164 => {
                format!(
                    "<{pri}>{ts} {host} {tag}: {msg}",
                    ts = bsd_timestamp(record.timestamp_ns),
                    host = self.hostname,
                    tag = self.app_name,
                )
            }
        }
    }

    fn emit_tcp(address: &str, conn: &Mutex<Option<TcpStream>>, line: &str) {
        let mut guard = conn.lock().expect("tcp conn mutex");
        if guard.is_none() {
            match TcpStream::connect(address) {
                Ok(s) => *guard = Some(s),
                Err(e) => {
                    tracing::debug!(error = %e, "log-syslog: tcp connect failed; dropping record");
                    return;
                }
            }
        }
        if let Some(stream) = guard.as_mut() {
            let framed = format!("{line}\n");
            if let Err(e) = stream.write_all(framed.as_bytes()) {
                tracing::debug!(error = %e, "log-syslog: tcp write failed; will reconnect");
                *guard = None;
            }
        }
    }
}

impl SyncLogSink for SyslogSink {
    fn manifest(&self) -> &PluginManifest {
        &self.manifest
    }

    fn emit(&self, record: &LogRecord) {
        if let Some(min) = self.min_level
            && record.level < min
        {
            return;
        }
        let line = self.render(record);
        match &self.emitter {
            Emitter::Udp { socket, address } => {
                if let Err(e) = socket.send_to(line.as_bytes(), address.as_str()) {
                    tracing::debug!(error = %e, "log-syslog: udp send failed; dropping record");
                }
            }
            Emitter::Tcp { address, conn } => Self::emit_tcp(address, conn, &line),
            Emitter::Stdout => {
                let mut out = std::io::stdout().lock();
                let _ = writeln!(out, "{line}");
            }
            Emitter::Stderr => {
                let mut out = std::io::stderr().lock();
                let _ = writeln!(out, "{line}");
            }
        }
    }
}

mcpg_plugin_sdk::declare_plugin! {
    plugin_id: "dev.mcpg.log.syslog",
    plugin_version: env!("CARGO_PKG_VERSION"),
    descriptor_yaml: include_str!("../plugin.yaml"),
    capabilities: &[Capability::NetworkOutbound],
    entities: [
        log_sink as sink {
            inner_name: "",
            plugin_type: SyslogSink,
            factory: |cfg: &str, _host: ::mcpg_plugin_sdk::HostHandle| SyslogSink::from_config_json(cfg),
        },
    ],
}

#[cfg(test)]
mod tests;
