//! Overrides owned by one base profile, independent of the global chain.

use crate::enhance::overlay::Overlay;
use crate::error::{Error, Result};
use crate::profile::item::{PrfItem, ProfileType};
use crate::profile::store::ProfileStore;

/// Index metadata identifying the base that owns a private patch.
pub const BASE_SCOPE: &str = "cvt-base";

impl PrfItem {
    /// The base profile this patch belongs to, or none for a global patch.
    #[must_use]
    pub fn base_scope(&self) -> Option<&str> {
        self.option
            .other
            .get(BASE_SCOPE)
            .and_then(serde_json::Value::as_str)
    }
}

impl ProfileStore {
    /// Find or create the override belonging to a base profile.
    ///
    /// # Errors
    /// Rejects missing or non-base profiles and propagates persistence failures.
    pub fn ensure_override(&mut self, base_uid: &str) -> Result<PrfItem> {
        let base = self.get(base_uid).ok_or_else(|| Error::ProfileNotFound {
            uid: base_uid.to_owned(),
        })?;
        if !base.kind.is_base() {
            return Err(Error::invalid("override", "select a base profile first"));
        }
        if let Some(item) = self
            .items()
            .iter()
            .find(|item| item.kind == ProfileType::Override && item.base_scope() == Some(base_uid))
        {
            return Ok(item.clone());
        }
        let mut item = PrfItem::patch(
            self.generate_uid(ProfileType::Override),
            format!("{} · override", base.label()),
            ProfileType::Override,
        );
        item.option.other.insert(
            BASE_SCOPE.to_owned(),
            serde_json::Value::String(base_uid.to_owned()),
        );
        let uid = self.add(item);
        let item = self
            .get(&uid)
            .cloned()
            .ok_or_else(|| Error::ProfileNotFound { uid })?;
        self.write_document(
            &item,
            &serde_norway::to_string(&Overlay::default())
                .map_err(|e| Error::serialize("override", e))?,
        )?;
        self.save()?;
        Ok(item)
    }

    /// Prepend a rule to a base's private override without modifying its source.
    ///
    /// # Errors
    /// Propagates malformed existing overrides or persistence failures.
    pub fn prepend_profile_rule(&mut self, base_uid: &str, rule: &str) -> Result<()> {
        let item = self.ensure_override(base_uid)?;
        let text = self.read_document(&item)?;
        let mut overlay: Overlay =
            serde_norway::from_str(&text).map_err(|e| Error::invalid("override", e.to_string()))?;
        let rules = overlay.prepend.entry("rules".to_owned()).or_default();
        let value = serde_json::Value::String(rule.to_owned());
        if !rules.contains(&value) {
            rules.insert(0, value);
        }
        let text =
            serde_norway::to_string(&overlay).map_err(|e| Error::serialize("override", e))?;
        self.write_document(&item, &text)
    }
}
