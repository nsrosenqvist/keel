//! Compile-time constants used by the self-update flow.

pub const VERSION: &str = env!("CARGO_PKG_VERSION");
pub const TARGET: &str = env!("AMPELOS_TARGET");

pub const GITHUB_RELEASES_LATEST_API: &str =
    "https://api.github.com/repos/nsrosenqvist/ampelos/releases/latest";
pub const GITHUB_RELEASES_ALL_API: &str =
    "https://api.github.com/repos/nsrosenqvist/ampelos/releases";

pub fn release_asset_url(tag: &str, target: &str) -> String {
    format!(
        "https://github.com/nsrosenqvist/ampelos/releases/download/{tag}/ampelos-{target}.tar.gz"
    )
}

pub fn release_checksums_url(tag: &str) -> String {
    format!("https://github.com/nsrosenqvist/ampelos/releases/download/{tag}/SHA256SUMS")
}
