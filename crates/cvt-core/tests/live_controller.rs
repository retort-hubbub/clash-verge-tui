//! Optional live checks against a **real** `mihomo` core.
//!
//! Every test here is a no-op unless `CVT_LIVE_CONTROLLER` is set, so the suite
//! stays green on a machine with no core:
//!
//! ```text
//! CVT_LIVE_CONTROLLER='127.0.0.1:19090|testsecret' \
//!   CVT_LIVE_LOG=/path/to/core.log \
//!   cargo test -p cvt-core --test live_controller -- --test-threads=1 --nocapture
//! ```
//!
//! The point is to confirm that what `client_contract.rs` asserts about a
//! hand-rolled fake controller is what the real core does — and to settle the
//! few claims only a live core can settle (which built-in policies exist,
//! whether percent-encoded names resolve, what `probe()` actually triggers).
//!
//! One test function on purpose: these checks share a single running core, and
//! cargo runs test functions in parallel. It starts by pushing a known
//! configuration, so it can be re-run without cleaning up by hand.

#![allow(clippy::unwrap_used, clippy::panic)]

use cvt_core::error::Error;
use cvt_core::mihomo::client::Client;
use cvt_core::mihomo::endpoint::{Endpoint, Transport};
use cvt_core::mihomo::types::ConfigPatch;
use serde_json::{Value, json};

/// `host:port` or `host:port|secret` from the environment.
fn live_endpoint() -> Option<Endpoint> {
    let raw = std::env::var("CVT_LIVE_CONTROLLER").ok()?;
    let (addr, secret) = match raw.split_once('|') {
        Some((addr, secret)) if !secret.is_empty() => (addr, Some(secret.to_owned())),
        _ => (raw.as_str(), None),
    };
    Some(Endpoint::tcp(addr, secret))
}

/// The configuration this test installs while it runs. It is a complete
/// document because `PUT /configs` replaces everything.
#[allow(clippy::needless_pass_by_value)] // a `json!` value reads better at the call site
fn installed_config(rules: Value) -> Value {
    json!({
        "mixed-port": 17990,
        "external-controller": "127.0.0.1:19090",
        "secret": "testsecret",
        "mode": "rule",
        "log-level": "info",
        "ipv6": false,
        "allow-lan": false,
        "proxies": [
            {"name": "JP 01", "type": "socks5", "server": "127.0.0.1", "port": 1080},
            {"name": "node b/slash", "type": "socks5", "server": "127.0.0.1", "port": 1081},
        ],
        "proxy-groups": [
            {"name": "grp-select", "type": "select", "proxies": ["JP 01", "node b/slash", "DIRECT"]},
            {"name": "auto", "type": "url-test", "proxies": ["JP 01", "node b/slash"],
             "url": "http://127.0.0.1:9/", "interval": 3600},
        ],
        "rule-providers": {
            "rp-inline": {"type": "inline", "behavior": "domain",
                          "payload": ["example.com", "example.org"]},
        },
        "rules": rules,
    })
}

