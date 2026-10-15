//! Community and project links (shown in Help ▸, the About dialog, the start screen and the
//! header's Discord button).

pub const APP_NAME: &str = "EffectCraft";
pub const APP_SLUG: &str = "effectcraft";
pub const DISCORD: &str = "https://discord.gg/artcraft";
pub const WEBSITE: &str = "https://getartcraft.com";
pub const APP_PAGE: &str = "https://getartcraft.com/apps/effectcraft";
pub const GITHUB: &str = "https://github.com/storytold/effectcraft";
pub const ISSUES: &str = "https://github.com/storytold/effectcraft/issues";
/// Where this build's updates are published (the fork's GitHub Releases; `update.rs`).
pub const RELEASES: &str = "https://github.com/abdullah-bcl/effectcraft/releases/latest";
/// The newest release's signed manifest (`latest.json`, `latest.json.sig` beside it).
pub const UPDATE_MANIFEST: &str = "https://github.com/abdullah-bcl/effectcraft/releases/latest/download/latest.json";

/// Sibling apps of the ArtCraft family: (name, what it is, GitHub repo, app page).
pub const SIBLINGS: &[(&str, &str)] = &[
    ("PhotoCraft", "photocraft"),
    ("VectorCraft", "vectorcraft"),
    ("FilmCraft", "filmcraft"),
    ("LightCraft", "lightcraft"),
    ("PdfCraft", "pdfcraft"),
    ("DesignCraft", "designcraft"),
];

pub fn github_of(slug: &str) -> String {
    format!("https://github.com/storytold/{slug}")
}
pub fn page_of(slug: &str) -> String {
    format!("https://getartcraft.com/apps/{slug}")
}
