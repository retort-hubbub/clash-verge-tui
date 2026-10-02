//! Ask for capability grants before starting a privileged listener or TUN.

use super::Executor;
use cvt_core::model::config::Config;
use cvt_core::{Error, Result};
use cvt_tui::{Data, Effect, Event, EventSink};

impl Executor {
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
            if service.store()?.current().is_none() {
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
            let config =
                if matches!(effect, Effect::StartCore | Effect::RestartCore) && runtime.is_file() {
                    Config::from_yaml(&service.paths().read(&runtime)?)?
                } else {
                    service.generate()?.config
                };
            let report = cvt_core::validate::check(&config);
            if !report.is_ok() {
                return Err(Error::Validation {
                    problems: report
                        .errors_iter()
                        .map(|finding| finding.message.clone())
                        .collect(),
                });
            }
            service.validate_environment(&config)?;
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
                    Ok(true) => false,
                    Ok(false) => {
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
