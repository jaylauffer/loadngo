//! Locale-keyed string catalogs — shared engine infrastructure for every
//! game built on `loadngo`, not a per-game solution. See
//! `docs/LOCALIZATION.md` for the roadmap this crate is Phase 1 of: this
//! crate is the catalog format and lookup API only. Locale
//! auto-detection, font glyph-coverage wiring, and migrating any game's
//! existing hardcoded strings onto this are separate, later phases.
//!
//! Mirrors `sng-roguelite/crates/game-data`'s established content-catalog
//! pattern (`parse_*_ron` + `validate_*`, `schema_version`/`revision`
//! header fields, a diagnostics-collecting validator) rather than
//! introducing a new convention for content that happens to be strings.

use std::collections::HashMap;
use std::fmt;

/// Bump when `LocaleCatalogDefinition`'s shape changes in a way old
/// catalog files wouldn't satisfy.
pub const CURRENT_LOCALE_CATALOG_SCHEMA_VERSION: u32 = 1;

/// A locale catalog exactly as authored in RON — one file per locale,
/// e.g. `assets/localization/en.ron`. Keys are opaque lookup strings
/// (e.g. `"title.press_to_start"`), not the English text itself, so that
/// every locale's catalog — including the base/English one — has the
/// same shape.
#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize)]
pub struct LocaleCatalogDefinition {
    pub schema_version: u32,
    pub locale: String,
    pub revision: u32,
    pub strings: HashMap<String, String>,
}

/// A [`LocaleCatalogDefinition`] that has passed [`validate_locale_catalog`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidatedLocaleCatalog {
    source_name: String,
    definition: LocaleCatalogDefinition,
}

impl ValidatedLocaleCatalog {
    #[must_use]
    pub fn source_name(&self) -> &str {
        &self.source_name
    }

    #[must_use]
    pub fn locale(&self) -> &str {
        &self.definition.locale
    }

    #[must_use]
    pub fn revision(&self) -> u32 {
        self.definition.revision
    }

    #[must_use]
    pub fn get(&self, key: &str) -> Option<&str> {
        self.definition.strings.get(key).map(String::as_str)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LocalizationError {
    Parse {
        source_name: String,
        message: String,
    },
    Validation {
        source_name: String,
        diagnostics: Vec<String>,
    },
}

impl LocalizationError {
    #[must_use]
    pub fn source_name(&self) -> &str {
        match self {
            Self::Parse { source_name, .. } | Self::Validation { source_name, .. } => source_name,
        }
    }

    #[must_use]
    pub fn diagnostics(&self) -> &[String] {
        match self {
            Self::Parse { .. } => &[],
            Self::Validation { diagnostics, .. } => diagnostics,
        }
    }
}

impl fmt::Display for LocalizationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Parse {
                source_name,
                message,
            } => write!(
                formatter,
                "{source_name}: failed to parse locale catalog: {message}"
            ),
            Self::Validation {
                source_name,
                diagnostics,
            } => write!(
                formatter,
                "{source_name}: locale catalog is invalid: {}",
                diagnostics.join("; ")
            ),
        }
    }
}

impl std::error::Error for LocalizationError {}

/// Parses and validates a locale catalog from RON source in one step.
///
/// # Errors
///
/// Returns [`LocalizationError::Parse`] if `source` isn't well-formed
/// RON, or [`LocalizationError::Validation`] with every diagnostic found
/// (not just the first) if it parses but fails validation.
pub fn parse_locale_catalog_ron(
    source_name: impl Into<String>,
    source: &str,
) -> Result<ValidatedLocaleCatalog, LocalizationError> {
    let source_name = source_name.into();
    let definition = ron::from_str(source).map_err(|error| LocalizationError::Parse {
        source_name: source_name.clone(),
        message: error.to_string(),
    })?;
    validate_locale_catalog(source_name, definition)
}

/// Validates an already-decoded locale catalog.
///
/// # Errors
///
/// Returns [`LocalizationError::Validation`] with every diagnostic found.
pub fn validate_locale_catalog(
    source_name: impl Into<String>,
    definition: LocaleCatalogDefinition,
) -> Result<ValidatedLocaleCatalog, LocalizationError> {
    let source_name = source_name.into();
    let mut diagnostics = Vec::new();

    if definition.schema_version != CURRENT_LOCALE_CATALOG_SCHEMA_VERSION {
        diagnostics.push(format!(
            "locale catalog schema_version {} is unsupported; expected {CURRENT_LOCALE_CATALOG_SCHEMA_VERSION}",
            definition.schema_version
        ));
    }
    if definition.locale.trim().is_empty() {
        diagnostics.push("locale catalog locale must not be empty".to_string());
    }
    if definition.revision == 0 {
        diagnostics.push("locale catalog revision must be positive".to_string());
    }
    if definition.strings.is_empty() {
        diagnostics.push("locale catalog must contain at least one string".to_string());
    }
    for (key, value) in &definition.strings {
        if key.trim().is_empty() {
            diagnostics.push("locale catalog contains an empty string key".to_string());
        }
        if value.is_empty() {
            diagnostics.push(format!(
                "locale catalog string \"{key}\" has an empty value"
            ));
        }
    }

    if diagnostics.is_empty() {
        Ok(ValidatedLocaleCatalog {
            source_name,
            definition,
        })
    } else {
        Err(LocalizationError::Validation {
            source_name,
            diagnostics,
        })
    }
}