#[tokio::test]
async fn live_core_contract() {
    let Some(endpoint) = live_endpoint() else {
        eprintln!("CVT_LIVE_CONTROLLER is unset; skipping the live checks");
        return;
    };
    let client = Client::new(endpoint.clone()).unwrap();
    let http = reqwest::Client::builder().no_proxy().build().unwrap();
    let base = endpoint.base_url();
    let secret = "testsecret";

    // ---------------------------------------------------------------- system
    let version = client.version().await.unwrap();
    println!("live core version = {}", version.version);
    assert!(version.meta, "every mihomo build reports meta: true");

    let raw_version: Value = http
        .get(format!("{base}/version"))
        .bearer_auth(secret)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let keys: Vec<&str> = raw_version
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    println!("raw /version keys = {keys:?}");
    assert_eq!(
        keys,
        vec!["meta", "version"],
        "the spec says there is no `premium` key"
    );
    assert!(client.is_reachable().await);
    assert_eq!(client.hello().await.unwrap().hello, "mihomo");

    // A wrong secret is a 401 with a JSON envelope.
    let wrong = Client::new(Endpoint::tcp(
        transport_addr(&endpoint),
        Some("wrong".into()),
    ))
    .unwrap();
    match wrong.version().await.unwrap_err() {
        Error::Api { status, body, .. } => {
            assert_eq!(status, 401);
            assert_eq!(body, "Unauthorized");
        }
        other => panic!("expected a 401 Api error, got {other:?}"),
    }

    // ------------------------------------------------------ install a config
    let base_rules = json!([
        "DOMAIN-SUFFIX,example.com,grp-select",
        "RULE-SET,rp-inline,DIRECT",
        "MATCH,DIRECT"
    ]);
    let body = serde_json::to_string(&installed_config(base_rules)).unwrap();
    client
        .reload_configs(None, Some(&body), true)
        .await
        .expect("PUT /configs with a payload must succeed");
    println!("installed the test configuration through Client::reload_configs");

    // -------------------------------------------------------------- proxies
    let proxies = client.proxies().await.unwrap();
    println!("proxy count = {}", proxies.proxies.len());
    for builtin in [
        "DIRECT",
        "REJECT",
        "REJECT-DROP",
        "PASS",
        "PASS-RULE",
        "COMPATIBLE",
        "GLOBAL",
    ] {
        assert!(
            proxies.proxies.contains_key(builtin),
            "`{builtin}` is always present; validate::check calls PASS-RULE and GLOBAL dangling (F1)"
        );
    }
    let group = proxies.proxies.get("grp-select").unwrap();
    assert!(group.is_group());
    assert_eq!(group.kind, "Selector");
    assert!(group.id.is_none(), "groups carry no id");
    // A `select` group defaults to its first member, but a choice made by an
    // earlier run survives in the core's `cache.db` and outlives a
    // `PUT /configs`, so assert the round trip rather than a virgin cache.
    assert!(
        group.now.is_some(),
        "a select group always reports a current pick"
    );
    for pick in ["JP 01", "node b/slash", "JP 01"] {
        client.select("grp-select", pick).await.unwrap();
        assert_eq!(
            client.group("grp-select").await.unwrap().now.as_deref(),
            Some(pick),
            "PUT /proxies/<name> selects, and the choice is visible"
        );
    }

    // Percent-encoding is the reason `encode_segment` exists.
    for name in ["JP 01", "node b/slash", "DIRECT"] {
        let one = client
            .proxy(name)
            .await
            .unwrap_or_else(|e| panic!("GET /proxies/{name:?} failed: {e}"));
        assert_eq!(one.name, name);
    }
    assert!(client.group("grp-select").await.unwrap().is_group());

    // ---------------------------------------------------------------- rules
    let rules = client.rules().await.unwrap();
    let kinds: Vec<&str> = rules.iter().map(|r| r.kind.as_str()).collect();
    println!("rule kinds = {kinds:?}");
    assert_eq!(
        kinds,
        vec!["DomainSuffix", "RuleSet", "Match"],
        "rule types arrive in Go PascalCase, not `DOMAIN-SUFFIX`"
    );
    assert_eq!(
        rules[0].as_config_rule(),
        "DOMAIN-SUFFIX,example.com,grp-select"
    );
    assert_eq!(rules[1].as_config_rule(), "RULE-SET,rp-inline,DIRECT");
    assert_eq!(rules[2].as_config_rule(), "MATCH,DIRECT");

    // `GET /providers/rules/{name}` does not exist: 405 with `Allow: PUT`.
    let raw_405 = http
        .get(format!("{base}/providers/rules/rp-inline"))
        .bearer_auth(secret)
        .send()
        .await
        .unwrap();
    println!("GET /providers/rules/rp-inline -> {}", raw_405.status());
    assert_eq!(raw_405.status().as_u16(), 405);
    assert_eq!(raw_405.headers().get("allow").unwrap(), "PUT");

    // ---------------------------------------------------------- connections
    let snapshot = client.connections().await.unwrap();
    println!(
        "connections = {:?}, totals up/down = {}/{}",
        snapshot.connections, snapshot.upload_total, snapshot.download_total
    );
    assert!(
        snapshot.items().is_empty(),
        "an idle core sends `null`, not `[]`"
    );
    let raw_connections = http
        .get(format!("{base}/connections"))
        .bearer_auth(secret)
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    assert!(
        raw_connections.contains(r#""connections":null"#),
        "raw body was {raw_connections:?}"
    );
    assert!(
        raw_connections.ends_with('\n'),
        "chi/render writes a trailing newline: {raw_connections:?}"
    );

    // ------------------------------------------------------------ providers
    let rule_providers = client.rule_providers().await.unwrap();
    let rp = rule_providers.providers.get("rp-inline").unwrap();
    assert_eq!(rp.vehicle_type, "Inline");
    assert_eq!(rp.format, "", "inline providers never set `format`");
    assert_eq!(rp.payload, ["example.com", "example.org"]);
    assert_eq!(
        client.rule_provider("rp-inline").await.unwrap().rule_count,
        2
    );
    match client.rule_provider("ghost").await.unwrap_err() {
        Error::Api { status, .. } => assert_eq!(status, 404),
        other => panic!("expected a 404 Api error, got {other:?}"),
    }
    let proxy_providers = client.proxy_providers().await.unwrap();
    assert!(
        proxy_providers.providers.contains_key("default"),
        "the synthetic `default` provider is always there"
    );
    assert!(
        proxy_providers.providers["default"].is_synthetic(),
        "and it is marked Compatible"
    );
    client
        .update_proxy_provider("default")
        .await
        .expect("a Compatible provider's update is a no-op that still answers 204");

    // -------------------------------------------------------------- configs
    let configs = client.configs().await.unwrap();
    assert_eq!(configs.mixed_port, Some(17990));
    assert_eq!(configs.mode.as_deref(), Some("rule"));
    client
        .patch_configs(&ConfigPatch {
            mode: Some("global".to_owned()),
            ..ConfigPatch::default()
        })
        .await
        .unwrap();
    assert_eq!(
        client.configs().await.unwrap().mode.as_deref(),
        Some("global"),
        "PATCH /configs applies immediately"
    );
    client
        .patch_configs(&ConfigPatch {
            mode: Some("rule".to_owned()),
            ..ConfigPatch::default()
        })
        .await
        .unwrap();

    // --------------------------------------------------------------- writes
    client.select("grp-select", "node b/slash").await.unwrap();
    assert_eq!(
        client.group("grp-select").await.unwrap().now.as_deref(),
        Some("node b/slash")
    );
    match client.select("DIRECT", "DIRECT").await.unwrap_err() {
        Error::Api { status, body, .. } => {
            assert_eq!(status, 400);
            assert_eq!(body, "Must be a Selector");
        }
        other => panic!("expected the documented 400, got {other:?}"),
    }

    let mut changes = std::collections::BTreeMap::new();
    changes.insert(0u32, true);
    client.set_rules_disabled(&changes).await.unwrap();
    assert!(
        client.rules().await.unwrap()[0].stats().disabled,
        "PATCH /rules/disable took effect"
    );
    let mut changes = std::collections::BTreeMap::new();
    changes.insert(0u32, false);
    client.set_rules_disabled(&changes).await.unwrap();

    client
        .storage_put("cvt-verify", &json!({"checked": true}))
        .await
        .unwrap();
    assert_eq!(
        client.storage_get("cvt-verify").await.unwrap(),
        Some(json!({"checked": true}))
    );
    client.storage_delete("cvt-verify").await.unwrap();
    assert!(client.storage_get("cvt-verify").await.unwrap().is_none());

    // --------------------------------------------------------------- delays
    // Nothing listens on the test URL, so every delay is a failure — and the
    // client must surface that rather than reporting a 0 ms success.
    match client
        .proxy_delay("JP 01", "http://127.0.0.1:9/", 1000, None)
        .await
    {
        Err(Error::Api { status, .. }) => assert!(
            status == 503 || status == 504,
            "a failed test answers 503/504, got {status}"
        ),
        other => panic!("expected an Api error, got {other:?}"),
    }
    match client
        .group_delay("auto", "http://127.0.0.1:9/", 1000, None)
        .await
    {
        Err(Error::Api { status, .. }) => assert!(status == 503 || status == 504, "{status}"),
        Ok(map) => assert!(map.is_empty() || map.values().all(|d| *d == 0), "{map:?}"),
        Err(other) => panic!("expected an Api error, got {other:?}"),
    }

    // ----------------------------------------------------------- error paths
    match client.proxy("ghost").await.unwrap_err() {
        Error::Api { status, body, .. } => {
            assert_eq!(status, 404);
            assert_eq!(body, "Resource not found", "JSON error envelope");
        }
        other => panic!("expected a 404 Api error, got {other:?}"),
    }
    match client.dns_query("example.com", "A").await.unwrap_err() {
        Error::Api { status, body, .. } => {
            assert_eq!(status, 500);
            assert_eq!(body, "DNS section is disabled");
        }
        other => panic!("the config has no dns section, so 500 is expected: {other:?}"),
    }
    // `PUT /debug/gc` is only mounted when the core logs at debug level: the
    // plain-text 404 must be recognised as "this build has no such route".
    match client.force_gc().await {
        Ok(()) => println!("this core was started with log-level: debug"),
        Err(Error::Unsupported(reason)) => assert!(reason.contains("log-level: debug"), "{reason}"),
        Err(other) => panic!("expected Ok or Unsupported, got {other:?}"),
    }
    let absent = http
        .get(format!("{base}/ui"))
        .bearer_auth(secret)
        .send()
        .await
        .unwrap();
    println!(
        "GET /ui -> {} (external-ui is not configured)",
        absent.status()
    );

    // -------------------------------------------------- built-in policy rules
    // The validator reports `E-DANGLING-POLICY` for both of these. If the core
    // accepts them, the validator is refusing a valid configuration (F1).
    let body = serde_json::to_string(&installed_config(json!([
        "DOMAIN-SUFFIX,g.global,GLOBAL",
        "DOMAIN-SUFFIX,p.pass,PASS-RULE",
        "MATCH,DIRECT"
    ])))
    .unwrap();
    let response = http
        .put(format!("{base}/configs?force=true"))
        .bearer_auth(secret)
        .json(&json!({"payload": body}))
        .send()
        .await
        .unwrap();
    println!(
        "PUT /configs with rules targeting GLOBAL and PASS-RULE -> {}",
        response.status()
    );
    assert!(
        response.status().is_success(),
        "the live core rejected a config whose rules target GLOBAL / PASS-RULE: {} {}",
        response.status(),
        response.text().await.unwrap_or_default()
    );
    let live_rules = client.rules().await.unwrap();
    assert_eq!(live_rules.len(), 3, "the core kept all three rules");
    assert_eq!(live_rules[0].proxy, "GLOBAL");
    assert_eq!(live_rules[1].proxy, "PASS-RULE");

    // Restore the rules the earlier checks expect, so the test is idempotent.
    let body = serde_json::to_string(&installed_config(json!([
        "DOMAIN-SUFFIX,example.com,grp-select",
        "RULE-SET,rp-inline,DIRECT",
        "MATCH,DIRECT"
    ])))
    .unwrap();
    client
        .reload_configs(None, Some(&body), true)
        .await
        .unwrap();

    // -------------------------------------------------- capability probing
    // Watch the core's own log while `probe()` runs: if geodata is being
    // fetched, the probe is *doing* work rather than asking questions (F18).
    let log_path = std::env::var("CVT_LIVE_LOG").unwrap_or_default();
    let before = std::fs::read_to_string(&log_path).unwrap_or_default();
    let started = std::time::Instant::now();
    let caps = client.probe().await.unwrap();
    let elapsed = started.elapsed();
    let after = std::fs::read_to_string(&log_path).unwrap_or_default();
    println!("probe -> {caps:?} in {elapsed:?}");
    let grew: Vec<&str> = after
        .lines()
        .skip(before.lines().count())
        .filter(|l| {
            let lower = l.to_ascii_lowercase();
            lower.contains("geo") || lower.contains("download")
        })
        .collect();
    assert!(
        grew.is_empty(),
        "probing must not start an update, but the core said: {grew:#?}"
    );
    assert!(
        caps.rules_disable,
        "PATCH /rules/disable is mounted on a normal build"
    );
    assert!(caps.configs_write);
    assert!(!caps.debug, "this core was not started with -debug");
    assert!(
        caps.upgrade,
        "the upgrade family exists, and a wrong method proves it without \
         running the geodata updater (F18)"
    );
}

/// The address half of a TCP endpoint.
fn transport_addr(endpoint: &Endpoint) -> String {
    match &endpoint.transport {
        Transport::Tcp(addr) => addr.clone(),
        other => panic!("expected a TCP endpoint, got {other:?}"),
    }
}
