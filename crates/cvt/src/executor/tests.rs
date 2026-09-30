use super::*;

#[cfg(test)]
mod speedtest_contract_tests {
    use super::{parse_exit_ip, speedtest_download_mbps};

    #[test]
    fn speedtest_go_reports_bytes_per_second() {
        let sample = br#"{"servers":[{"dl_speed":12500000.0}]}"#;
        assert_eq!(speedtest_download_mbps(sample), Some(100.0));
        assert_eq!(speedtest_download_mbps(b"{}"), None);
        assert_eq!(
            speedtest_download_mbps(br#"{"servers":[{"dl_speed":-1}]}"#),
            None
        );
    }

    #[test]
    fn exit_ip_parses_both_providers_and_rejects_error_objects() {
        let ipapi = serde_json::json!({"ip":"203.0.113.7","country_name":"Example","org":"Net"});
        let ipsb = serde_json::json!({"ip":"2001:db8::7","country":"Example","organization":"Net"});
        assert_eq!(
            parse_exit_ip(&ipapi).map(|info| info.organization),
            Ok("Net".to_owned())
        );
        assert_eq!(
            parse_exit_ip(&ipsb).map(|info| info.ip),
            Ok("2001:db8::7".to_owned())
        );
        assert!(parse_exit_ip(&serde_json::json!({"ip":"rate limited"})).is_err());
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod behavior_tests {
    use super::*;

    #[test]
    fn parses_icmp_round_trip_without_mistaking_packet_statistics_for_latency() {
        assert_eq!(parse_ping_ms("64 bytes: time=12.4 ms\n1 packets"), Some(13));
        assert_eq!(parse_ping_ms("64 bytes: time=0,8 ms"), Some(1));
        assert_eq!(parse_ping_ms("Reply from 127.0.0.1: time<1ms"), Some(1));
        assert_eq!(parse_ping_ms("1 packets transmitted, 0 received"), None);
    }

    #[tokio::test]
    async fn tcp_probe_measures_an_open_listener_and_rejects_a_closed_one() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        assert!(tcp_connect_ms("127.0.0.1", port, 500).await.is_ok());
        drop(listener);
        assert!(tcp_connect_ms("127.0.0.1", 0, 500).await.is_err());
    }

    #[test]
    fn benchmark_addresses_cannot_be_reported_as_direct_server_latency() {
        use std::net::IpAddr;
        assert!(is_benchmark_address(IpAddr::from([198, 18, 0, 1])));
        assert!(is_benchmark_address(IpAddr::from([198, 19, 255, 255])));
        assert!(!is_benchmark_address(IpAddr::from([198, 20, 0, 1])));
    }

    #[test]
    fn live_inventory_places_each_member_under_its_group() {
        let inventory: cvt_core::mihomo::types::ProxiesResponse = serde_json::from_value(
            serde_json::json!({"proxies": {
                "GROUP": {"name":"GROUP", "type":"Selector", "all":["node-a", "DIRECT"], "now":"node-a"},
                "node-a": {"name":"node-a", "type":"Vless", "alive":true},
                "DIRECT": {"name":"DIRECT", "type":"Direct", "alive":true}
            }}),
        )
        .unwrap();
        let rows = node_rows(&inventory.proxies, Some("rule"), &[]);
        assert_eq!(rows.len(), 3);
        assert!(rows[0].is_group);
        assert_eq!(rows[0].members, 2);
        assert_eq!(rows[1].group.as_deref(), Some("GROUP"));
        assert!(rows[1].active);
        assert_eq!(rows[2].group.as_deref(), Some("GROUP"));
    }

    #[test]
    fn internal_adapters_are_hidden_and_global_only_appears_in_global_mode() {
        let inventory: cvt_core::mihomo::types::ProxiesResponse =
            serde_json::from_value(serde_json::json!({"proxies": {
                "GLOBAL": {"name":"GLOBAL", "type":"Selector", "all":["node-a"]},
                "GROUP": {"name":"GROUP", "type":"Selector", "all":["node-a"]},
                "node-a": {"name":"node-a", "type":"Vless"},
                "COMPATIBLE": {"name":"COMPATIBLE", "type":"Compatible"},
                "PASS": {"name":"PASS", "type":"Pass"},
                "PASS-RULE": {"name":"PASS-RULE", "type":"PassRule"},
                "REJECT-DROP": {"name":"REJECT-DROP", "type":"RejectDrop"}
            }}))
            .unwrap();
        let rule = node_rows(&inventory.proxies, Some("rule"), &[]);
        assert_eq!(rule.iter().filter(|row| row.is_group).count(), 1);
        assert!(
            rule.iter()
                .all(|row| row.name != "GLOBAL" && row.name != "PASS")
        );
        let global = node_rows(&inventory.proxies, Some("global"), &[]);
        assert_eq!(global.iter().filter(|row| row.is_group).count(), 2);
        assert!(global.iter().any(|row| row.name == "GLOBAL"));
        assert!(global.iter().all(|row| row.name != "COMPATIBLE"));
    }

    #[test]
    fn live_group_order_matches_generated_configuration() {
        let inventory: cvt_core::mihomo::types::ProxiesResponse =
            serde_json::from_value(serde_json::json!({"proxies": {
                "Alpha": {"name":"Alpha", "type":"Selector", "all":["node-a"]},
                "Zulu": {"name":"Zulu", "type":"Selector", "all":["node-a"]},
                "node-a": {"name":"node-a", "type":"Vless"}
            }}))
            .unwrap();
        let order = vec!["Zulu".to_owned(), "Alpha".to_owned()];
        let rows = node_rows(&inventory.proxies, Some("rule"), &order);
        assert_eq!(
            rows.iter()
                .filter(|row| row.is_group)
                .map(|row| row.name.as_str())
                .collect::<Vec<_>>(),
            ["Zulu", "Alpha"]
        );
    }

    #[test]
    fn selected_document_has_group_members_even_without_a_core() {
        let config = cvt_core::model::config::Config::from_yaml(
            "proxies:\n  - {name: node-a, type: vless}\nproxy-groups:\n  - {name: GROUP, type: select, proxies: [node-a, DIRECT]}\n",
        )
        .unwrap();
        let rows = config_node_rows(&config);
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[0].members, 2);
        assert_eq!(rows[1].group.as_deref(), Some("GROUP"));
        assert_eq!(rows[2].name, "DIRECT");
    }

    #[test]
    fn all_due_selects_only_remote_profiles_whose_interval_elapsed() {
        let now = 1_000_000;
        let mut due = PrfItem::remote("due", "due", "https://example.com/a");
        due.option.allow_auto_update = Some(true);
        due.option.update_interval = Some(60);
        due.updated = Some(now - 3_600);

        let mut fresh = PrfItem::remote("fresh", "fresh", "https://example.com/b");
        fresh.option.allow_auto_update = Some(true);
        fresh.option.update_interval = Some(60);
        fresh.updated = Some(now - 1);

        let mut disabled = PrfItem::remote("disabled", "disabled", "https://example.com/c");
        disabled.option.allow_auto_update = Some(false);
        disabled.option.update_interval = Some(60);

        let mut local = PrfItem::local("local", "local");
        local.option.allow_auto_update = Some(true);
        local.option.update_interval = Some(60);

        assert_eq!(
            due_remote_uids(&[fresh, disabled, local, due], now),
            vec!["due"]
        );
    }

    /// Every effect the interface can ask for is executed, and every arm names
    /// an effect that exists.
    ///
    /// Textual, and for the same reason the diagnostic scan is: the enum and
    /// the match are two lists in two crates that have to agree, and nothing in
    /// the type system makes them. An effect with no arm is a key that appears
    /// to work and does nothing — which is what a `match` with a `_ => {}` arm
    /// looks like from the outside.
    #[test]
    fn every_effect_is_executed_and_every_arm_names_an_effect() {
        let app = include_str!("../../../cvt-tui/src/app/protocol.rs");
        let defined: Vec<&str> = app
            .split("pub enum Effect {")
            .nth(1)
            .unwrap_or_default()
            .split("\n}")
            .next()
            .unwrap_or_default()
            .lines()
            .filter_map(|line| {
                let name = line
                    .trim()
                    .split(['(', '{', ','])
                    .next()
                    .unwrap_or_default()
                    .trim();
                (line.starts_with("    ")
                    && !line.starts_with("     ")
                    && name.chars().next().is_some_and(char::is_uppercase))
                .then_some(name)
            })
            .collect();
        assert!(
            defined.len() > 20,
            "the scan found only {} effects, so it is looking in the wrong place",
            defined.len()
        );

        let source = include_str!("../executor.rs");
        let handled: Vec<&str> = source
            .match_indices("Effect::")
            .filter_map(|(at, _)| {
                let rest = &source[at + "Effect::".len()..];
                let name: String = rest
                    .chars()
                    .take_while(char::is_ascii_alphanumeric)
                    .collect();
                (!name.is_empty()).then_some(Box::leak(name.into_boxed_str()) as &str)
            })
            .collect();

        let unexecuted: Vec<&&str> = defined
            .iter()
            .filter(|name| !handled.contains(*name))
            .collect();
        assert!(
            unexecuted.is_empty(),
            "these effects are defined and no arm executes them: {unexecuted:?}"
        );

        let unknown: Vec<&&str> = handled
            .iter()
            .filter(|name| !defined.contains(*name))
            .collect();
        assert!(
            unknown.is_empty(),
            "the executor names effects that do not exist: {unknown:?}"
        );
    }
}

mod subscription_workflows;
