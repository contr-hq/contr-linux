//! Whether the client runs in Flatpak's sandbox, which changes what it can reach on the host.

use std::path::Path;

/// Flatpak writes this file into every sandbox it starts.
const FLATPAK_INFO: &str = "/.flatpak-info";

pub(crate) fn flatpak() -> bool {
    Path::new(FLATPAK_INFO).exists()
}
