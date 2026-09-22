//! Anchor wire-format helpers: discriminators and little-endian field reads.

use sha2::{Digest, Sha256};

/// Prefix of every `emit_cpi!` self-invoke: sha256("anchor:event")[..8], byte-reversed (as Anchor defines it).
pub const EVENT_IX_PREFIX: [u8; 8] = [0xe4, 0x45, 0xa5, 0x2e, 0x51, 0xcb, 0x9a, 0x1d];

/// sha256(`{namespace}:{name}`)[..8]
pub fn discriminator(namespace: &str, name: &str) -> [u8; 8] {
    let h = Sha256::digest(format!("{namespace}:{name}").as_bytes());
    let mut out = [0u8; 8];
    out.copy_from_slice(&h[..8]);
    out
}

/// Cursor over a borsh-encoded byte slice. Every read is bounds-checked and
/// returns `None` past the end so a truncated/legacy event never panics.
pub struct Reader<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    pub fn new(buf: &'a [u8]) -> Self {
        Self { buf, pos: 0 }
    }

    pub fn remaining(&self) -> usize {
        self.buf.len().saturating_sub(self.pos)
    }

    fn take(&mut self, n: usize) -> Option<&'a [u8]> {
        let end = self.pos.checked_add(n)?;
        let s = self.buf.get(self.pos..end)?;
        self.pos = end;
        Some(s)
    }

    pub fn u8(&mut self) -> Option<u8> {
        self.take(1).map(|s| s[0])
    }

    pub fn bool(&mut self) -> Option<bool> {
        self.u8().map(|b| b != 0)
    }

    pub fn u64(&mut self) -> Option<u64> {
        self.take(8).map(|s| u64::from_le_bytes(s.try_into().unwrap()))
    }

    pub fn i64(&mut self) -> Option<i64> {
        self.take(8).map(|s| i64::from_le_bytes(s.try_into().unwrap()))
    }

    pub fn pubkey(&mut self) -> Option<String> {
        self.take(32).map(|s| bs58::encode(s).into_string())
    }

    pub fn skip(&mut self, n: usize) -> Option<()> {
        self.take(n).map(|_| ())
    }

    /// Borsh `String`: u32 LE length + UTF-8 bytes (lossy).
    pub fn string(&mut self) -> Option<String> {
        let len = self.u32()? as usize;
        self.take(len).map(|b| String::from_utf8_lossy(b).into_owned())
    }

    pub fn u32(&mut self) -> Option<u32> {
        self.take(4).map(|s| u32::from_le_bytes(s.try_into().unwrap()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_discriminators() {
        assert_eq!(hex(&discriminator("global", "buy")), "66063d1201daebea");
        assert_eq!(hex(&discriminator("global", "sell")), "33e685a4017f83ad");
        assert_eq!(hex(&discriminator("event", "TradeEvent")), "bddb7fd34ee661ee");
        assert_eq!(hex(&discriminator("event", "BuyEvent")), "67f4521f2cf57777");
        assert_eq!(hex(&discriminator("event", "SellEvent")), "3e2f370aa503dc2a");
        let mut rev = discriminator("anchor", "event");
        rev.reverse();
        assert_eq!(EVENT_IX_PREFIX, rev);
    }

    #[test]
    fn reader_is_bounds_checked() {
        let mut r = Reader::new(&[1, 0, 0, 0, 0, 0, 0, 0, 9]);
        assert_eq!(r.u64(), Some(1));
        assert_eq!(r.remaining(), 1);
        assert_eq!(r.u64(), None);
        assert_eq!(r.u8(), Some(9));
        assert_eq!(r.u8(), None);
    }

    fn hex(b: &[u8]) -> String {
        b.iter().map(|x| format!("{x:02x}")).collect()
    }
}
