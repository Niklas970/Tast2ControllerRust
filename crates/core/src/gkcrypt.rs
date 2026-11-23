//! "GK crypt": AES-128 counter-style key stream plus truncated GMAC used on the Takion stream.
//! Port of chiaki-ng `lib/src/gkcrypt.c` and `lib/src/ecdh.c`.

use aes::cipher::{BlockEncrypt, KeyInit};
use aes::Aes128;
use aes_gcm::aead::consts::U16;
use aes_gcm::aead::AeadInPlace;
use aes_gcm::AesGcm;
use hmac::{Hmac, Mac};
use k256::elliptic_curve::sec1::ToEncodedPoint;
use sha2::{Digest, Sha256};

pub const BLOCK_SIZE: usize = 16;
pub const GMAC_SIZE: usize = 4;
const GMAC_KEY_REFRESH_KEY_POS: u64 = 45000;
const GMAC_KEY_REFRESH_IV_OFFSET: u64 = 44910;

type Aes128Gcm16 = AesGcm<Aes128, U16>;

/// Adds `v` to the 16 byte little-endian counter `base`.
fn counter_add(base: &[u8; 16], v: u64) -> [u8; 16] {
    u128::from_le_bytes(*base)
        .wrapping_add(v as u128)
        .to_le_bytes()
}

fn gen_gmac_key(index: u64, key_base: &[u8; 16], iv: &[u8; 16]) -> [u8; 16] {
    let mut data = [0u8; 32];
    data[..16].copy_from_slice(key_base);
    data[16..].copy_from_slice(&counter_add(
        iv,
        index.wrapping_mul(GMAC_KEY_REFRESH_IV_OFFSET),
    ));
    let md = Sha256::digest(data);
    let mut out = [0u8; 16];
    for i in 0..16 {
        out[i] = md[i] ^ md[i + 16];
    }
    out
}

pub struct GkCrypt {
    #[cfg_attr(not(test), allow(dead_code))]
    key_base: [u8; 16],
    cipher: Aes128,
    iv: [u8; 16],
    key_gmac_base: [u8; 16],
    key_gmac_index_current: u64,
    key_gmac_current: [u8; 16],
}

impl GkCrypt {
    /// `index` is 2 for the local (client to console) direction and 3 for the remote one.
    pub fn new(index: u8, handshake_key: &[u8; 16], ecdh_secret: &[u8; 32]) -> Self {
        let mut data = [0u8; 3 + 16 + 2];
        data[0] = 1;
        data[1] = index;
        data[2] = 0;
        data[3..19].copy_from_slice(handshake_key);
        data[19] = 1;
        data[20] = 0;
        let mut mac = <Hmac<Sha256> as Mac>::new_from_slice(ecdh_secret).expect("hmac key");
        mac.update(&data);
        let hmac = mac.finalize().into_bytes();

        let mut key_base = [0u8; 16];
        let mut iv = [0u8; 16];
        key_base.copy_from_slice(&hmac[..16]);
        iv.copy_from_slice(&hmac[16..32]);

        let key_gmac_base = gen_gmac_key(0, &key_base, &iv);
        Self {
            key_base,
            cipher: Aes128::new(&key_base.into()),
            iv,
            key_gmac_base,
            key_gmac_index_current: 0,
            key_gmac_current: key_gmac_base,
        }
    }

    /// XORs the key stream at `key_pos` into `buf` (encryption and decryption are the same).
    pub fn crypt(&self, key_pos: u64, buf: &mut [u8]) {
        let pad = (key_pos % BLOCK_SIZE as u64) as usize;
        let mut counter = key_pos / BLOCK_SIZE as u64;
        let mut offset = 0usize; // index into buf
        let mut skip = pad;
        while offset < buf.len() {
            let mut block = aes::Block::from(counter_add(&self.iv, counter));
            self.cipher.encrypt_block(&mut block);
            for &k in block.iter().skip(skip) {
                if offset >= buf.len() {
                    break;
                }
                buf[offset] ^= k;
                offset += 1;
            }
            skip = 0;
            counter += 1;
        }
    }

    pub fn gmac(&mut self, key_pos: u64, buf: &[u8]) -> [u8; GMAC_SIZE] {
        let iv = counter_add(&self.iv, key_pos / 16);
        let key_index = key_pos.saturating_sub(1) / GMAC_KEY_REFRESH_KEY_POS;
        let key = if key_index > self.key_gmac_index_current {
            self.key_gmac_current = gen_gmac_key(key_index, &self.key_gmac_base, &self.iv);
            self.key_gmac_index_current = key_index;
            self.key_gmac_current
        } else if key_index < self.key_gmac_index_current {
            if key_index == 0 {
                self.key_gmac_base
            } else {
                gen_gmac_key(key_index, &self.key_gmac_base, &self.iv)
            }
        } else {
            self.key_gmac_current
        };

        let gcm = Aes128Gcm16::new(&key.into());
        let tag = gcm
            .encrypt_in_place_detached(&iv.into(), buf, &mut [])
            .expect("gmac");
        let mut out = [0u8; GMAC_SIZE];
        out.copy_from_slice(&tag[..GMAC_SIZE]);
        out
    }
}

/// Ephemeral secp256k1 key pair used for the BIG/BANG exchange.
pub struct Ecdh {
    secret: k256::SecretKey,
}

impl Default for Ecdh {
    fn default() -> Self {
        Self::new()
    }
}

impl Ecdh {
    pub fn new() -> Self {
        Self {
            secret: k256::SecretKey::random(&mut rand::rngs::OsRng),
        }
    }

    #[cfg(test)]
    fn from_private(key: &[u8]) -> Self {
        Self {
            secret: k256::SecretKey::from_slice(key).unwrap(),
        }
    }

