//! Metadata parsing against real mainnet accounts captured 2026-09-08
//! (`tests/fixtures/token_meta.json`): a Token-2022 Pump.fun mint with the
//! `TokenMetadata` extension, a legacy SPL mint, and that mint's Metaplex
//! metadata PDA.

use base64::Engine;
use ingest::metadata::{
    find_program_address, metaplex_metadata_pda, parse_metaplex_metadata, parse_token2022_metadata,
    ParsedMeta, METAPLEX_PROGRAM, TOKEN_2022_PROGRAM,
};
use serde_json::Value;

fn fixtures() -> Value {
    let path = format!("{}/tests/fixtures/token_meta.json", env!("CARGO_MANIFEST_DIR"));
    serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
}

fn data(v: &Value) -> Vec<u8> {
    base64::engine::general_purpose::STANDARD
        .decode(v["data_base64"].as_str().unwrap())
        .unwrap()
}

#[test]
fn token2022_extension_metadata() {
    let f = fixtures();
    let acct = &f["token2022_mint"];
    assert_eq!(acct["owner"], TOKEN_2022_PROGRAM);
    let parsed = parse_token2022_metadata(&data(acct)).expect("has TokenMetadata extension");
    assert_eq!(parsed.name, "Clanker");
    assert_eq!(parsed.symbol, "Clanker");
    assert!(parsed.uri.starts_with("https://"));
    let meta = parsed.into_meta("BitAst9t1moZhc88Ccwpk6a3gV6JmTXBEotiQmTZpump");
    assert_eq!(meta.symbol, "Clanker");
}

#[test]
fn legacy_mint_has_no_extension_metadata() {
    let f = fixtures();
    let acct = &f["legacy_mint"];
    assert_eq!(acct["owner"], "TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA");
    assert_eq!(data(acct).len(), 82);
    assert_eq!(parse_token2022_metadata(&data(acct)), None);
}

#[test]
fn metaplex_pda_and_metadata() {
    let f = fixtures();
    let m = &f["metaplex_metadata"];
    let mint = m["mint"].as_str().unwrap();
    assert_eq!(metaplex_metadata_pda(mint).unwrap(), m["pda"].as_str().unwrap());
    assert_eq!(m["owner"], METAPLEX_PROGRAM);
    let parsed = parse_metaplex_metadata(&data(m)).unwrap();
    assert_eq!(
        parsed,
        ParsedMeta {
            name: "Apple".into(),
            symbol: "AAPL".into(),
            uri: parsed.uri.clone(),
        }
    );
    assert!(!parsed.name.contains('\0'), "NUL padding must be stripped");
}

#[test]
fn pda_bump_matches_reference() {
    let f = fixtures();
    let m = &f["metaplex_metadata"];
    let mint: [u8; 32] = bs58::decode(m["mint"].as_str().unwrap()).into_vec().unwrap().try_into().unwrap();
    let program: [u8; 32] = bs58::decode(METAPLEX_PROGRAM).into_vec().unwrap().try_into().unwrap();
    let (_, bump) = find_program_address(&[b"metadata", &program, &mint], &program).unwrap();
    assert_eq!(bump as u64, m["bump"].as_u64().unwrap());
}

#[test]
fn image_is_read_from_pump_metadata_json() {
    let path = format!("{}/tests/fixtures/pump_metadata.json", env!("CARGO_MANIFEST_DIR"));
    let json = std::fs::read_to_string(path).unwrap();
    assert_eq!(
        ingest::metadata::image_from_metadata_json(&json).as_deref(),
        Some("https://ipfs.io/ipfs/Qmbw1ZJRWfTHPBSzwmZgwbzLHtTBSckmFWaXdqWE9PS8ga")
    );
    assert_eq!(
        ingest::metadata::image_from_metadata_json(r#"{"image":"ipfs://QmX/1.png"}"#).as_deref(),
        Some("https://ipfs.io/ipfs/QmX/1.png")
    );
    assert_eq!(ingest::metadata::image_from_metadata_json(r#"{"name":"x"}"#), None);
    assert_eq!(ingest::metadata::image_from_metadata_json("not json"), None);
    assert_eq!(ingest::metadata::normalize_ipfs("https://a/b.png"), "https://a/b.png");
}

#[test]
fn gateway_fallbacks_and_extensions() {
    let c = ingest::metadata::gateway_candidates("https://ipfs.io/ipfs/QmX/a.png");
    assert_eq!(c[0], "https://ipfs.io/ipfs/QmX/a.png");
    assert!(c.iter().any(|u| u.starts_with("https://cloudflare-ipfs.com/ipfs/QmX/a.png")));
    assert_eq!(c.len(), 4);
    assert_eq!(ingest::metadata::gateway_candidates("ipfs://QmY"), ingest::metadata::gateway_candidates("https://ipfs.io/ipfs/QmY"));
    assert_eq!(ingest::metadata::gateway_candidates("https://pbs.twimg.com/x.jpg"), vec!["https://pbs.twimg.com/x.jpg"]);
    assert_eq!(ingest::metadata::image_ext("image/jpeg; charset=binary"), "jpg");
    assert_eq!(ingest::metadata::image_ext("image/webp"), "webp");
    assert_eq!(ingest::metadata::image_ext("application/octet-stream"), "png");
}

#[test]
fn truncated_inputs_do_not_panic() {
    let f = fixtures();
    let full = data(&f["token2022_mint"]);
    for n in 0..full.len() {
        let _ = parse_token2022_metadata(&full[..n]);
    }
    let full = data(&f["metaplex_metadata"]);
    for n in 0..full.len() {
        let _ = parse_metaplex_metadata(&full[..n]);
    }
}
