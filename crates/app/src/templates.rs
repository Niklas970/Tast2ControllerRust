//! Built-in mapping templates: ready-made profiles that can be added to a config.

use anyhow::{Context, Result};
use serde::Deserialize;

use crate::config::{Config, Profile, DEFAULT_CONFIG};

pub struct Template {
    /// Shown on the button.
    pub label: &'static str,
    /// One sentence on what it contains.
    pub about: &'static str,
    toml: &'static str,
}

/// All templates. Each one only refers to its own profiles (profile switches stay inside it).
pub const TEMPLATES: &[Template] = &[
    Template {
        label: "Shooter",
        about: "WASD, Maus als rechter Stick, Linksklick feuern, Rechtsklick zielen; F2 wechselt ins Menü-Profil.",
        toml: DEFAULT_CONFIG,
    },
    Template {
        label: "GTA",
        about: "Zu Fuß („gta“) und im Fahrzeug („gta_fahren“, W/S = Gas/Bremse), Wechsel mit F2 / F1. Angelehnt an GTA V, im Spiel prüfen.",
        toml: include_str!("../templates/gta.toml"),
    },
    Template {
        label: "Menü",
        about: "Pfeiltasten/WASD als Steuerkreuz, Enter und Linksklick bestätigen, Rücktaste und Rechtsklick zurück.",
        toml: include_str!("../templates/menu.toml"),
    },
];

#[derive(Deserialize)]
struct Profiles {
    #[serde(default, rename = "profile")]
    profiles: Vec<Profile>,
}

impl Template {
    pub fn profiles(&self) -> Result<Vec<Profile>> {
        // the shooter template is the default config: only its profiles are wanted
        let doc: toml::Table = toml::from_str(self.toml).context("template")?;
        let profiles = doc
            .get("profile")
            .cloned()
            .unwrap_or(toml::Value::Array(vec![]));
        let mut t = toml::Table::new();
        t.insert("profile".into(), profiles);
        let p: Profiles = t.try_into().context("template profiles")?;
        Ok(p.profiles)
    }
}

/// Adds the template's profiles to `cfg`; a profile of the same name is replaced.
/// Returns the names of the profiles that were replaced.
pub fn apply(cfg: &mut Config, template: &Template) -> Result<Vec<String>> {
    let mut replaced = Vec::new();
    for p in template.profiles()? {
        match cfg
            .profiles
            .iter_mut()
            .find(|q| q.name.eq_ignore_ascii_case(&p.name))
        {
            Some(q) => {
                replaced.push(q.name.clone());
                *q = p;
            }
            None => cfg.profiles.push(p),
        }
    }
    Ok(replaced)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::input::mapper::Mapper;

    #[test]
    fn every_template_maps_on_its_own_and_together() {
        let mut all = Config::default();
        for t in TEMPLATES {
            let profiles = t.profiles().unwrap();
            assert!(!profiles.is_empty(), "{}", t.label);
            let mut cfg = Config::default();
            apply(&mut cfg, t).unwrap();
            Mapper::new(&cfg).unwrap_or_else(|e| panic!("{}: {e:#}", t.label));
            apply(&mut all, t).unwrap();
        }
        Mapper::new(&all).unwrap();
    }

    #[test]
    fn applying_twice_replaces() {
        let mut cfg = Config::default_config();
        let n = cfg.profiles.len();
        let replaced = apply(&mut cfg, &TEMPLATES[0]).unwrap();
        assert_eq!(cfg.profiles.len(), n);
        assert_eq!(replaced.len(), n);
    }
}
