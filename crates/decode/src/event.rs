use serde::{Deserialize, Serialize};

/// Which on-chain program executed the swap.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Venue {
    /// Pump.fun bonding curve (`6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P`)
    PumpFunCurve,
    /// PumpSwap AMM (`pAMMBay6oceH9fJKBRHGP5D4bD4sWpmSwMn52FMfXEA`)
    PumpSwapAmm,
    /// Jupiter v6 aggregator — decoded from its `SwapEvent`s, whatever pools it routed through
    Jupiter,
    RaydiumAmmV4,
    RaydiumCpmm,
    RaydiumClmm,
    MeteoraDlmm,
    MeteoraDammV2,
    OrcaWhirlpool,
}

impl Venue {
    pub const ALL: [Venue; 9] = [
        Venue::PumpFunCurve,
        Venue::PumpSwapAmm,
        Venue::Jupiter,
        Venue::RaydiumAmmV4,
        Venue::RaydiumCpmm,
        Venue::RaydiumClmm,
        Venue::MeteoraDlmm,
        Venue::MeteoraDammV2,
        Venue::OrcaWhirlpool,
    ];

    pub fn as_str(&self) -> &'static str {
        match self {
            Venue::PumpFunCurve => "pumpfun_curve",
            Venue::PumpSwapAmm => "pumpswap_amm",
            Venue::Jupiter => "jupiter",
            Venue::RaydiumAmmV4 => "raydium_amm_v4",
            Venue::RaydiumCpmm => "raydium_cpmm",
            Venue::RaydiumClmm => "raydium_clmm",
            Venue::MeteoraDlmm => "meteora_dlmm",
            Venue::MeteoraDammV2 => "meteora_damm_v2",
            Venue::OrcaWhirlpool => "orca_whirlpool",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Venue::ALL.into_iter().find(|v| v.as_str() == s)
    }

    pub fn program_id(&self) -> &'static str {
        match self {
            Venue::PumpFunCurve => "6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P",
            Venue::PumpSwapAmm => "pAMMBay6oceH9fJKBRHGP5D4bD4sWpmSwMn52FMfXEA",
            Venue::Jupiter => "JUP6LkbZbjS1jKKwapdHNy74zcZ3tLUZoi5QNyVTaV4",
            Venue::RaydiumAmmV4 => "675kPX9MHTjS2zt1qfr1NYHuzeLXfQM9H24wFSUt1Mp8",
            Venue::RaydiumCpmm => "CPMMoo8L3F4NbTegBCKVNunggL7H1ZpdTHKxQB5qKP1C",
            Venue::RaydiumClmm => "CAMMCzo5YL8w4VFF8KVHrK22GGUsp5VTaW7grrKgrWqK",
            Venue::MeteoraDlmm => "LBUZKhRxPF3XUpBCjp4YzTKgLccjZhTSDM9YuVaPwxo",
            Venue::MeteoraDammV2 => "cpamdpZCGKUy5JxQXB4dcpGPiikHawvSWAd6mEn1sGG",
            Venue::OrcaWhirlpool => "whirLbMiicVdio4qvUfM5KAg6Ct8VwpYzGff3uctyCc",
        }
    }

    pub fn from_program(id: &str) -> Option<Self> {
        Venue::ALL.into_iter().find(|v| v.program_id() == id)
    }

    /// Venues decoded generically from the token transfers under their swap
    /// instruction (everything without a purpose-built decoder).
    pub fn is_transfer_decoded(&self) -> bool {
        !matches!(self, Venue::PumpFunCurve | Venue::PumpSwapAmm | Venue::Jupiter)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Side {
    Buy,
    Sell,
}

impl Side {
    pub fn as_str(&self) -> &'static str {
        match self {
            Side::Buy => "buy",
            Side::Sell => "sell",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "buy" => Some(Side::Buy),
            "sell" => Some(Side::Sell),
            _ => None,
        }
    }
}

/// A fully decoded swap by a tracked wallet. All amounts are raw on-chain units.
///
/// `sol_amount` is what the wallet actually paid (buy) or received (sell) for
/// the token leg, *net of venue fees* — that is the number cost basis needs.
/// `fee_lamports` is informational only.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SwapEvent {
    /// base58 transaction signature
    pub signature: String,
    pub slot: u64,
    /// unix seconds; `None` if the RPC omitted it
    pub block_time: Option<i64>,
    /// the trader (owner of the token account that changed), base58
    pub wallet: String,
    /// token mint, base58
    pub mint: String,
    pub venue: Venue,
    pub side: Side,
    /// raw token units; scale by `token_decimals` for display
    pub token_amount: u64,
    /// decimals as reported by the node for this mint — never hardcoded
    pub token_decimals: u8,
    /// lamports paid (buy) / received (sell), net of venue fees
    pub sol_amount: u64,
    /// venue fees taken in lamports (0 when fees were taken in tokens or are
    /// embedded in the pool price and not separately reported)
    pub fee_lamports: u64,
    /// position of the swap instruction in the flattened (outer+inner) ix list;
    /// disambiguates multiple swaps in one transaction
    pub ix_index: u16,
}

/// A tracked wallet moved a token but no known layout matched.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UnknownSwap {
    pub signature: String,
    pub slot: u64,
    pub block_time: Option<i64>,
    pub wallet: String,
    /// Non-trivial top-level programs invoked (base58), sorted.
    pub programs: Vec<String>,
}

/// Output of [`crate::decode_transaction`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "data", rename_all = "snake_case")]
pub enum DecodedTx {
    /// One tx can contain several swaps (bundlers, bots).
    Swaps(Vec<SwapEvent>),
    /// Logged and stored for the audit trail, ignored by pnl.
    Unknown(UnknownSwap),
    /// Transfer, ATA create, failed tx, etc.
    NotASwap,
}

/// Display metadata for a mint (symbol/name), resolved out-of-band from the
/// Token-2022 metadata extension or the Metaplex metadata account. Lives here
/// because this crate holds the shared domain types; `decode` itself never
/// looks metadata up.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TokenMeta {
    pub mint: String,
    /// empty when the chain has no metadata for this mint
    pub symbol: String,
    pub name: String,
    pub uri: String,
    /// image URL from the off-chain metadata JSON at `uri` (empty until fetched / if none)
    #[serde(default)]
    pub image: String,
}