/// The single thing a game's UI code is expected to consult for any
/// player-facing text — mirrors `sng-roguelite`'s `FormFactor` in that
/// respect (one obvious fact to check, not something to re-derive ad
/// hoc).
///
/// There is deliberately no committed English catalog anywhere. English
/// is always the caller-supplied `default` passed to [`Localizer::t`] —
/// for hand-authored UI chrome, that's the literal string being migrated
/// (`localizer.t("title.press_to_start", "Press Space to start")`); for
/// catalog content (an item's description, say), that's the field
/// already authored in that game's own RON content file
/// (`localizer.t(&format!("item.{}.description", item.id),
/// &item.description)`). This keeps every existing English source of
/// truth exactly where it already lives and exactly as readable as it
/// already is — a game's `items.ron` never has to grow translation keys
/// of its own — and it means a locale catalog only ever needs to contain
/// *actual translations*: an untranslated key simply falls through to
/// the always-correct English default, so a partial translation is never
/// broken, just incomplete. [`Localizer::t`] never panics.
pub struct Localizer {
    locale: String,
    catalog: Option<ValidatedLocaleCatalog>,
}

impl Localizer {
    /// `locale` is the resolved display locale (see `loadngo-host-desktop`'s
    /// `system_locale()`) even when `catalog` is `None` — e.g. the
    /// detected locale is `"en"` itself (nothing to look up, defaults are
    /// already English), or no catalog file exists yet for that locale.
    #[must_use]
    pub fn new(locale: impl Into<String>, catalog: Option<ValidatedLocaleCatalog>) -> Self {
        Self {
            locale: locale.into(),
            catalog,
        }
    }

    #[must_use]
    pub fn locale(&self) -> &str {
        &self.locale
    }

    /// Looks `key` up in the current locale's catalog (if one is loaded),
    /// returning `default` on any miss — an untranslated key, or no
    /// catalog loaded at all. Never panics.
    #[must_use]
    pub fn t<'a>(&'a self, key: &str, default: &'a str) -> &'a str {
        self.catalog
            .as_ref()
            .and_then(|catalog| catalog.get(key))
            .unwrap_or(default)
    }
}

/// Derives a stable lookup key from content that has no naturally stable
/// identifier of its own (unlike an item or encounter, which already has
/// an authored `id`). FNV-1a 64-bit, deliberately the same algorithm and
/// output shape as `sng-rusty`'s `stable_line_id` (`src/bin/
/// export_lines.rs`) — same reasoning applies here: deterministic across
/// machines, and the key changes if `text` changes, so a stale
/// translation (or a stale voiceover clip, in `sng-rusty`'s case) is
/// detectable rather than silently left behind. `context` disambiguates
/// otherwise-identical text (e.g. a speaker name, or a UI area) the same
/// way `sng-rusty` mixes in the speaker before hashing.
#[must_use]
pub fn stable_key_from_text(context: &str, text: &str) -> String {
    const OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
    const PRIME: u64 = 0x0000_0100_0000_01b3;
    let mut hash = OFFSET;
    for byte in context
        .as_bytes()
        .iter()
        .chain(b"|".iter())
        .chain(text.as_bytes())
    {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(PRIME);
    }
    format!("{hash:016x}")
}

#[cfg(test)]
mod tests {
    use super::{
        parse_locale_catalog_ron, validate_locale_catalog, LocaleCatalogDefinition,
        LocalizationError, Localizer,
    };
    use std::collections::HashMap;

    fn sample_ron(
        locale: &str,
        schema_version: u32,
        revision: u32,
        strings: &[(&str, &str)],
    ) -> String {
        let entries: String = strings
            .iter()
            .map(|(key, value)| format!("\"{key}\": \"{value}\","))
            .collect::<Vec<_>>()
            .join(" ");
        format!(
            "(schema_version: {schema_version}, locale: \"{locale}\", revision: {revision}, strings: {{{entries}}})"
        )
    }

