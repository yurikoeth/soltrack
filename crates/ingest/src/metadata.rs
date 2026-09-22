//! Mint → symbol/name. Two sources, tried in order:
//!
//! 1. **Token-2022 `TokenMetadata` extension** on the mint account itself
//!    (every Pump.fun mint since 2025). One `getAccountInfo`.
//! 2. **Metaplex Token Metadata** PDA (`["metadata", program, mint]`) for
//!    legacy SPL-Token mints. Needs an on-curve check to derive the PDA,
//!    hence the `curve25519-dalek` dependency. Two `getAccountInfo`s.
//!
//! Parsing is pure and fixture-tested; only `fetch_token_meta` touches RPC.

use decode::TokenMeta;

use crate::rpc::{RpcClient, RpcError};

pub const TOKEN_2022_PROGRAM: &str = "TokenzQdBNbLqP5VEhdkAS6EPFLC1PHnBqCXEpPxuEb";
pub const METAPLEX_PROGRAM: &str = "metaqbxxUerdq28cj1RbAWkYQm3ybzjb6a8bt518x1s";

/// Token-2022 extension type id for `TokenMetadata`.
const EXT_TOKEN_METADATA: u16 = 19;
/// Offset of the account-type byte in an extended Token-2022 account.
const ACCOUNT_TYPE_OFFSET: usize = 165;
const ACCOUNT_TYPE_MINT: u8 = 1;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedMeta {
    pub name: String,
    pub symbol: String,
    pub uri: String,
}

impl ParsedMeta {
    pub fn into_meta(self, mint: &str) -> TokenMeta {
        TokenMeta {
            mint: mint.to_string(),
            symbol: self.symbol,
            name: self.name,
            uri: self.uri,
            image: String::new(),
        }
    }
}

/// Parse the `TokenMetadata` extension out of a Token-2022 mint account.
///
/// Layout: 82-byte base mint, padding to 165, account type byte, then TLV
/// entries `[type u16][len u16][value]`. The metadata value is
/// `update_authority[32] mint[32] name:String symbol:String uri:String …`.
pub fn parse_token2022_metadata(data: &[u8]) -> Option<ParsedMeta> {
    if data.len() <= ACCOUNT_TYPE_OFFSET || data[ACCOUNT_TYPE_OFFSET] != ACCOUNT_TYPE_MINT {
        return None;
    }
    let mut i = ACCOUNT_TYPE_OFFSET + 1;
    while i + 4 <= data.len() {
        let ty = u16::from_le_bytes([data[i], data[i + 1]]);
        let len = u16::from_le_bytes([data[i + 2], data[i + 3]]) as usize;
        let start = i + 4;
        let end = start.checked_add(len)?;
        if end > data.len() {
            return None;
        }
        if ty == EXT_TOKEN_METADATA {
            let v = &data[start..end];
            let mut pos = 64; // update_authority + mint
            let name = read_string(v, &mut pos)?;
            let symbol = read_string(v, &mut pos)?;
            let uri = read_string(v, &mut pos)?;
            return Some(ParsedMeta {
                name: clean(&name),
                symbol: clean(&symbol),
                uri: clean(&uri),
            });
        }
        if ty == 0 && len == 0 {
            break; // uninitialized padding
        }
        i = end;
    }
    None
}

/// Parse a Metaplex `Metadata` account:
/// `key u8, update_authority[32], mint[32], name:String, symbol:String, uri:String, …`.
/// Metaplex pads strings with NULs to fixed widths (32/10/200); `clean` strips them.
pub fn parse_metaplex_metadata(data: &[u8]) -> Option<ParsedMeta> {
    let mut pos = 1 + 32 + 32;
    let name = read_string(data, &mut pos)?;
    let symbol = read_string(data, &mut pos)?;
    let uri = read_string(data, &mut pos)?;
    Some(ParsedMeta {
        name: clean(&name),
        symbol: clean(&symbol),
        uri: clean(&uri),
    })
}

/// Metaplex metadata PDA for `mint`, base58.
pub fn metaplex_metadata_pda(mint: &str) -> Option<String> {
    let mint_bytes = decode::pda::pubkey_bytes(mint)?;
    let program = decode::pda::pubkey_bytes(METAPLEX_PROGRAM)?;
    decode::pda::derive(&[b"metadata", &program, &mint_bytes], METAPLEX_PROGRAM)
}

pub use decode::pda::find_program_address;

/// Image URL from a token's off-chain metadata JSON (`{"image": "..."}`),
/// with `ipfs://` normalised to a public gateway.
pub fn image_from_metadata_json(json: &str) -> Option<String> {
    let v: serde_json::Value = serde_json::from_str(json).ok()?;
    let img = v.get("image")?.as_str()?.trim();
    if img.is_empty() {
        return None;
    }
    Some(normalize_ipfs(img))
}

pub fn normalize_ipfs(url: &str) -> String {
    if let Some(rest) = url.strip_prefix("ipfs://") {
        let rest = rest.trim_start_matches("ipfs/");
        return format!("https://ipfs.io/ipfs/{rest}");
    }
    url.to_string()
}

