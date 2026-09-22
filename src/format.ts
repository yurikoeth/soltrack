// Display-unit conversion happens here and only here. Everything upstream
// is raw lamports / raw token units as decimal strings.

import type { Scale, TokenMeta } from "./types";

const LAMPORTS_PER_SOL = 1_000_000_000n;

/** Lamports (possibly negative, as string) → "±1.2345" SOL. */
export function fmtSol(lamports: string, opts: { sign?: boolean; digits?: number } = {}): string {
  const v = BigInt(lamports);
  const neg = v < 0n;
  const abs = neg ? -v : v;
  const whole = abs / LAMPORTS_PER_SOL;
  const frac = abs % LAMPORTS_PER_SOL;
  // small amounts get more precision so they don't render as 0.0000
  const digits = opts.digits ?? (abs < 10_000_000n ? 6 : abs < 100_000_000_000n ? 4 : 2);
  const fracStr = frac.toString().padStart(9, "0").slice(0, digits);
  const body = digits > 0 ? `${whole}.${fracStr}` : `${whole}`;
  const sign = neg ? "−" : opts.sign && v > 0n ? "+" : "";
  return `${sign}${body}`;
}

/** Raw token units → compact "1.23M" using the mint's decimals. */
export function fmtTokens(raw: string, decimals: number): string {
  const v = BigInt(raw);
  const scale = 10n ** BigInt(decimals);
  const whole = v / scale;
  const frac = v % scale;
  const abs = whole < 0n ? -whole : whole;
  if (abs >= 1_000_000_000n) return `${trim(abs, 1_000_000_000n)}B`;
  if (abs >= 1_000_000n) return `${trim(abs, 1_000_000n)}M`;
  if (abs >= 10_000n) return `${trim(abs, 1_000n)}K`;
  if (abs >= 100n || decimals === 0) return abs.toString();
  const fracStr = frac.toString().padStart(decimals, "0").slice(0, 3).replace(/0+$/, "");
  return fracStr ? `${abs}.${fracStr}` : abs.toString();
}

function trim(v: bigint, unit: bigint): string {
  const whole = v / unit;
  const tenth = ((v % unit) * 10n) / unit;
  return tenth === 0n || whole >= 100n ? whole.toString() : `${whole}.${tenth}`;
}

/** unrealized / cost as a percentage string, or "" when cost is 0. */
export function fmtPct(part: string, whole: string): string {
  const p = BigInt(part);
  const w = BigInt(whole);
  if (w === 0n) return "";
  const bp = (p * 10_000n) / w; // basis points
  const neg = bp < 0n;
  const abs = neg ? -bp : bp;
  const int = abs / 100n;
  const dec = (abs % 100n).toString().padStart(2, "0").slice(0, 1);
  return `${neg ? "−" : "+"}${int}.${dec}%`;
}

/** basis points → "62%" */
export function fmtBp(bp: number | null): string {
  if (bp === null) return "—";
  return `${Math.round(bp / 100)}%`;
}

/** seconds → "14m", "2.5h", "3d" */
export function fmtDuration(secs: number | null): string {
  if (secs === null) return "—";
  if (secs < 60) return `${secs}s`;
  if (secs < 3600) return `${Math.round(secs / 60)}m`;
  if (secs < 86400) {
    const h = secs / 3600;
    return h < 10 ? `${h.toFixed(1)}h` : `${Math.round(h)}h`;
  }
  return `${(secs / 86400).toFixed(1)}d`;
}

/** Short beep via Web Audio; no asset needed. */
export function beep(kind: "buy" | "big_buy" | "sell") {
  try {
    const ctx = new AudioContext();
    const notes = kind === "big_buy" ? [660, 990] : kind === "sell" ? [440] : [660];
    notes.forEach((f, i) => {
      const o = ctx.createOscillator();
      const g = ctx.createGain();
      o.type = "sine";
      o.frequency.value = f;
      o.connect(g);
      g.connect(ctx.destination);
      const t = ctx.currentTime + i * 0.14;
      g.gain.setValueAtTime(0.0001, t);
      g.gain.exponentialRampToValueAtTime(0.18, t + 0.01);
      g.gain.exponentialRampToValueAtTime(0.0001, t + 0.22);
      o.start(t);
      o.stop(t + 0.24);
    });
    window.setTimeout(() => ctx.close(), 800);
  } catch {
    /* audio unavailable */
  }
}

export function short(addr: string, n = 4): string {
  if (addr.length <= n * 2 + 1) return addr;
  return `${addr.slice(0, n)}…${addr.slice(-n)}`;
}