    #[test]
    fn parses_and_validates_a_well_formed_catalog() {
        let source = sample_ron(
            "en",
            1,
            1,
            &[("title.press_to_start", "Press Space to start")],
        );
        let catalog = parse_locale_catalog_ron("en.ron", &source).unwrap();

        assert_eq!(catalog.locale(), "en");
        assert_eq!(catalog.revision(), 1);
        assert_eq!(
            catalog.get("title.press_to_start"),
            Some("Press Space to start")
        );
        assert_eq!(catalog.get("missing.key"), None);
    }

    #[test]
    fn rejects_wrong_schema_version() {
        let source = sample_ron("en", 999, 1, &[("k", "v")]);
        let error = parse_locale_catalog_ron("en.ron", &source).unwrap_err();
        assert!(matches!(error, LocalizationError::Validation { .. }));
        assert!(error
            .diagnostics()
            .iter()
            .any(|d| d.contains("schema_version")));
    }

    #[test]
    fn rejects_zero_revision() {
        let source = sample_ron("en", 1, 0, &[("k", "v")]);
        let error = parse_locale_catalog_ron("en.ron", &source).unwrap_err();
        assert!(error.diagnostics().iter().any(|d| d.contains("revision")));
    }

    #[test]
    fn rejects_empty_locale() {
        let source = sample_ron("", 1, 1, &[("k", "v")]);
        let error = parse_locale_catalog_ron("en.ron", &source).unwrap_err();
        assert!(error.diagnostics().iter().any(|d| d.contains("locale")));
    }

    #[test]
    fn rejects_catalog_with_no_strings() {
        let definition = LocaleCatalogDefinition {
            schema_version: 1,
            locale: "en".to_string(),
            revision: 1,
            strings: HashMap::new(),
        };
        let error = validate_locale_catalog("en.ron", definition).unwrap_err();
        assert!(error
            .diagnostics()
            .iter()
            .any(|d| d.contains("at least one string")));
    }

    #[test]
    fn rejects_empty_string_value() {
        let source = sample_ron("en", 1, 1, &[("title.press_to_start", "")]);
        let error = parse_locale_catalog_ron("en.ron", &source).unwrap_err();
        assert!(error
            .diagnostics()
            .iter()
            .any(|d| d.contains("empty value")));
    }

    #[test]
    fn parse_error_reports_bad_ron_as_parse_not_validation() {
        let error = parse_locale_catalog_ron("en.ron", "not valid ron {{{").unwrap_err();
        assert!(matches!(error, LocalizationError::Parse { .. }));
        assert_eq!(error.source_name(), "en.ron");
    }

    #[test]
    fn localizer_prefers_a_real_translation_over_the_default() {
        let catalog = parse_locale_catalog_ron(
            "de.ron",
            &sample_ron("de", 1, 1, &[("title.press_to_start", "Leertaste drücken")]),
        )
        .unwrap();

        let localizer = Localizer::new("de", Some(catalog));
        assert_eq!(localizer.locale(), "de");
        assert_eq!(
            localizer.t("title.press_to_start", "Press Space to start"),
            "Leertaste drücken"
        );
    }

    #[test]
    fn localizer_falls_back_to_the_default_when_the_key_is_untranslated() {
        let catalog = parse_locale_catalog_ron(
            "de.ron",
            &sample_ron("de", 1, 1, &[("title.tap_to_start", "Zum Starten tippen")]),
        )
        .unwrap();

        // A partial translation is never broken, just incomplete: an
        // untranslated key falls straight through to the caller's default.
        let localizer = Localizer::new("de", Some(catalog));
        assert_eq!(
            localizer.t("title.press_to_start", "Press Space to start"),
            "Press Space to start"
        );
    }

    #[test]
    fn localizer_falls_back_to_the_default_with_no_catalog_loaded_at_all() {
        // The common case for the base/English locale: no catalog file
        // needs to exist at all, since English is always the default.
        let localizer = Localizer::new("en", None);
        assert_eq!(localizer.locale(), "en");
        assert_eq!(
            localizer.t("title.press_to_start", "Press Space to start"),
            "Press Space to start"
        );
    }

    #[test]
    fn stable_key_from_text_is_deterministic_and_context_sensitive() {
        assert_eq!(
            super::stable_key_from_text("item.forked_signal", "+1 shot and spread"),
            super::stable_key_from_text("item.forked_signal", "+1 shot and spread"),
        );
        // Same text, different context -> different key, matching
        // sng-rusty's speaker-disambiguation reasoning.
        assert_ne!(
            super::stable_key_from_text("item.forked_signal", "+1 shot and spread"),
            super::stable_key_from_text("item.dense_pulse", "+1 shot and spread"),
        );
        // Any change to the text itself -> a different key, so a stale
        // translation is detectable rather than silently left behind.
        assert_ne!(
            super::stable_key_from_text("item.forked_signal", "+1 shot and spread"),
            super::stable_key_from_text("item.forked_signal", "+2 shot and spread"),
        );
    }
}