    /// Returns (uncompressed public key, HMAC-SHA256(handshake_key, public key)).
    pub fn local_pub_key(&self, handshake_key: &[u8; 16]) -> (Vec<u8>, Vec<u8>) {
        let point = self.secret.public_key().to_encoded_point(false);
        let key = point.as_bytes().to_vec();
        let mut mac = <Hmac<Sha256> as Mac>::new_from_slice(handshake_key).expect("hmac key");
        mac.update(&key);
        let sig = mac.finalize().into_bytes().to_vec();
        (key, sig)
    }

    pub fn derive_secret(&self, remote_pub_key: &[u8]) -> anyhow::Result<[u8; 32]> {
        let remote = k256::PublicKey::from_sec1_bytes(remote_pub_key)
            .map_err(|_| anyhow::anyhow!("invalid remote ECDH public key"))?;
        let shared =
            k256::ecdh::diffie_hellman(self.secret.to_nonzero_scalar(), remote.as_affine());
        let mut out = [0u8; 32];
        out.copy_from_slice(shared.raw_secret_bytes());
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counter_add_carries() {
        let mut base = [0u8; 16];
        base[0] = 0xff;
        let r = counter_add(&base, 1);
        assert_eq!(r[0], 0);
        assert_eq!(r[1], 1);
    }

    #[test]
    fn crypt_unaligned_matches_aligned() {
        let c = GkCrypt::new(2, &[1u8; 16], &[2u8; 32]);
        let mut whole = vec![0u8; 64];
        c.crypt(0, &mut whole);
        let mut part = vec![0u8; 20];
        c.crypt(7, &mut part);
        assert_eq!(&part[..], &whole[7..27]);
    }

    use crate::test_vectors as tv;

    /// A crypt with only the GMAC state set, like chiaki-ng's GMAC tests build it.
    fn gmac_only(gmac_key: [u8; 16], iv: [u8; 16]) -> GkCrypt {
        GkCrypt {
            key_base: [0; 16],
            cipher: Aes128::new(&[0u8; 16].into()),
            iv,
            key_gmac_base: [0; 16],
            key_gmac_index_current: 0,
            key_gmac_current: gmac_key,
        }
    }

    #[test]
    fn vector_ecdh() {
        use tv::ecdh::*;
        let e = Ecdh::from_private(&LOCAL_PRIVATE_KEY);
        let (key, sig) = e.local_pub_key(&HANDSHAKE_KEY);
        assert_eq!(key, LOCAL_PUBLIC_KEY);
        assert_eq!(sig, LOCAL_PUBLIC_KEY_SIG);
        assert_eq!(e.derive_secret(&REMOTE_PUBLIC_KEY).unwrap(), SECRET);
    }

    #[test]
    fn vector_key_stream() {
        use tv::key_stream::*;
        let c = GkCrypt::new(42, &HANDSHAKE_KEY, &ECDH_SECRET);
        assert_eq!(c.key_base, GKCRYPT_KEY);
        assert_eq!(c.iv, GKCRYPT_IV);
        let mut ks = [0u8; 0x20];
        c.crypt(0x30, &mut ks);
        assert_eq!(ks, KEY_STREAM);
        let mut ks = [0u8; 0x20];
        c.crypt((1u64 << 32) + 0x420, &mut ks);
        assert_eq!(ks, KEY_STREAM_HIGH);
    }

    #[test]
    fn vector_endecrypt() {
        use tv::endecrypt::*;
        let c = GkCrypt::new(42, &HANDSHAKE_KEY, &ECDH_SECRET);
        assert_eq!(c.key_base, GKCRYPT_KEY);
        assert_eq!(c.iv, GKCRYPT_IV);
        let mut buf = CLEAR_DATA;
        c.crypt(0x11, &mut buf);
        assert_eq!(buf, ENC_DATA);
        let mut buf = CLEAR_DATA;
        c.crypt((1u64 << 32) + 0x420, &mut buf);
        assert_eq!(buf, ENC_DATA_HIGH);
    }

    #[test]
    fn vector_gmac() {
        use tv::gmac::*;
        assert_eq!(
            gmac_only(GKCRYPT_KEY, GKCRYPT_IV).gmac(0x69a0, &BUF),
            GMAC_EXPECTED
        );
        assert_eq!(
            gmac_only(GKCRYPT_KEY, GKCRYPT_IV).gmac((1u64 << 32) + 0x420, &BUF),
            GMAC_EXPECTED_HIGH
        );
    }

    #[test]
    fn vector_gmac_split() {
        use tv::gmac_split::*;
        assert_eq!(
            gmac_only(GKCRYPT_KEY, GKCRYPT_IV).gmac(0xadf0, &BUF),
            GMAC_EXPECTED
        );
        assert_eq!(
            gmac_only(GKCRYPT_KEY, GKCRYPT_IV).gmac((1u64 << 32) + 0x420, &BUF),
            GMAC_EXPECTED_HIGH
        );
    }

    #[test]
    fn vector_gen_gmac_key() {
        use tv::gen_gmac_key::*;
        assert_eq!(gen_gmac_key(1, &KEY_INITIAL, &IV), KEY_RESULT);
    }

    #[test]
    fn ecdh_agrees() {
        let a = Ecdh::new();
        let b = Ecdh::new();
        let (pa, _) = a.local_pub_key(&[0u8; 16]);
        let (pb, _) = b.local_pub_key(&[0u8; 16]);
        assert_eq!(pa.len(), 65);
        assert_eq!(a.derive_secret(&pb).unwrap(), b.derive_secret(&pa).unwrap());
    }
}
