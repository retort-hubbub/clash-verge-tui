//! Inventory adapter for TUI effects.

use cvt_core::mihomo::types::ProxyView;
use cvt_tui::row::NodeRow;
use std::collections::{BTreeMap, HashMap, HashSet};

/// Flatten selectable groups and their members into rows.
///
/// The core also reports internal adapters and an always-present GLOBAL group.
/// They have no useful action in rule/direct mode and standalone adapters
/// cannot be selected, so neither belongs in the interactive list.
pub(super) fn node_rows(
    proxies: &BTreeMap<String, ProxyView>,
    mode: Option<&str>,
    group_order: &[String],
) -> Vec<NodeRow> {
    let mut rows = Vec::new();
    let rank: HashMap<&str, usize> = group_order
        .iter()
        .enumerate()
        .map(|(index, name)| (name.as_str(), index))
        .collect();
    let mut groups: Vec<_> = proxies
        .values()
        .filter(|view| view.is_group() && (view.name != "GLOBAL" || mode == Some("global")))
        .collect();
    groups.sort_by_key(|view| rank.get(view.name.as_str()).copied().unwrap_or(usize::MAX));
    for view in groups {
        rows.push(NodeRow::from_group(view));
        for name in view.members() {
            if let Some(member) = proxies.get(name) {
                rows.push(NodeRow::from_member(
                    member,
                    &view.name,
                    view.now.as_deref() == Some(name.as_str())
                        || view.fixed.as_deref() == Some(name.as_str()),
                ));
            }
        }
    }
    rows
}

/// Show the selected document while Mihomo is stopped. Live health and
/// provider-expanded membership become available once the core starts.
pub(super) fn config_node_rows(config: &cvt_core::model::config::Config) -> Vec<NodeRow> {
    use cvt_core::model::proxy::GroupKind;

    let proxies = config.proxies();
    let groups = config.proxy_groups();
    let mut rows = Vec::new();
    let mut referenced = HashSet::new();
    for group in &groups {
        let kind = match GroupKind::from_wire(&group.kind) {
            GroupKind::Select => "Selector",
            GroupKind::UrlTest => "URLTest",
            GroupKind::Fallback => "Fallback",
            GroupKind::LoadBalance => "LoadBalance",
            GroupKind::Unknown => group.kind.as_str(),
        };
        rows.push(NodeRow {
            name: group.name.clone(),
            kind: kind.to_owned(),
            group: None,
            delay: None,
            alive: false,
            active: false,
            is_group: true,
            is_proxy: false,
            members: group.proxies.len(),
            selectable: false,
        });
        for name in &group.proxies {
            referenced.insert(name.as_str());
            let kind = proxies
                .iter()
                .find(|proxy| proxy.name == *name)
                .map_or_else(
                    || {
                        groups
                            .iter()
                            .find(|candidate| candidate.name == *name)
                            .map_or("builtin", |candidate| candidate.kind.as_str())
                    },
                    |proxy| proxy.kind.as_str(),
                );
            rows.push(NodeRow {
                name: name.clone(),
                kind: kind.to_owned(),
                group: Some(group.name.clone()),
                delay: None,
                alive: false,
                active: false,
                is_group: false,
                is_proxy: proxies.iter().any(|proxy| {
                    proxy.name == *name && proxy.server.is_some() && proxy.port.is_some()
                }),
                members: 0,
                selectable: false,
            });
        }
    }
    for proxy in &proxies {
        if !referenced.contains(proxy.name.as_str()) {
            rows.push(NodeRow::from_config_proxy(proxy));
        }
    }
    rows
}
