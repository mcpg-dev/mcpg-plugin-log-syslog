use std::net::UdpSocket;
use std::time::Duration;

use mcpg_plugin_protocol::logs::{LogLevel, LogRecord};
use mcpg_plugin_sdk::ffi::SyncLogSink;
use serde_json::{Value, json};

use super::{PLUGIN_ID, SyslogSink};

const TS: u64 = 1_700_000_000_000_000_000; // fixed for deterministic timestamps

fn build(cfg: Value) -> SyslogSink {
    SyslogSink::from_config_json(&cfg.to_string())
}

fn rec(level: LogLevel, msg: &str) -> LogRecord {
    LogRecord {
        timestamp_ns: TS,
        level,
        target: "mcpg::test".into(),
        message: msg.into(),
        fields: Default::default(),
        span_id: None,
        trace_id: None,
        request_id: None,
        identity: None,
        node_id: None,
        plugin_id: None,
    }
}

#[test]
fn manifest_is_correct() {
    use mcpg_plugin_protocol::PluginClass;
    use mcpg_plugin_protocol::capability::Capability;
    let p = build(json!({ "destination": { "kind": "stdout" } }));
    let m = SyncLogSink::manifest(&p);
    assert_eq!(m.id, PLUGIN_ID);
    assert_eq!(m.plugin_class, PluginClass::LogSink);
    assert!(
        m.required_capabilities
            .iter()
            .any(|c| matches!(c, Capability::NetworkOutbound)),
        "syslog declares network egress"
    );
}

#[test]
fn rfc5424_render_shape() {
    let p = build(
        json!({ "destination": { "kind": "stdout" }, "hostname": "host1", "app_name": "gw" }),
    );
    // facility 1 (default) + Info severity 6 → PRI 14.
    let line = p.render(&rec(LogLevel::Info, "hello"));
    assert!(line.starts_with("<14>1 "), "{line}");
    assert!(line.contains(" host1 gw - mcpg::test - hello"), "{line}");
}

#[test]
fn rfc3164_render_shape() {
    let p = build(json!({
        "destination": { "kind": "stdout" }, "format": "rfc3164",
        "hostname": "host1", "app_name": "gw"
    }));
    let line = p.render(&rec(LogLevel::Warn, "careful"));
    // facility 1 + Warn severity 4 → PRI 12.
    assert!(line.starts_with("<12>"), "{line}");
    assert!(line.contains(" host1 gw: careful"), "{line}");
}

#[test]
fn pri_uses_facility_and_severity() {
    let p = build(json!({ "destination": { "kind": "stdout" }, "facility": 16 }));
    // facility 16 * 8 + Error severity 3 = 131.
    assert!(p.render(&rec(LogLevel::Error, "boom")).starts_with("<131>"));
}

#[test]
fn include_fields_appends_json() {
    let p = build(json!({ "destination": { "kind": "stdout" } }));
    let mut r = rec(LogLevel::Info, "msg");
    r.fields.insert("k".into(), json!("v"));
    let line = p.render(&r);
    assert!(line.contains(r#"{"k":"v"}"#), "{line}");
}

#[test]
fn fields_excluded_when_disabled() {
    let p = build(json!({ "destination": { "kind": "stdout" }, "include_fields": false }));
    let mut r = rec(LogLevel::Info, "msg");
    r.fields.insert("k".into(), json!("v"));
    assert!(!p.render(&r).contains("\"k\""));
}

#[test]
fn udp_loopback_emit_delivers_line() {
    let listener = UdpSocket::bind("127.0.0.1:0").unwrap();
    listener
        .set_read_timeout(Some(Duration::from_millis(500)))
        .unwrap();
    let addr = listener.local_addr().unwrap().to_string();

    let p = build(json!({ "destination": { "kind": "udp", "address": addr } }));
    let record = rec(LogLevel::Info, "over the wire");
    let expected = p.render(&record);
    p.emit(&record);

    let mut buf = [0u8; 4096];
    let (n, _src) = listener
        .recv_from(&mut buf)
        .expect("datagram should arrive");
    assert_eq!(&buf[..n], expected.as_bytes());
}

#[test]
fn min_level_drops_lower_records() {
    let listener = UdpSocket::bind("127.0.0.1:0").unwrap();
    listener
        .set_read_timeout(Some(Duration::from_millis(250)))
        .unwrap();
    let addr = listener.local_addr().unwrap().to_string();

    let p =
        build(json!({ "destination": { "kind": "udp", "address": addr }, "min_level": "warn" }));
    p.emit(&rec(LogLevel::Info, "too quiet")); // below warn → dropped

    let mut buf = [0u8; 4096];
    assert!(
        listener.recv_from(&mut buf).is_err(),
        "info must be dropped under min_level=warn"
    );
}

#[test]
fn min_level_passes_equal_or_higher() {
    let listener = UdpSocket::bind("127.0.0.1:0").unwrap();
    listener
        .set_read_timeout(Some(Duration::from_millis(500)))
        .unwrap();
    let addr = listener.local_addr().unwrap().to_string();

    let p =
        build(json!({ "destination": { "kind": "udp", "address": addr }, "min_level": "warn" }));
    p.emit(&rec(LogLevel::Error, "loud"));

    let mut buf = [0u8; 4096];
    assert!(listener.recv_from(&mut buf).is_ok());
}

#[test]
fn stdout_emit_does_not_panic() {
    let p = build(json!({ "destination": { "kind": "stderr" } }));
    p.emit(&rec(LogLevel::Info, "to stderr"));
}

#[test]
#[should_panic(expected = "facility")]
fn bad_facility_panics() {
    build(json!({ "destination": { "kind": "stdout" }, "facility": 24 }));
}

#[test]
#[should_panic(expected = "config JSON failed to parse")]
fn unknown_field_panics() {
    build(json!({ "destination": { "kind": "stdout" }, "bogus": 1 }));
}

#[test]
#[should_panic(expected = "config JSON failed to parse")]
fn malformed_config_panics() {
    SyslogSink::from_config_json("{ not json");
}

#[test]
#[should_panic(expected = "address must not be empty")]
fn empty_udp_address_panics() {
    build(json!({ "destination": { "kind": "udp", "address": "" } }));
}
