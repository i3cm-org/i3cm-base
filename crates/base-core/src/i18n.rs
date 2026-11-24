//! Display names and descriptions per type, rendered from Fluent templates.
//! Message ids: `<ns>-<Type>-name` and `<ns>-<Type>-description`; variables are the
//! attributes of the type part.

use crate::catalog::Model;
use crate::error::{BaseError, Result};
use crate::values::display;
use fluent_bundle::concurrent::FluentBundle;
use fluent_bundle::{FluentArgs, FluentResource};
use serde_json::{Map, Value};
use std::collections::BTreeMap;
use unic_langid::LanguageIdentifier;

pub const DEFAULT_LANG: &str = "en";

pub fn parse_check(module: &str, lang: &str, src: &str) -> Result<()> {
    FluentResource::try_new(src.to_string()).map(|_| ()).map_err(|(_, errs)| {
        BaseError::invalid("i18n.parse", "Fluent file cannot be parsed").with("module", module).with("lang", lang).with("errors", format!("{errs:?}"))
    })?;
    lang.parse::<LanguageIdentifier>()
        .map(|_| ())
        .map_err(|_| BaseError::invalid("i18n.lang", "unknown language tag").with("module", module).with("lang", lang))
}

pub struct I18n {
    bundles: BTreeMap<String, FluentBundle<FluentResource>>,
}

impl I18n {
    pub fn new(model: &Model) -> Result<I18n> {
        let mut bundles: BTreeMap<String, FluentBundle<FluentResource>> = BTreeMap::new();
        for m in &model.catalog.modules {
            for (lang, src) in &m.i18n {
                let id: LanguageIdentifier = lang.parse().map_err(|_| BaseError::invalid("i18n.lang", "unknown language tag").with("lang", lang.clone()))?;
                let b = bundles.entry(lang.clone()).or_insert_with(|| {
                    let mut b = FluentBundle::new_concurrent(vec![id]);
                    b.set_use_isolating(false);
                    b
                });
                let res = FluentResource::try_new(src.clone()).map_err(|_| BaseError::invalid("i18n.parse", "Fluent file cannot be parsed"))?;
                b.add_resource(res).map_err(|e| BaseError::invalid("i18n.duplicate", "duplicate Fluent message").with("errors", format!("{e:?}")))?;
            }
        }
        Ok(I18n { bundles })
    }

    /// Pick the best language from an Accept-Language header.
    pub fn pick(&self, accept: Option<&str>) -> String {
        if let Some(h) = accept {
            for part in h.split(',') {
                let tag = part.split(';').next().unwrap_or("").trim().to_lowercase();
                let primary = tag.split('-').next().unwrap_or("");
                if self.bundles.contains_key(&tag) {
                    return tag;
                }
                if self.bundles.contains_key(primary) {
                    return primary.to_string();
                }
            }
        }
        if self.bundles.contains_key(DEFAULT_LANG) {
            DEFAULT_LANG.to_string()
        } else {
            self.bundles.keys().next().cloned().unwrap_or_default()
        }
    }

    fn render(&self, lang: &str, id: &str, attrs: &Map<String, Value>) -> Option<String> {
        let b = self.bundles.get(lang)?;
        let msg = b.get_message(id)?;
        let pattern = msg.value()?;
        let mut args = FluentArgs::new();
        for (k, v) in attrs {
            args.set(k.clone(), display(v));
        }
        let mut errs = vec![];
        let s = b.format_pattern(pattern, Some(&args), &mut errs);
        Some(s.split_whitespace().collect::<Vec<_>>().join(" "))
    }

    /// Display name and description of a type part within an aspect; the aspect's texts
    /// (`<aspect>--<type>-name`) override the global ones.
    pub fn names_in(&self, aspect: Option<&str>, lang: &str, type_name: &str, attrs: &Map<String, Value>) -> (String, String) {
        let (mut n, mut d) = self.names(lang, type_name, attrs);
        if let Some(a) = aspect {
            let base = format!("{}--{}", a.replace(':', "-"), type_name.replace(':', "-"));
            if let Some(x) = self.render(lang, &format!("{base}-name"), attrs) {
                n = x;
            }
            if let Some(x) = self.render(lang, &format!("{base}-description"), attrs) {
                d = x;
            }
        }
        (n, d)
    }

    /// Label of an attribute, folder, tab, column or function within an aspect.
    pub fn label(&self, aspect: &str, lang: &str, label: &str) -> String {
        let id = format!("{}--label-{}", aspect.replace(':', "-"), label);
        self.render(lang, &id, &Map::new()).unwrap_or_else(|| label.to_string())
    }

    /// (display name, description) of a type part.
    pub fn names(&self, lang: &str, type_name: &str, attrs: &Map<String, Value>) -> (String, String) {
        let base = type_name.replace(':', "-");
        let name = self.render(lang, &format!("{base}-name"), attrs).unwrap_or_else(|| type_name.to_string());
        let desc = self.render(lang, &format!("{base}-description"), attrs).unwrap_or_default();
        (name, desc)
    }
}
