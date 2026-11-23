//! Credentials produced by console registration (the rest of the app config lives in the app crate).

/// What registration produced and every session needs.
#[derive(Clone)]
pub struct Credentials {
    /// Parsed `PS5-RegistKey` bytes, zero padded.
    pub regist_key: [u8; 16],
    /// `RP-Key`.
    pub morning: [u8; 16],
}

impl Credentials {
    pub fn regist_key_trimmed(&self) -> &[u8] {
        let len = self.regist_key.iter().position(|&b| b == 0).unwrap_or(16);
        &self.regist_key[..len]
    }
}
