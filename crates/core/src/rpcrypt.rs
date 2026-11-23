//! "RP crypt": the AES-128-CFB layer used by registration, the ctrl channel and the launch spec.
//! PS5 (RP-Version 1.0) only. Port of chiaki-ng `lib/src/rpcrypt.c`.

use aes::cipher::{AsyncStreamCipher, KeyIvInit};
use hmac::{Hmac, Mac};
use sha2::Sha256;

use crate::rpcrypt_tables::{AEROPAUSE_KEYS_1_PS5, KEYS_A_PS5, KEYS_B_PS5, REGIST_KEYS_0_PS5};

pub const KEY_SIZE: usize = 16;

const HMAC_KEY_PS5: [u8; 16] = [
    0x46, 0x46, 0x87, 0xb3, 0x49, 0xca, 0x8c, 0xe8, 0x59, 0xc5, 0x27, 0x0f, 0x5d, 0x7a, 0x69, 0xd6,
];

type Aes128CfbEnc = cfb_mode::Encryptor<aes::Aes128>;
type Aes128CfbDec = cfb_mode::Decryptor<aes::Aes128>;

#[derive(Clone)]
pub struct RpCrypt {
    bright: [u8; KEY_SIZE],
    ambassador: [u8; KEY_SIZE],
}

impl RpCrypt {
    /// Crypt for an authenticated session, from the session nonce and the registered "morning" (RP-Key).
    pub fn new_auth(nonce: &[u8; KEY_SIZE], morning: &[u8; KEY_SIZE]) -> Self {
        let mut bright = [0u8; KEY_SIZE];
        let mut ambassador = [0u8; KEY_SIZE];

        let key_a = &KEYS_A_PS5[(nonce[0] >> 3) as usize * 0x70..];
        for i in 0..KEY_SIZE {
            ambassador[i] = nonce[i].wrapping_sub(0x2d).wrapping_sub(i as u8) ^ key_a[i];
        }
        let key_b = &KEYS_B_PS5[(nonce[7] >> 3) as usize * 0x70..];
        for i in 0..KEY_SIZE {
            bright[i] = morning[i].wrapping_add(0x18).wrapping_add(i as u8) ^ nonce[i] ^ key_b[i];
        }
        Self { bright, ambassador }
    }

    /// Crypt for registration with a PIN shown on the console.
    pub fn new_regist(ambassador: &[u8; KEY_SIZE], key_0_off: usize, pin: u32) -> Self {
        assert!(key_0_off < 0x20);
        let mut bright = [0u8; KEY_SIZE];
        for (i, b) in bright.iter_mut().enumerate() {
            *b = REGIST_KEYS_0_PS5[i * 0x20 + key_0_off];
        }
        bright[0xc] ^= (pin >> 24) as u8;
        bright[0xd] ^= (pin >> 16) as u8;
        bright[0xe] ^= (pin >> 8) as u8;
        bright[0xf] ^= pin as u8;
        Self {
            bright,
            ambassador: *ambassador,
        }
    }

    pub fn ambassador(&self) -> &[u8; KEY_SIZE] {
        &self.ambassador
    }

    fn iv(&self, counter: u64) -> [u8; KEY_SIZE] {
        let mut mac = <Hmac<Sha256> as Mac>::new_from_slice(&HMAC_KEY_PS5).expect("hmac key");
        mac.update(&self.ambassador);
        mac.update(&counter.to_be_bytes());
        let out = mac.finalize().into_bytes();
        let mut iv = [0u8; KEY_SIZE];
        iv.copy_from_slice(&out[..KEY_SIZE]);
        iv
    }

    pub fn encrypt(&self, counter: u64, data: &mut [u8]) {
        let iv = self.iv(counter);
        Aes128CfbEnc::new(&self.bright.into(), &iv.into()).encrypt(data);
    }

    pub fn decrypt(&self, counter: u64, data: &mut [u8]) {
        let iv = self.iv(counter);
        Aes128CfbDec::new(&self.bright.into(), &iv.into()).decrypt(data);
    }
}

/// "Aeropause" value embedded in the registration payload (PS5 variant).
pub fn aeropause(key_1_off: usize, ambassador: &[u8; KEY_SIZE]) -> [u8; KEY_SIZE] {
    let wurzelbert = (-0x2di8) as u8;
    let mut out = [0u8; KEY_SIZE];
    for i in 0..KEY_SIZE {
        let k = AEROPAUSE_KEYS_1_PS5[i * 0x20 + key_1_off];
        out[i] = (ambassador[i] ^ k)
            .wrapping_add(wurzelbert)
            .wrapping_add(i as u8);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vector_iv_regist_ps5() {
        use crate::test_vectors::iv_regist_ps5::*;
        assert_eq!(RpCrypt::new_regist(&AMBASSADOR, 0, 0).iv(0), IV_EXPECTED);
    }

    #[test]
    fn vector_bright_regist_ps5() {
        use crate::test_vectors::bright_regist_ps5::*;
        assert_eq!(
            RpCrypt::new_regist(&AMBASSADOR, 0x1e, 78703893).bright,
            BRIGHT_EXPECTED
        );
    }

    #[test]
    fn cfb_roundtrip() {
        let c = RpCrypt::new_auth(&[7u8; 16], &[9u8; 16]);
        let plain = b"hello remote play, this is longer than one block".to_vec();
        let mut buf = plain.clone();
        c.encrypt(3, &mut buf);
        assert_ne!(buf, plain);
        c.decrypt(3, &mut buf);
        assert_eq!(buf, plain);
    }
}
