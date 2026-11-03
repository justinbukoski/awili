//! The persisted preference, distinct from the appearance currently resolved by macOS.
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ColorMode {
    Light,
    System,
    /// Missing or unknown preference keeps the existing Dark appearance.
    #[default]
    #[serde(other)]
    Dark,
}

impl ColorMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Light => "light",
            Self::Dark => "dark",
            Self::System => "system",
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize, Serialize)]
pub struct AppearanceConfig {
    #[serde(default)]
    pub color_mode: ColorMode,
}
