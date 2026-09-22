// Mirrors src-tauri/src/dto.rs. Amounts are decimal strings (BigInt-safe).

export type Side = "buy" | "sell";
export type Venue =
  | "pumpfun_curve"
  | "pumpswap_amm"
  | "jupiter"
  | "raydium_amm_v4"
  | "raydium_cpmm"
  | "raydium_clmm"
  | "meteora_dlmm"
  | "meteora_damm_v2"
  | "orca_whirlpool";

export interface SwapRow {
  signature: string;
  ix_index: number;
  slot: number;
  block_time: number | null;
  wallet: string;
  mint: string;
  venue: Venue;
  side: Side;
  token_amount: string;
  token_decimals: number;
  sol_amount: string;
  fee_lamports: string;
}

export interface UnknownRow {
  signature: string;
  slot: number;
  block_time: number | null;
  wallet: string;
  programs: string[];
}

export interface PositionRow {
  mint: string;
  token_decimals: number;
  qty: string;
  cost_lamports: string;
  value_lamports: string;
  realized_lamports: string;
  unrealized_lamports: string;
  buys: number;
  sells: number;
  hold_secs: number | null;
}

export interface StatsRow {
  trades: number;
  buys: number;
  sells: number;
  tokens_traded: number;
  tokens_with_exits: number;
  wins: number;
  losses: number;
  win_rate_bp: number | null;
  median_hold_secs: number | null;
  median_buy_lamports: string | null;
  max_buy_lamports: string;
  bought_lamports: string;
  sold_lamports: string;
  realized_24h: string;
  realized_7d: string;
  realized_30d: string;
  oversold_events: number;
}

export interface SimRow {
  describe: string;
  copied_buys: number;
  copied_sells: number;
  skipped_small_buys: number;
  deployed_lamports: string;
  returned_lamports: string;
  realized_lamports: string;
  unrealized_lamports: string;
  open_positions: number;
  wins: number;
  losses: number;
  win_rate_bp: number | null;
  adverse_entries: number;
  missed_exits: number;
  stopped_out: number;
  took_profit: number;
}

export interface WalletRow {
  wallet: string;
  label: string | null;
  last_trade: SwapRow | null;
  realized_lamports: string;
  unrealized_lamports: string;
  trade_count: number;
  stats: StatsRow;
  sim: SimRow | null;
  open_positions: PositionRow[];
  closed_positions: PositionRow[];
}

export interface CandidateRow {
  rank: number;
  wallet: string;
  signature: string;
  slot: number;
  block_time: number | null;
  slots_after_launch: number;
  venue: Venue;
  sol_amount: string;
  token_amount: string;
  token_decimals: number;
  extra_buys: number;
  sold_in_window: boolean;
  tracked: boolean;
}

export interface DiscoverResult {
  mint: string;
  candidates: CandidateRow[];
  reached_launch: boolean;
  scanned: number;
  launch_slot: number;
  launch_time: number | null;
  max_pages: number;
}

export interface DiscoverProgress {
  phase: "paging" | "decoding" | "done";
  done: number;
  total: number;
}

export interface TokenMeta {
  mint: string;
  symbol: string;
  name: string;
  uri: string;
  image: string;
  /** local cache file (absolute path) when downloaded; use convertFileSrc */
  local: string | null;
}

export type AlertKind = "buy" | "big_buy" | "sell" | "confluence" | "migrated" | "progress" | "mcap";

export interface AlertRow {
  id: number;
  kind: AlertKind;
  at: number;
  mint: string;
  wallet: string | null;
  label: string | null;
  swap: SwapRow | null;
  median_buy_lamports: string | null;
  note: string | null;
}

export interface TokenStateRow {
  fetched_at: number;
  is_pump: boolean;
  complete: boolean;
  progress_bp: number;
  mcap_lamports: string | null;
  real_sol_lamports: string | null;
  price_lamports: string | null;
  price_token_units: string | null;
  pool: string | null;
  creator: string | null;
}

export interface TokenRow {
  mint: string;
  token_decimals: number;
  buyers: string[];
  sellers: string[];
  buys: number;
  sells: number;
  bought_lamports: string;
  sold_lamports: string;
  net_flow_lamports: string;
  first_trade: number | null;
  last_trade: number | null;
  last_venue: Venue | null;
  last_price_lamports: string | null;
  last_price_token_units: string | null;
  holders: number;
  held_qty: string;
  held_value_lamports: string;
  realized_lamports: string;
  unrealized_lamports: string;
  state: TokenStateRow | null;
  watched: boolean;
}

export interface WatchPoint {
  t: number;
  price_lamports: string | null;
  price_token_units: string | null;
  mcap_lamports: string | null;
  progress_bp: number;
  complete: boolean;
  is_pump: boolean;
}

export interface WatchRow {
  mint: string;
  points: WatchPoint[];
  latest: WatchPoint | null;
  creator: string | null;
}

export interface LaunchRow {
  mint: string;
  name: string;
  symbol: string;
  uri: string;
  creator: string;
  creator_tracked: boolean;
  signature: string;
  slot: number;
  block_time: number | null;
  dev_buy_lamports: string;
  dev_buy_tokens: string;
  token_total_supply: string;
  initial_mcap_lamports: string;
  watched: boolean;
}

export type LaunchState =
  | { state: "connecting" }
  | { state: "live" }
  | { state: "disconnected"; reason: string; retry_in_ms: number };

export type ConnState =
  | { state: "idle" }
  | { state: "connecting" }
  | { state: "subscribed"; wallets: number }
  | { state: "backfilling"; wallet: string; pending: number }
  | { state: "live" }
  | { state: "disconnected"; reason: string; retry_in_ms: number };

export interface Snapshot {
  wallets: WalletRow[];
  feed: SwapRow[];
  unknown: UnknownRow[];
  alerts: AlertRow[];
  meta: TokenMeta[];
  status: ConnState;
  config_path: string;
  config_created: boolean;
  config_error: string | null;
  rpc_url: string;
  fetch_failures: number;
  alerts_enabled: boolean;
  sound: boolean;
  muted: boolean;
  sim_enabled: boolean;
  sim_describe: string;
  tokens_window_hours: number;
  confluence_wallets: number;
  confluence_minutes: number;
  watch: WatchRow[];
  launches: LaunchRow[];
  launches_enabled: boolean;
  launch_state: LaunchState;
  chart_url: string;
}

export type Scale = "compact" | "normal" | "large";
