//! Serde model of `getTransaction` (`encoding: "json"`,
//! `maxSupportedTransactionVersion: 0`) plus the helpers decoders need.

use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RawTransaction {
    pub slot: u64,
    #[serde(default)]
    pub block_time: Option<i64>,
    pub transaction: Transaction,
    pub meta: Meta,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Transaction {
    pub signatures: Vec<String>,
    pub message: Message,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Message {
    pub account_keys: Vec<String>,
    pub instructions: Vec<Instruction>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub address_table_lookups: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recent_blockhash: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub header: Option<Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Instruction {
    pub program_id_index: usize,
    pub accounts: Vec<usize>,
    /// base58
    pub data: String,
    #[serde(default)]
    pub stack_height: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Meta {
    #[serde(default)]
    pub err: Option<Value>,
    #[serde(default)]
    pub fee: u64,
    #[serde(default)]
    pub pre_balances: Vec<u64>,
    #[serde(default)]
    pub post_balances: Vec<u64>,
    #[serde(default)]
    pub inner_instructions: Option<Vec<InnerInstructions>>,
    #[serde(default)]
    pub pre_token_balances: Option<Vec<TokenBalance>>,
    #[serde(default)]
    pub post_token_balances: Option<Vec<TokenBalance>>,
    #[serde(default)]
    pub loaded_addresses: Option<LoadedAddresses>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub log_messages: Option<Vec<String>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InnerInstructions {
    /// index of the outer instruction these belong to
    pub index: usize,
    pub instructions: Vec<Instruction>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LoadedAddresses {
    #[serde(default)]
    pub writable: Vec<String>,
    #[serde(default)]
    pub readonly: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TokenBalance {
    pub account_index: usize,
    pub mint: String,
    #[serde(default)]
    pub owner: Option<String>,
    pub ui_token_amount: UiTokenAmount,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UiTokenAmount {
    /// raw units as a decimal string (u64 range)
    pub amount: String,
    pub decimals: u8,
}

/// One instruction with indices resolved to pubkeys, in execution order.
#[derive(Debug, Clone)]
pub struct FlatIx<'a> {
    /// position in the flattened outer+inner list
    pub ix_index: u16,
    /// 1 for outer instructions, 2+ for CPIs
    pub stack_height: u32,
    pub program: &'a str,
    pub accounts: Vec<&'a str>,
    pub data: Vec<u8>,
}

/// Owner and mint of a token account, from the balance metadata.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TokenAccountInfo<'a> {
    pub owner: &'a str,
    pub mint: &'a str,
    pub decimals: u8,
}

/// Net change of one token account across the transaction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TokenDelta {
    pub account: String,
    pub owner: String,
    pub mint: String,
    pub decimals: u8,
    pub delta: i128,
}

impl RawTransaction {
    pub fn signature(&self) -> &str {
        self.transaction
            .signatures
            .first()
            .map(String::as_str)
            .unwrap_or("")
    }

    /// Accounts that signed the transaction: the first
    /// `header.numRequiredSignatures` static keys (at least the fee payer).
    /// Pool PDAs never sign, so this separates users from vault authorities.
    pub fn signers(&self) -> &[String] {
        let n = self
            .transaction
            .message
            .header
            .as_ref()
            .and_then(|h| h.get("numRequiredSignatures"))
            .and_then(Value::as_u64)
            .unwrap_or(1)
            .max(1) as usize;
        let keys = &self.transaction.message.account_keys;
        &keys[..n.min(keys.len())]
    }

    pub fn is_signer(&self, pubkey: &str) -> bool {
        self.signers().iter().any(|s| s == pubkey)
    }

    /// Static keys followed by lookup-table loaded keys (writable, then
    /// readonly) — the order instruction account indices refer to.
    pub fn account_keys(&self) -> Vec<&str> {
        let mut keys: Vec<&str> = self
            .transaction
            .message
            .account_keys
            .iter()
            .map(String::as_str)
            .collect();
        if let Some(loaded) = &self.meta.loaded_addresses {
            keys.extend(loaded.writable.iter().map(String::as_str));
            keys.extend(loaded.readonly.iter().map(String::as_str));
        }
        keys
    }

    /// Outer instructions each followed by their inner instructions.
    pub fn flatten_instructions<'a>(&'a self, keys: &[&'a str]) -> Vec<FlatIx<'a>> {
        let mut out = Vec::new();
        let inners = self.meta.inner_instructions.as_deref().unwrap_or(&[]);
        let mut n: u16 = 0;
        let mut push = |ix: &'a Instruction, default_height: u32| {
            let program = keys.get(ix.program_id_index).copied().unwrap_or("");
            let accounts = ix
                .accounts
                .iter()
                .map(|&i| keys.get(i).copied().unwrap_or(""))
                .collect();
            let data = bs58::decode(&ix.data).into_vec().unwrap_or_default();
            out.push(FlatIx {
                ix_index: n,
                stack_height: ix.stack_height.unwrap_or(default_height),
                program,
                accounts,
                data,
            });
            n = n.saturating_add(1);
        };
        for (i, outer) in self.transaction.message.instructions.iter().enumerate() {
            push(outer, 1);
            for group in inners.iter().filter(|g| g.index == i) {
                for inner in &group.instructions {
                    push(inner, 2);
                }
            }
        }
        out
    }

    /// Per-token-account balance change, including accounts that only exist
    /// on one side (created or closed during the tx).
    pub fn token_deltas(&self, keys: &[&str]) -> Vec<TokenDelta> {
        let pre = self.meta.pre_token_balances.as_deref().unwrap_or(&[]);
        let post = self.meta.post_token_balances.as_deref().unwrap_or(&[]);
        let amount = |b: &TokenBalance| b.ui_token_amount.amount.parse::<i128>().unwrap_or(0);
        let mk = |b: &TokenBalance, delta: i128| TokenDelta {
            account: keys.get(b.account_index).unwrap_or(&"").to_string(),
            owner: b.owner.clone().unwrap_or_default(),
            mint: b.mint.clone(),
            decimals: b.ui_token_amount.decimals,
            delta,
        };
        let mut out: Vec<TokenDelta> = Vec::new();
        for b in post {
            let before = pre
                .iter()
                .find(|p| p.account_index == b.account_index)
                .map(amount)
                .unwrap_or(0);
            out.push(mk(b, amount(b) - before));
        }
        for b in pre {
            if !post.iter().any(|p| p.account_index == b.account_index) {
                out.push(mk(b, -amount(b)));
            }
        }
        out
    }

    /// Token account pubkey → (owner, mint, decimals) for every token account
    /// present in pre or post balances. Accounts created *and* closed within
    /// the transaction are absent.
    pub fn token_accounts<'a>(&'a self, keys: &[&'a str]) -> std::collections::HashMap<&'a str, TokenAccountInfo<'a>> {
        let pre = self.meta.pre_token_balances.as_deref().unwrap_or(&[]);
        let post = self.meta.post_token_balances.as_deref().unwrap_or(&[]);
        let mut map = std::collections::HashMap::new();
        for b in pre.iter().chain(post.iter()) {
            if let (Some(&key), Some(owner)) = (keys.get(b.account_index), b.owner.as_deref()) {
                map.insert(
                    key,
                    TokenAccountInfo {
                        owner,
                        mint: b.mint.as_str(),
                        decimals: b.ui_token_amount.decimals,
                    },
                );
            }
        }
        map
    }

    /// Decimals of `mint` as reported by the node in the token balance
    /// metadata (which the node reads from the mint account).
    pub fn mint_decimals(&self, mint: &str) -> Option<u8> {
        let pre = self.meta.pre_token_balances.as_deref().unwrap_or(&[]);
        let post = self.meta.post_token_balances.as_deref().unwrap_or(&[]);
        post.iter()
            .chain(pre.iter())
            .find(|b| b.mint == mint)
            .map(|b| b.ui_token_amount.decimals)
    }
}