/// Fetch the metadata JSON at `uri` (IPFS gateway / pump.fun) and return its
/// image URL. Bounded: 10 s timeout, 256 KB body.
pub async fn fetch_image_url(http: &reqwest::Client, uri: &str) -> Option<String> {
    let uri = normalize_ipfs(uri.trim());
    if !uri.starts_with("https://") && !uri.starts_with("http://") {
        return None;
    }
    let resp = http.get(&uri).send().await.ok()?;
    if !resp.status().is_success() {
        return None;
    }
    let bytes = resp.bytes().await.ok()?;
    if bytes.len() > 256 * 1024 {
        return None;
    }
    image_from_metadata_json(std::str::from_utf8(&bytes).ok()?)
}

/// Alternative public IPFS gateways, tried in order when the URL's own
/// gateway refuses (ipfs.io rate-limits hotlinking aggressively).
const IPFS_GATEWAYS: &[&str] = &[
    "https://ipfs.io/ipfs/",
    "https://cloudflare-ipfs.com/ipfs/",
    "https://gateway.pinata.cloud/ipfs/",
    "https://dweb.link/ipfs/",
];

/// For an IPFS gateway URL, the same content on every gateway we know;
/// otherwise just the URL itself.
pub fn gateway_candidates(url: &str) -> Vec<String> {
    let url = normalize_ipfs(url);
    for g in IPFS_GATEWAYS {
        if let Some(cid_path) = url.strip_prefix(g) {
            let mut v: Vec<String> = IPFS_GATEWAYS.iter().map(|x| format!("{x}{cid_path}")).collect();
            // the original first
            v.retain(|c| c != &url);
            v.insert(0, url.clone());
            return v;
        }
    }
    vec![url]
}

/// File extension for an image content type (default png).
pub fn image_ext(content_type: &str) -> &'static str {
    let ct = content_type.split(';').next().unwrap_or("").trim().to_ascii_lowercase();
    match ct.as_str() {
        "image/jpeg" | "image/jpg" => "jpg",
        "image/gif" => "gif",
        "image/webp" => "webp",
        "image/svg+xml" => "svg",
        _ => "png",
    }
}

/// Download an image (≤ 3 MB), trying alternative IPFS gateways on failure.
/// Returns the bytes and a file extension.
pub async fn download_image(http: &reqwest::Client, url: &str) -> Option<(Vec<u8>, &'static str)> {
    for candidate in gateway_candidates(url) {
        let Ok(resp) = http.get(&candidate).send().await else { continue };
        if !resp.status().is_success() {
            tracing::debug!(url = %candidate, status = %resp.status(), "image fetch refused");
            continue;
        }
        let ct = resp
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_string();
        let Ok(bytes) = resp.bytes().await else { continue };
        if bytes.is_empty() || bytes.len() > 3 * 1024 * 1024 {
            continue;
        }
        // some gateways answer HTML error pages with 200; sniff the magic bytes
        let looks_like_image = bytes.starts_with(&[0x89, b'P', b'N', b'G'])
            || bytes.starts_with(&[0xFF, 0xD8])
            || bytes.starts_with(b"GIF8")
            || bytes.starts_with(b"RIFF")
            || bytes.starts_with(b"<svg")
            || bytes.starts_with(b"<?xml");
        if !looks_like_image && !ct.starts_with("image/") {
            continue;
        }
        let ext = if ct.starts_with("image/") {
            image_ext(&ct)
        } else if bytes.starts_with(&[0xFF, 0xD8]) {
            "jpg"
        } else if bytes.starts_with(b"GIF8") {
            "gif"
        } else if bytes.starts_with(b"RIFF") {
            "webp"
        } else if bytes.starts_with(b"<") {
            "svg"
        } else {
            "png"
        };
        return Some((bytes.to_vec(), ext));
    }
    None
}

/// HTTP client for off-chain metadata (separate from the RPC client).
pub fn metadata_http_client() -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .user_agent(concat!("soltrack/", env!("CARGO_PKG_VERSION")))
        .build()
        .expect("reqwest client")
}

/// Resolve metadata for `mint`. `Ok(None)` when the chain has none.
pub async fn fetch_token_meta(rpc: &RpcClient, mint: &str) -> Result<Option<TokenMeta>, RpcError> {
    let Some((owner, data)) = rpc.get_account(mint).await? else {
        return Ok(None);
    };
    if owner == TOKEN_2022_PROGRAM {
        if let Some(m) = parse_token2022_metadata(&data) {
            return Ok(Some(m.into_meta(mint)));
        }
    }
    let Some(pda) = metaplex_metadata_pda(mint) else {
        return Ok(None);
    };
    match rpc.get_account(&pda).await? {
        Some((pda_owner, data)) if pda_owner == METAPLEX_PROGRAM => {
            Ok(parse_metaplex_metadata(&data).map(|m| m.into_meta(mint)))
        }
        _ => Ok(None),
    }
}

/// Borsh `String`: u32 LE length + UTF-8 bytes.
fn read_string(buf: &[u8], pos: &mut usize) -> Option<String> {
    let len = u32::from_le_bytes(buf.get(*pos..*pos + 4)?.try_into().ok()?) as usize;
    *pos += 4;
    let bytes = buf.get(*pos..(*pos).checked_add(len)?)?;
    *pos += len;
    Some(String::from_utf8_lossy(bytes).into_owned())
}

fn clean(s: &str) -> String {
    s.trim_end_matches('\0').trim().to_string()
}