export function ago(unixSecs: number | null, nowMs: number): string {
  if (unixSecs === null) return "";
  const s = Math.max(0, Math.floor(nowMs / 1000) - unixSecs);
  if (s < 60) return `${s}s`;
  if (s < 3600) return `${Math.floor(s / 60)}m`;
  if (s < 86400) return `${Math.floor(s / 3600)}h`;
  return `${Math.floor(s / 86400)}d`;
}

export function signClass(lamports: string): string {
  const v = BigInt(lamports);
  return v > 0n ? "text-up" : v < 0n ? "text-down" : "text-dim";
}

export const venueLabel: Record<string, string> = {
  pumpfun_curve: "pump.fun",
  pumpswap_amm: "PumpSwap",
  jupiter: "Jupiter",
  raydium_amm_v4: "Raydium v4",
  raydium_cpmm: "Raydium CPMM",
  raydium_clmm: "Raydium CLMM",
  meteora_dlmm: "Meteora DLMM",
  meteora_damm_v2: "Meteora DAMM",
  orca_whirlpool: "Orca",
};

/** Symbol for a mint if known, else the shortened mint. */
export function sym(mint: string, meta: Record<string, TokenMeta>): string {
  const m = meta[mint];
  if (m?.symbol) return m.symbol.length > 12 ? m.symbol.slice(0, 12) : m.symbol;
  if (m?.name) return m.name.length > 12 ? m.name.slice(0, 12) : m.name;
  return short(mint, 4);
}

export function tokenTitle(mint: string, meta: Record<string, TokenMeta>): string {
  const m = meta[mint];
  return m?.name ? `${m.name} (${m.symbol})\n${mint}` : mint;
}

/** Well-known program ids, for the "unknown" tab. */
export const programNames: Record<string, string> = {
  JUP6LkbZbjS1jKKwapdHNy74zcZ3tLUZoi5QNyVTaV4: "Jupiter v6",
  JUP4Fb2cqiRUcaTHdrPC8h2gNsA2ETXiPDD33WcGuJB: "Jupiter v4",
  "675kPX9MHTjS2zt1qfr1NYHuzeLXfQM9H24wFSUt1Mp8": "Raydium AMM v4",
  CPMMoo8L3F4NbTegBCKVNunggL7H1ZpdTHKxQB5qKP1C: "Raydium CPMM",
  CAMMCzo5YL8w4VFF8KVHrK22GGUsp5VTaW7grrKgrWqK: "Raydium CLMM",
  LBUZKhRxPF3XUpBCjp4YzTKgLccjZhTSDM9YuVaPwxo: "Meteora DLMM",
  Eo7WjKq67rjJQSZxS6z3YkapzY3eMj6Xy8X5EQVn5UaB: "Meteora Pools",
  cpamdpZCGKUy5JxQXB4dcpGPiikHawvSWAd6mEn1sGG: "Meteora DAMM v2",
  whirLbMiicVdio4qvUfM5KAg6Ct8VwpYzGff3uctyCc: "Orca Whirlpool",
  "6m2CDdhRgxpH4WjvdzxAYbGxwdGUz5MziiL5jek2kBma": "OKX DEX",
  "6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P": "pump.fun",
  pAMMBay6oceH9fJKBRHGP5D4bD4sWpmSwMn52FMfXEA: "PumpSwap",
  "6AxebMWZ6HbFQPXgNsHyVvVtb9vSp6HfWPdU6v3xzVEG": "Photon",
  BSfD6SHZigAfDWSjzD5Q41jw8LmKwtmjskPH9XW1mrRW: "Trojan",
  AxiomfHaWDemCFBLBayqnEnNwE6b7B2Qz3UmzMpgbMG6: "Axiom",
  b1oAdvancedFinTech1111111111111111111111111: "BloomBot",
  MaestroAAe9ge5HTc64VbBQZ6fP77pwvrhM8i1XWSAx: "Maestro",
};

export function programName(id: string): string {
  return programNames[id] ?? short(id, 4);
}

export const scalePx: Record<Scale, string> = {
  compact: "11px",
  normal: "13px",
  large: "15px",
};

/** seconds → "+4s", "+2m 10s" (for time-since-launch) */
export function fmtOffset(secs: number | null): string {
  if (secs === null) return "—";
  if (secs < 60) return `+${secs}s`;
  return `+${Math.floor(secs / 60)}m ${secs % 60}s`;
}

export const pumpFunUrl = (mint: string) => `https://pump.fun/coin/${mint}`;
export const chartUrl = (template: string, mint: string) => template.replace("{mint}", mint);

export const solscanTx = (sig: string) => `https://solscan.io/tx/${sig}`;
export const solscanAccount = (addr: string) => `https://solscan.io/account/${addr}`;
export const solscanToken = (mint: string) => `https://solscan.io/token/${mint}`;
