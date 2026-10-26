// SPDX-License-Identifier: AGPL-3.0-or-later

/// An API key. `Debug` and `Display` print `<secret>`, so a key inside a struct that
/// gets logged or put in a panic message never shows (MODEL_ROUTING §11).
#[derive(Clone, PartialEq, Eq)]
pub struct Secret(String);

impl Secret {
    /// Wraps a key read from the environment or keychain.
    pub fn new(value: String) -> Secret {
        Secret(value)
    }

    /// The key itself, for the one place that sends it: the provider's auth header.
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Debug for Secret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("<secret>")
    }
}

impl std::fmt::Display for Secret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("<secret>")
    }
}
