/// Centralized desktop config: base URLs, feature flags, env-driven overrides.
/// Kept tiny on purpose — anything user-visible belongs in the UI layer.

// The server reads the OS from this to tell Mac and Windows installs apart in
// usage analytics.
#[cfg(target_os = "macos")]
pub const USER_AGENT: &str = concat!("Clipy-Desktop/", env!("CARGO_PKG_VERSION"), " (macos)");
#[cfg(target_os = "windows")]
pub const USER_AGENT: &str = concat!("Clipy-Desktop/", env!("CARGO_PKG_VERSION"), " (windows)");
#[cfg(not(any(target_os = "macos", target_os = "windows")))]
pub const USER_AGENT: &str = concat!("Clipy-Desktop/", env!("CARGO_PKG_VERSION"));

#[derive(Clone)]
pub struct Config {
    pub api_base: String,
    pub deep_link_scheme: &'static str,
}

impl Config {
    pub fn load() -> Self {
        // The local edition talks to no Clipy service. Code that still names
        // the field (the upload stub's unused arguments) gets an empty string.
        let api_base = String::new();

        Self {
            api_base,
            deep_link_scheme: crate::edition::DEEP_LINK_SCHEME,
        }
    }
}
