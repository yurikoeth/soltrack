//! Program-derived addresses: the standard Solana derivation (try bumps
//! 255→0, first hash that is *not* an ed25519 point wins).

use curve25519_dalek::edwards::CompressedEdwardsY;
use sha2::{Digest, Sha256};

pub fn is_on_curve(bytes: &[u8; 32]) -> bool {
    CompressedEdwardsY(*bytes).decompress().is_some()
}

pub fn find_program_address(seeds: &[&[u8]], program_id: &[u8; 32]) -> Option<([u8; 32], u8)> {
    for bump in (0..=255u8).rev() {
        let mut h = Sha256::new();
        for s in seeds {
            h.update(s);
        }
        h.update([bump]);
        h.update(program_id);
        h.update(b"ProgramDerivedAddress");
        let out: [u8; 32] = h.finalize().into();
        if !is_on_curve(&out) {
            return Some((out, bump));
        }
    }
    None
}

/// Same, with base58 program id and result.
pub fn derive(seeds: &[&[u8]], program_id: &str) -> Option<String> {
    let program = pubkey_bytes(program_id)?;
    find_program_address(seeds, &program).map(|(a, _)| bs58::encode(a).into_string())
}

pub fn pubkey_bytes(b58: &str) -> Option<[u8; 32]> {
    bs58::decode(b58).into_vec().ok()?.try_into().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn system_program_is_on_curve_and_pdas_are_not() {
        // 11111111111111111111111111111111 = all zeros = the identity point, on curve
        assert!(is_on_curve(&[0u8; 32]));
        let (pda, _) = find_program_address(&[b"metadata"], &[7u8; 32]).unwrap();
        assert!(!is_on_curve(&pda));
    }
}
