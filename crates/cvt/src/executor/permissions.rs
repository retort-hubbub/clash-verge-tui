//! Ask for capability grants before starting a privileged listener or TUN.

use super::Executor;
use cvt_core::model::config::Config;
use cvt_core::{Error, Result};
use cvt_tui::{Data, Effect, Event, EventSink};

impl Executor {
    /// Offer a local DNS alternative before capability authorization or deployment.
    pub(super) fn request_dns_resolution(&self, effect: &Effect, sink: &EventSink) -> bool {
        let result: Result<_> = self.with_service(|service| {
            let mut candidate = service.clone();
            if let Effect::SaveSettings { settings } = effect {
                if settings.core == service.settings().core {
                    return Ok(None);
                }
                settings.validate()?;
                candidate.set_settings(settings.clone());
            }
            let mut store = candidate.store()?;
            if let Effect::SwitchProfile { uid } = effect {
                store.set_current(uid)?;
            }
            if store.current().is_none() {
                return Ok(None);
            }
            let config = candidate.pipeline().generate(&store)?.config;
            if service.core_status().is_running() {
                let old =
                    Config::from_yaml(&service.paths().read(&service.paths().runtime_config())?)?;
                // A managed process's unchanged listener is not a foreign conflict.
                if old.get("dns").and_then(|dns| dns.get("listen"))
                    == config.get("dns").and_then(|dns| dns.get("listen"))
                {
                    return Ok(None);
                }
            }
            Ok(cvt_core::mihomo::listeners::dns_conflict(&config))
        });
        if let Ok(Some(conflict)) = result {
            Self::emit(
                sink,
                Event::Data(Data::DnsConflict {
                    conflict,
                    next: Box::new(effect.clone()),
                }),
            );
            true
        } else {
            // Existing permission/deployment validation reports generation errors.
            false
        }
    }

    /// Return true when an authorization dialog or failure replaced this effect.
    pub(super) fn request_permissions(&self, effect: &Effect, sink: &EventSink) -> bool {
        let required: Result<_> = self.with_service(|service| {
            let mut service = service.clone();
            if matches!(effect, Effect::StartCore) && service.core_status().is_running() {
                return Ok(None);
            }
            if let Effect::SaveSettings { settings } = effect {
                settings.validate()?;
                if !cfg!(target_os = "linux") && settings.core.tun_enabled == Some(true) {
                    return Err(Error::Unsupported(
                        "TUN authorization is only supported on Linux".to_owned(),
                    ));
                }
                service.set_settings(settings.clone());
            }
            // Do not require capabilities merely to save unrelated preferences.
            if matches!(effect, Effect::SaveSettings { .. })
                && service.settings().core.tun_enabled != Some(true)
            {
                return Ok(None);
            }
            let mut store = service.store()?;
            if let Effect::SwitchProfile { uid } = effect {
                store.set_current(uid)?;
            }
            if store.current().is_none() {
                if matches!(effect, Effect::SaveSettings { .. })
                    && service.settings().core.tun_enabled == Some(true)
                {
                    let binary = service.core_binary().ok_or_else(|| {
                        Error::Unsupported(
                            "install or select a Mihomo core before enabling TUN".to_owned(),
                        )
                    })?;
                    return Ok(Some((
                        binary,
                        "cap_net_admin,cap_net_raw,cap_net_bind_service".to_owned(),
                    )));
                }
                return Ok(None);
            }
            let runtime = service.paths().runtime_config();
            let config = if matches!(effect, Effect::RestartCore)
                && runtime.is_file()
                && service.settings().core.dns_listen.is_none()
            {
                Config::from_yaml(&service.paths().read(&runtime)?)?
            } else {
                service.pipeline().generate(&store)?.config
            };
            if config.tun_enabled()
                && cvt_core::mihomo::resolver::available()
                && (!matches!(effect, Effect::RestartCore)
                    || config
                        .get("tun")
                        .and_then(|tun| tun.get("device"))
                        .and_then(serde_json::Value::as_str)
                        == Some(cvt_core::mihomo::resolver::DEVICE))
            {
                cvt_core::mihomo::resolver::Target::from_config(&config)?;
            }
            let report = cvt_core::validate::check(&config);
            if !report.is_ok() {
                return Err(Error::Validation {
                    problems: report
                        .errors_iter()
                        .map(|finding| finding.message.clone())
                        .collect(),
                });
            }
            // Restart resource checks belong after shutdown. Authorization
            // must not reintroduce the pre-stop listener/TUN conflict check.
            let restarting = service.core_status().is_running()
                && (matches!(effect, Effect::RestartCore)
                    || matches!(
                        effect,
                        Effect::ApplyConfig {
                            mode: cvt_core::ReloadMode::Restart
                        }
                    )
                    || (matches!(effect, Effect::SwitchProfile { .. })
                        && !service.settings().update.prefer_hot_reload));
            if !restarting {
                service.validate_environment(&config)?;
            }
            let capabilities = crate::tun::required_capabilities(&config);
            if capabilities.is_empty() {
                return Ok(None);
            }
            let binary = service.core_binary().ok_or_else(|| {
                Error::Unsupported(
                    "install or select a Mihomo core before authorization".to_owned(),
                )
            })?;
            Ok(Some((binary, capabilities)))
        });
        match required {
            Ok(Some((binary, capabilities))) => {
                match crate::tun::has_capabilities(&binary, &capabilities) {
                    Ok(true)
                        if !capabilities.split(',').any(|cap| cap == "cap_net_admin")
                            || cvt_core::mihomo::resolver::authorized() =>
                    {
                        false
                    }
                    Ok(_) => {
                        Self::emit(
                            sink,
                            Event::Data(Data::CoreAuthorization {
                                binary,
                                capabilities,
                                next: Box::new(effect.clone()),
                            }),
                        );
                        true
                    }
                    Err(error) => {
                        Self::emit(sink, Event::Failed(error.to_string()));
                        true
                    }
                }
            }
            Ok(None) => false,
            Err(error) => {
                Self::emit(sink, Event::Failed(error.to_string()));
                true
            }
        }
    }
}
