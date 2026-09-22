import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { convertFileSrc, invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { openUrl } from "@tauri-apps/plugin-opener";
import pumpfunIcon from "./assets/pumpfun.png";
import type {
  AlertRow,
  CandidateRow,
  ConnState,
  DiscoverProgress,
  DiscoverResult,
  LaunchRow,
  LaunchState,
  PositionRow,
  Scale,
  SimRow,
  Snapshot,
  StatsRow,
  SwapRow,
  TokenMeta,
  TokenRow,
  TokenStateRow,
  UnknownRow,
  WalletRow,
  WatchPoint,
  WatchRow,
} from "./types";
import {
  ago,
  beep,
  chartUrl,
  fmtBp,
  fmtDuration,
  fmtOffset,
  fmtPct,
  fmtSol,
  fmtTokens,
  programName,
  pumpFunUrl,
  scalePx,
  short,
  signClass,
  solscanAccount,
  solscanToken,
  solscanTx,
  sym,
  tokenTitle,
  venueLabel,
} from "./format";

const FEED_MAX = 200;

/** Newest first by block time; duplicates (same signature + ix) are dropped. */
function insertSwap(feed: SwapRow[], row: SwapRow): SwapRow[] {
  if (feed.some((r) => r.signature === row.signature && r.ix_index === row.ix_index)) return feed;
  const t = row.block_time ?? Number.MAX_SAFE_INTEGER;
  const idx = feed.findIndex((r) => (r.block_time ?? Number.MAX_SAFE_INTEGER) <= t);
  const next = idx === -1 ? [...feed, row] : [...feed.slice(0, idx), row, ...feed.slice(idx)];
  return next.slice(0, FEED_MAX);
}
const SCALE_KEY = "soltrack.scale";
type Tab = "feed" | "alerts" | "tokens" | "watch" | "launches" | "discover" | "unknown";

function loadScale(): Scale {
  try {
    const v = localStorage.getItem(SCALE_KEY);
    if (v === "compact" || v === "normal" || v === "large") return v;
  } catch {
    /* storage unavailable */
  }
  return "normal";
}

export default function App() {
  const [snap, setSnap] = useState<Snapshot | null>(null);
  const [snapError, setSnapError] = useState<string | null>(null);
  const [wallets, setWallets] = useState<Record<string, WalletRow>>({});
  const [order, setOrder] = useState<string[]>([]);
  const [feed, setFeed] = useState<SwapRow[]>([]);
  const [alerts, setAlerts] = useState<AlertRow[]>([]);
  const [unknown, setUnknown] = useState<UnknownRow[]>([]);
  const [meta, setMeta] = useState<Record<string, TokenMeta>>({});
  const [status, setStatus] = useState<ConnState>({ state: "connecting" });
  const [now, setNow] = useState(Date.now());
  const [pinned, setPinned] = useState(true);
  const [muted, setMuted] = useState(false);
  const [tab, setTab] = useState<Tab>("feed");
  const [expanded, setExpanded] = useState<Set<string>>(new Set());
  const [scale, setScale] = useState<Scale>(loadScale);
  const [flashKey, setFlashKey] = useState<string | null>(null);
  const [toast, setToast] = useState<{ text: string; kind?: AlertRow["kind"] } | null>(null);
  const [unseenAlerts, setUnseenAlerts] = useState(0);
  // tokens tab
  const [tokens, setTokens] = useState<TokenRow[] | null>(null);
  const [tokensDirty, setTokensDirty] = useState(true);
  const [tokenStates, setTokenStates] = useState<Record<string, TokenStateRow>>({});
  // watch tab
  const [watch, setWatch] = useState<WatchRow[]>([]);
  const [watchInput, setWatchInput] = useState("");
  // launches tab
  const [launches, setLaunches] = useState<LaunchRow[]>([]);
  const [launchState, setLaunchState] = useState<LaunchState>({ state: "connecting" });
  const [unseenLaunches, setUnseenLaunches] = useState(0);
  // discover tab
  const [mintInput, setMintInput] = useState("");
  const [discovering, setDiscovering] = useState(false);
  const [progress, setProgress] = useState<DiscoverProgress | null>(null);
  const [candidates, setCandidates] = useState<CandidateRow[] | null>(null);
  const [discovery, setDiscovery] = useState<DiscoverResult | null>(null);
  const [discoverError, setDiscoverError] = useState<string | null>(null);
  const [discoveredMint, setDiscoveredMint] = useState<string | null>(null);

  const timers = useRef<{ flash?: number; toast?: number }>({});
  // refs so the event listeners (registered once) see current values
  const mutedRef = useRef(false);
  const soundRef = useRef(true);
  const tabRef = useRef<Tab>("feed");
  const labelsRef = useRef<Record<string, string>>({});
  const metaRef = useRef<Record<string, TokenMeta>>({});
  mutedRef.current = muted;
  tabRef.current = tab;
  metaRef.current = meta;

  useEffect(() => {
    document.documentElement.style.fontSize = scalePx[scale];
    try {
      localStorage.setItem(SCALE_KEY, scale);
    } catch {
      /* ignore */
    }
  }, [scale]);

  function applyWalletRows(rows: WalletRow[]) {
    setOrder(rows.map((w) => w.wallet));
    setWallets(Object.fromEntries(rows.map((w) => [w.wallet, w])));
    labelsRef.current = Object.fromEntries(rows.filter((w) => w.label).map((w) => [w.wallet, w.label as string]));
  }

  const feedRef = useRef<SwapRow[]>([]);
  useEffect(() => {
    feedRef.current = feed;
  }, [feed]);

  useEffect(() => {
    let alive = true;
    invoke<Snapshot>("snapshot").then((s) => {
      if (!alive) return;
      setSnap(s);
      applyWalletRows(s.wallets);
      setFeed(s.feed);
      setAlerts(s.alerts);
      setUnknown(s.unknown);
      setMeta(Object.fromEntries(s.meta.map((m) => [m.mint, m])));
      setStatus(s.status);
      setMuted(s.muted);
      setWatch(s.watch);
      setLaunches(s.launches);
      setLaunchState(s.launch_state);
      soundRef.current = s.sound;
    }).catch((e) => {
      if (alive) setSnapError(String(e));
    });
    const unlisten = [
      listen<SwapRow>("swap", (e) => {
        const row = e.payload;
        const head = feedRef.current[0];
        const newest = !head || (row.block_time ?? Infinity) >= (head.block_time ?? 0);
        setFeed((f) => insertSwap(f, row));
        if (!newest) return;
        setFlashKey(`${row.signature}:${row.ix_index}`);
        window.clearTimeout(timers.current.flash);
        timers.current.flash = window.setTimeout(() => setFlashKey(null), 1500);
      }),
      listen<WalletRow>("wallet", (e) => setWallets((w) => ({ ...w, [e.payload.wallet]: e.payload }))),
      listen<WalletRow[]>("wallets", (e) => applyWalletRows(e.payload)),
      listen<UnknownRow>("unknown", (e) => setUnknown((u) => [e.payload, ...u].slice(0, 50))),
      listen<TokenMeta>("meta", (e) => setMeta((m) => ({ ...m, [e.payload.mint]: e.payload }))),
      listen<ConnState>("status", (e) => setStatus(e.payload)),
      listen<DiscoverProgress>("discover_progress", (e) => setProgress(e.payload)),
      listen<void>("tokens_changed", () => setTokensDirty(true)),
      listen<[string, TokenStateRow]>("token_state", (e) => {
        const [mint, st] = e.payload;
        setTokenStates((m) => ({ ...m, [mint]: st }));
      }),
      listen<[string, WatchPoint]>("watch_point", (e) => {
        const [mint, p] = e.payload;
        setWatch((rows) =>
          rows.map((r) => (r.mint === mint ? { ...r, points: [...r.points, p].slice(-240), latest: p } : r)),
        );
      }),
      listen<WatchRow[]>("watch", (e) => setWatch(e.payload)),
      listen<LaunchRow>("launch", (e) => {
        setLaunches((l) => [e.payload, ...l].slice(0, 200));
        if (tabRef.current !== "launches") setUnseenLaunches((n) => Math.min(n + 1, 99));
      }),
      listen<LaunchState>("launch_state", (e) => setLaunchState(e.payload)),
      listen<AlertRow>("alert", (e) => {
        const a = e.payload;
        setAlerts((list) => [a, ...list].slice(0, 100));
        if (tabRef.current !== "alerts") setUnseenAlerts((n) => n + 1);
        if (!mutedRef.current && soundRef.current) beep(a.kind === "confluence" || a.kind === "migrated" ? "big_buy" : a.kind === "sell" ? "sell" : "buy");
        const token = sym(a.mint, metaRef.current);
        const who = a.label ?? (a.wallet ? labelsRef.current[a.wallet] ?? short(a.wallet) : "");
        const text =
          a.kind === "confluence"
            ? `CONFLUENCE ${token} · ${a.note ?? ""}`
            : a.kind === "migrated" || a.kind === "progress" || a.kind === "mcap"
              ? `${token}: ${a.note ?? a.kind}`
              : `${who} ${a.kind === "big_buy" ? "BIG BUY" : a.kind === "sell" ? "sold" : "bought"} ${token}${a.swap ? ` · ${fmtSol(a.swap.sol_amount)}◎` : ""}`;
        showToast(text, a.kind);
      }),
    ];
    const tick = window.setInterval(() => setNow(Date.now()), 5000);
    return () => {
      alive = false;
      window.clearInterval(tick);
      unlisten.forEach((p) => p.then((f) => f()));
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  // tokens tab: refresh when opened, when swaps land, and every 60 s while visible.
  // Rows without on-chain state ask the refresher for it (throttled server-side).
  const [tokensError, setTokensError] = useState<string | null>(null);
  const loadTokens = useCallback(async () => {
    try {
      const rows = await invoke<TokenRow[]>("tokens");
      setTokens(rows);
      setTokensError(null);
      setTokensDirty(false);
      rows
        .filter((r) => !r.state)
        .slice(0, 40)
        .forEach((r) => invoke("refresh_token", { mint: r.mint }).catch(() => {}));
    } catch (e) {
      setTokensError(String(e));
    }
  }, []);
  useEffect(() => {
    if (tab !== "tokens") return;
    if (tokensDirty || tokens === null) loadTokens();
    const id = window.setInterval(loadTokens, 60_000);
    return () => window.clearInterval(id);
  }, [tab, tokensDirty, tokens === null, loadTokens]);

  const rows = useMemo(() => order.map((w) => wallets[w]).filter(Boolean), [order, wallets]);
  const totals = useMemo(() => {
    let r = 0n;
    let u = 0n;
    let s = 0n;
    for (const w of rows) {
      r += BigInt(w.realized_lamports);
      u += BigInt(w.unrealized_lamports);
      if (w.sim) s += BigInt(w.sim.realized_lamports);
    }
    return { realized: r.toString(), unrealized: u.toString(), sim: s.toString() };
  }, [rows]);

  const win = getCurrentWindow();
  const simOn = !!snap?.sim_enabled;

  async function togglePin() {
    const next = !pinned;
    await win.setAlwaysOnTop(next);
    setPinned(next);
  }

  async function toggleMute() {
    const next = await invoke<boolean>("set_muted", { muted: !muted });
    setMuted(next);
    showToast(next ? "alerts muted" : "alerts on");
  }

  function cycleScale() {
    setScale((s) => (s === "compact" ? "normal" : s === "normal" ? "large" : "compact"));
  }

  function toggleExpanded(w: string) {
    setExpanded((set) => {
      const next = new Set(set);
      if (next.has(w)) next.delete(w);
      else next.add(w);
      return next;
    });
  }

  function switchTab(t: Tab) {
    setTab(t);
    if (t === "alerts") setUnseenAlerts(0);
    if (t === "launches") setUnseenLaunches(0);
  }

  async function copy(text: string, what: string) {
    try {
      await navigator.clipboard.writeText(text);
      showToast(`${what} copied`);
    } catch {
      showToast("copy failed");
    }
  }

  function showToast(text: string, kind?: AlertRow["kind"]) {
    setToast({ text, kind });
    window.clearTimeout(timers.current.toast);
    timers.current.toast = window.setTimeout(() => setToast(null), kind ? 3500 : 1400);
  }

  async function runDiscover(mintArg?: string) {
    const mint = (mintArg ?? mintInput).trim();
    if (!mint || discovering) return;
    setMintInput(mint);
    setTab("discover");
    setDiscovering(true);
    setDiscoverError(null);
    setCandidates(null);
    setProgress({ phase: "paging", done: 0, total: 0 });
    try {
      const found = await invoke<DiscoverResult>("discover", { mint });
      setDiscovery(found);
      setCandidates(found.candidates);
      setDiscoveredMint(mint);
    } catch (e) {
      setDiscoverError(String(e));
    } finally {
      setDiscovering(false);
      setProgress(null);
    }
  }

  async function track(address: string, label: string | null) {
    try {
      const rows = await invoke<WalletRow[]>("track_wallet", { address, label });
      applyWalletRows(rows);
      setCandidates((c) => c?.map((x) => (x.wallet === address ? { ...x, tracked: true } : x)) ?? null);
      setLaunches((l) => l.map((x) => (x.creator === address ? { ...x, creator_tracked: true } : x)));
      showToast(`tracking ${label ?? short(address)}`);
    } catch (e) {
      showToast(`track failed: ${String(e)}`);
    }
  }

  async function untrack(address: string) {
    if (!window.confirm(`Stop tracking ${walletName(address)}?\n\nIts history stays in the database.`)) return;
    try {
      const rows = await invoke<WalletRow[]>("untrack_wallet", { address });
      applyWalletRows(rows);
      setCandidates((c) => c?.map((x) => (x.wallet === address ? { ...x, tracked: false } : x)) ?? null);
      showToast("untracked");
    } catch (e) {
      showToast(`untrack failed: ${String(e)}`);
    }
  }

  async function watchMint(mint: string) {
    try {
      const rows = await invoke<WatchRow[]>("watch_mint", { mint });
      setWatch(rows);
      setTokens((t) => t?.map((x) => (x.mint === mint ? { ...x, watched: true } : x)) ?? null);
      setLaunches((l) => l.map((x) => (x.mint === mint ? { ...x, watched: true } : x)));
      showToast(`watching ${sym(mint, meta)}`);
    } catch (e) {
      showToast(`watch failed: ${String(e)}`);
    }
  }

  async function unwatchMint(mint: string) {
    try {
      const rows = await invoke<WatchRow[]>("unwatch_mint", { mint });
      setWatch(rows);
      setTokens((t) => t?.map((x) => (x.mint === mint ? { ...x, watched: false } : x)) ?? null);
      setLaunches((l) => l.map((x) => (x.mint === mint ? { ...x, watched: false } : x)));
    } catch (e) {
      showToast(`unwatch failed: ${String(e)}`);
    }
  }

  const walletName = (addr: string) => wallets[addr]?.label ?? short(addr);
  const watchedSet = useMemo(() => new Set(watch.map((w) => w.mint)), [watch]);

  return (
    <div className="relative flex h-full flex-col bg-bg">
      {/* ── title bar / drag region ─────────────────────────────────────── */}
      <div
        data-tauri-drag-region
        className="flex h-[2.2rem] shrink-0 items-center gap-2 border-b border-line bg-panel px-2.5"
      >
        <StatusDot status={status} />
        <span data-tauri-drag-region className="font-semibold tracking-wide text-fg">
          soltrack
        </span>
        <span data-tauri-drag-region className="truncate text-dim">
          {statusText(status, walletName)}
        </span>
        <div className="ml-auto flex items-center gap-0.5">
          {snap?.alerts_enabled && (
            <IconButton title={muted ? "Alerts muted — click to unmute" : "Alerts on — click to mute"} onClick={toggleMute} active={!muted}>
              {muted ? "🔕" : "🔔"}
            </IconButton>
          )}
          <IconButton title={`Text size: ${scale} (click to cycle)`} onClick={cycleScale}>
            <span className="text-[0.85em]">A</span>
            <span className="text-[1.15em]">A</span>
          </IconButton>
          <IconButton title={pinned ? "Unpin (allow other windows on top)" : "Pin on top"} onClick={togglePin} active={pinned}>
            ⬒
          </IconButton>
          <IconButton title="Minimize" onClick={() => win.minimize()}>
            –
          </IconButton>
          <IconButton title="Close" onClick={() => win.close()} danger>
            ✕
          </IconButton>
        </div>
      </div>

      {snapError !== null && (
        <Banner tone="error">
          Failed to load state from the app: <span className="selectable">{snapError}</span>. Check the log and restart.
        </Banner>
      )}
      {snap?.config_error && (
        <Banner tone="error">
          Config not loaded — <span className="selectable">{snap.config_error}</span>. Fix the file and restart.
        </Banner>
      )}
      {snap?.config_created && (
        <Banner>
          No config found. A template was written to <b className="selectable text-fg">{snap.config_path}</b>. Add wallets there
          or use the <button className="underline" onClick={() => switchTab("discover")}>discover</button> tab.
        </Banner>
      )}
      {snap && !snap.config_created && rows.length === 0 && (
        <Banner>
          No wallets tracked yet — paste a token mint in the{" "}
          <button className="underline" onClick={() => switchTab("discover")}>discover</button> tab to find its early
          buyers, or add addresses to <b className="selectable text-fg">{snap.config_path}</b>.
        </Banner>
      )}

      {/* ── wallet table ───────────────────────────────────────────────── */}
      <div className="shrink-0 overflow-x-auto border-b border-line">
        <table className="w-full border-collapse">
          <thead className="text-left text-[0.8rem] uppercase tracking-wider text-faint">
            <tr>
              <th className="px-2.5 py-1.5 font-medium">wallet</th>
              <th className="px-2 py-1.5 font-medium">last trade</th>
              <th className="px-2 py-1.5 text-right font-medium" title="tokens exited at a profit / with a loss">
                win
              </th>
              <th className="px-2 py-1.5 text-right font-medium">realized</th>
              <th className="px-2 py-1.5 text-right font-medium">unrealized</th>
              {simOn && (
                <th className="px-2 py-1.5 text-right font-medium" title={`if copied: ${snap?.sim_describe}`}>
                  copy
                </th>
              )}
              <th className="px-2.5 py-1.5 text-right font-medium">open</th>
            </tr>
          </thead>
          <tbody>
            {rows.map((w) => (
              <WalletRows
                key={w.wallet}
                w={w}
                open={expanded.has(w.wallet)}
                now={now}
                meta={meta}
                simOn={simOn}
                onToggle={() => toggleExpanded(w.wallet)}
                onCopy={copy}
                onUntrack={() => untrack(w.wallet)}
              />
            ))}
          </tbody>
          {rows.length > 1 && (
            <tfoot className="border-t border-line bg-panel/60 text-dim">
              <tr>
                <td className="px-2.5 py-1.5" colSpan={3}>
                  total <span className="text-faint">· {rows.reduce((n, w) => n + w.trade_count, 0)} trades</span>
                </td>
                <td className={`tnum px-2 py-1.5 text-right font-semibold ${signClass(totals.realized)}`}>
                  {fmtSol(totals.realized, { sign: true })}
                </td>
                <td className={`tnum px-2 py-1.5 text-right font-semibold ${signClass(totals.unrealized)}`}>
                  {fmtSol(totals.unrealized, { sign: true })}
                </td>
                {simOn && (
                  <td className={`tnum px-2 py-1.5 text-right font-semibold ${signClass(totals.sim)}`}>
                    {fmtSol(totals.sim, { sign: true })}
                  </td>
                )}
                <td className="tnum px-2.5 py-1.5 text-right">{rows.reduce((n, w) => n + w.open_positions.length, 0)}</td>
              </tr>
            </tfoot>
          )}
        </table>
      </div>

      {/* ── tabs ───────────────────────────────────────────────────────── */}
      <div className="flex shrink-0 items-center gap-3 border-b border-line bg-panel px-2.5 py-1 text-[0.8rem] uppercase tracking-wider text-faint">
        <TabButton active={tab === "feed"} onClick={() => switchTab("feed")}>
          swaps
        </TabButton>
        {snap?.alerts_enabled && (
          <TabButton active={tab === "alerts"} onClick={() => switchTab("alerts")}>
            alerts {unseenAlerts > 0 && <span className="text-accent">{unseenAlerts}</span>}
          </TabButton>
        )}
        <TabButton active={tab === "tokens"} onClick={() => switchTab("tokens")}>
          tokens
        </TabButton>
        <TabButton active={tab === "watch"} onClick={() => switchTab("watch")}>
          watch {watch.length > 0 && <span className="text-dim">{watch.length}</span>}
        </TabButton>
        {snap?.launches_enabled && (
          <TabButton active={tab === "launches"} onClick={() => switchTab("launches")}>
            launches {unseenLaunches > 0 && <span className="text-dim">{unseenLaunches}</span>}
          </TabButton>
        )}
        <TabButton active={tab === "discover"} onClick={() => switchTab("discover")}>
          discover
        </TabButton>
        <TabButton active={tab === "unknown"} onClick={() => switchTab("unknown")}>
          unknown {unknown.length > 0 && <span className="text-down">{unknown.length}</span>}
        </TabButton>
      </div>
      <div className="min-h-0 flex-1 overflow-y-auto">
        {tab === "feed" &&
          (feed.length === 0 ? (
            <Empty>waiting for swaps…</Empty>
          ) : (
            <table className="w-full border-collapse">
              <tbody>
                {feed.map((s) => {
                  const key = `${s.signature}:${s.ix_index}`;
                  const buy = s.side === "buy";
                  return (
                    <tr
                      key={key}
                      className={`group border-b border-line/50 hover:bg-panel ${key === flashKey ? "flash" : ""}`}
                    >
                      <td className="tnum w-[3rem] px-2.5 py-1 text-faint">{ago(s.block_time, now)}</td>
                      <td className="px-1 py-1 text-dim" title={s.wallet}>
                        {walletName(s.wallet)}
                      </td>
                      <td className={`px-1 py-1 font-semibold uppercase ${buy ? "text-up" : "text-down"}`}>{s.side}</td>
                      <td className="tnum px-1 py-1 text-right text-fg">{fmtTokens(s.token_amount, s.token_decimals)}</td>
                      <td className="whitespace-nowrap px-1 py-1">
                        <TokenIcon mint={s.mint} meta={meta} size="1.05rem" />{" "}
                        <LinkText title={tokenTitle(s.mint, meta)} onClick={() => openUrl(solscanToken(s.mint))}>
                          {sym(s.mint, meta)}
                        </LinkText>
                      </td>
                      <td className={`tnum px-1 py-1 text-right ${buy ? "text-dim" : "text-fg"}`}>
                        {buy ? "−" : "+"}
                        {fmtSol(s.sol_amount)} <span className="text-faint">◎</span>
                      </td>
                      <td className="px-1 py-1 text-right text-faint">{venueLabel[s.venue] ?? s.venue}</td>
                      <td className="w-[3.4rem] px-1.5 py-1 text-right">
                        <RowActions
                          onCopy={() => copy(s.signature, "signature")}
                          onOpen={() => openUrl(solscanTx(s.signature))}
                          title={s.signature}
                        />
                      </td>
                    </tr>
                  );
                })}
              </tbody>
            </table>
          ))}

        {tab === "alerts" &&
          (alerts.length === 0 ? (
            <Empty>
              no alerts yet — live buys above the minimum, confluence ({snap?.confluence_wallets} wallets in {snap?.confluence_minutes}m),
              and watchlist events show here{muted ? " (muted)" : ""}
            </Empty>
          ) : (
            <table className="w-full border-collapse">
              <tbody>
                {alerts.map((a) => (
                  <tr key={a.id} className="group border-b border-line/50 hover:bg-panel">
                    <td className="tnum w-[3rem] px-2.5 py-1 text-faint">{ago(a.at, now)}</td>
                    <td className="px-1 py-1">
                      <KindBadge kind={a.kind} />
                    </td>
                    <td className="px-1 py-1 text-fg" title={a.wallet ?? ""}>
                      {a.wallet ? a.label ?? walletName(a.wallet) : ""}
                    </td>
                    <td className="whitespace-nowrap px-1 py-1">
                      <TokenIcon mint={a.mint} meta={meta} size="1.05rem" />{" "}
                      <LinkText title={tokenTitle(a.mint, meta)} onClick={() => openUrl(solscanToken(a.mint))}>
                        {sym(a.mint, meta)}
                      </LinkText>
                    </td>
                    <td className="tnum px-1 py-1 text-right text-fg">
                      {a.swap ? (
                        <>
                          {fmtSol(a.swap.sol_amount)} <span className="text-faint">◎</span>
                        </>
                      ) : (
                        ""
                      )}
                    </td>
                    <td className="px-1 py-1 text-dim">{a.note ?? (a.median_buy_lamports ? `median ${fmtSol(a.median_buy_lamports)}` : "")}</td>
                    <td className="px-1 py-1 text-right text-faint">{a.swap ? venueLabel[a.swap.venue] ?? a.swap.venue : ""}</td>
                    <td className="w-[3.4rem] px-1.5 py-1 text-right">
                      {a.swap && (
                        <RowActions
                          onCopy={() => copy(a.swap!.signature, "signature")}
                          onOpen={() => openUrl(solscanTx(a.swap!.signature))}
                          title={a.swap.signature}
                        />
                      )}
                    </td>
                  </tr>
                ))}
              </tbody>
            </table>
          ))}

        {tab === "tokens" && (
          <TokensTab
            tokens={tokens}
            tokenStates={tokenStates}
            meta={meta}
            now={now}
            windowHours={snap?.tokens_window_hours ?? 24}
            walletName={walletName}
            watched={watchedSet}
            onWatch={watchMint}
            onUnwatch={unwatchMint}
            onDiscover={(m) => runDiscover(m)}
            onRefresh={loadTokens}
            error={tokensError}
            chart={snap?.chart_url ?? "https://dexscreener.com/solana/{mint}"}
          />
        )}

        {tab === "watch" && (
          <div className="flex h-full flex-col">
            <form
              className="flex shrink-0 items-center gap-2 border-b border-line/60 px-2.5 py-2"
              onSubmit={(e) => {
                e.preventDefault();
                const m = watchInput.trim();
                if (m) {
                  watchMint(m);
                  setWatchInput("");
                }
              }}
            >
              <input
                className="selectable min-w-0 flex-1 rounded border border-line bg-bg px-2 py-1 text-fg outline-none placeholder:text-faint focus:border-accent"
                placeholder="token mint to watch — price, mcap, curve progress every few seconds"
                value={watchInput}
                onChange={(e) => setWatchInput(e.target.value)}
                spellCheck={false}
              />
              <button type="submit" disabled={!watchInput.trim()} className="rounded bg-accent/20 px-2.5 py-1 text-fg hover:bg-accent/30 disabled:opacity-40">
                watch
              </button>
            </form>
            {watch.length === 0 ? (
              <Empty>nothing watched — add a mint above, or press watch on a token / launch row</Empty>
            ) : (
              <table className="w-full border-collapse">
                <thead className="text-left text-[0.75rem] uppercase tracking-wider text-faint">
                  <tr>
                    <th className="px-2.5 py-1 font-medium">token</th>
                    <th className="px-1 py-1 font-medium">mcap</th>
                    <th className="px-1 py-1 text-right font-medium">now</th>
                    <th className="px-1 py-1 text-right font-medium">Δ</th>
                    <th className="px-1 py-1 text-right font-medium">curve</th>
                    <th className="px-1 py-1 font-medium">creator</th>
                    <th className="px-2.5 py-1 text-right font-medium"></th>
                  </tr>
                </thead>
                <tbody>
                  {watch.map((w) => (
                    <WatchLine
                      key={w.mint}
                      w={w}
                      meta={meta}
                      now={now}
                      walletName={walletName}
                      onUnwatch={() => unwatchMint(w.mint)}
                      onDiscover={() => runDiscover(w.mint)}
                      onCopy={copy}
                      chart={snap?.chart_url ?? "https://dexscreener.com/solana/{mint}"}
                    />
                  ))}
                </tbody>
              </table>
            )}
          </div>
        )}

        {tab === "launches" && (
          <div className="flex h-full flex-col">
            <div className="flex shrink-0 items-center gap-2 border-b border-line/60 px-2.5 py-1 text-[0.8rem] text-faint">
              <span
                className={`inline-block h-[0.55rem] w-[0.55rem] rounded-full ${
                  launchState.state === "live" ? "bg-up" : launchState.state === "disconnected" ? "bg-down" : "bg-accent animate-pulse"
                }`}
              />
              {launchState.state === "live" && "pump.fun launch stream live"}
              {launchState.state === "connecting" && "connecting to launch stream…"}
              {launchState.state === "disconnected" && `launch stream down: ${launchState.reason}`}
              <span className="ml-auto">{launches.length} recent</span>
            </div>
            {launches.length === 0 ? (
              <Empty>waiting for new tokens… (pump.fun launches dozens a minute; each row is a fresh mint)</Empty>
            ) : (
              <table className="w-full border-collapse">
                <thead className="text-left text-[0.75rem] uppercase tracking-wider text-faint">
                  <tr>
                    <th className="w-[3rem] px-2.5 py-1 font-medium">age</th>
                    <th className="px-1 py-1 font-medium">token</th>
                    <th className="px-1 py-1 text-right font-medium" title="creator's buy in the launch transaction">
                      dev buy
                    </th>
                    <th className="px-1 py-1 text-right font-medium">mcap₀</th>
                    <th className="px-1 py-1 font-medium">creator</th>
                    <th className="px-2.5 py-1 text-right font-medium"></th>
                  </tr>
                </thead>
                <tbody>
                  {launches.map((l) => (
                    <tr key={l.mint} className="group border-b border-line/50 hover:bg-panel">
                      <td className="tnum px-2.5 py-1 text-faint">{ago(l.block_time, now)}</td>
                      <td className="whitespace-nowrap px-1 py-1" title={`${l.name}\n${l.mint}`}>
                        <TokenIcon mint={l.mint} meta={meta} />{" "}
                        <LinkText title={`${l.name}\n${l.mint}`} onClick={() => openUrl(solscanToken(l.mint))}>
                          {l.symbol || short(l.mint)}
                        </LinkText>{" "}
                        <span className="text-faint">{l.name.length > 24 ? l.name.slice(0, 24) + "…" : l.name}</span>
                      </td>
                      <td className={`tnum px-1 py-1 text-right ${BigInt(l.dev_buy_lamports) >= 1_000_000_000n ? "text-up" : "text-fg"}`}>
                        {BigInt(l.dev_buy_lamports) === 0n ? <span className="text-faint">none</span> : `${fmtSol(l.dev_buy_lamports)}◎`}
                      </td>
                      <td className="tnum px-1 py-1 text-right text-dim">{fmtSol(l.initial_mcap_lamports, { digits: 1 })}◎</td>
                      <td className="px-1 py-1 text-dim" title={l.creator}>
                        <LinkText title={l.creator} onClick={() => openUrl(solscanAccount(l.creator))}>
                          {walletName(l.creator)}
                        </LinkText>
                        {l.creator_tracked && <span className="ml-1 text-accent">tracked</span>}
                      </td>
                      <td className="whitespace-nowrap px-2.5 py-1 text-right">
                        <TokenLinks mint={l.mint} chart={snap?.chart_url ?? "https://dexscreener.com/solana/{mint}"} />
                        {l.watched || watchedSet.has(l.mint) ? (
                          <span className="text-faint">watching</span>
                        ) : (
                          <button className="rounded bg-accent/15 px-2 py-0.5 text-fg hover:bg-accent/25" onClick={() => watchMint(l.mint)}>
                            watch
                          </button>
                        )}
                        {!l.creator_tracked && (
                          <button
                            className="ml-1 rounded bg-up/15 px-2 py-0.5 text-up hover:bg-up/25"
                            title="Track the creator wallet"
                            onClick={() => {
                              const label = window.prompt("Label (optional)", `dev-${l.symbol || short(l.mint, 3)}`);
                              if (label === null) return;
                              track(l.creator, label.trim() || null);
                            }}
                          >
                            track dev
                          </button>
                        )}
                      </td>
                    </tr>
                  ))}
                </tbody>
              </table>
            )}
          </div>
        )}

        {tab === "discover" && (
          <div className="flex h-full flex-col">
            <form
              className="flex shrink-0 items-center gap-2 border-b border-line/60 px-2.5 py-2"
              onSubmit={(e) => {
                e.preventDefault();
                runDiscover();
              }}
            >
              <input
                className="selectable min-w-0 flex-1 rounded border border-line bg-bg px-2 py-1 text-fg outline-none placeholder:text-faint focus:border-accent"
                placeholder="token mint address — find who bought it first"
                value={mintInput}
                onChange={(e) => setMintInput(e.target.value)}
                spellCheck={false}
              />
              <button
                type="submit"
                disabled={discovering || !mintInput.trim()}
                className="rounded bg-accent/20 px-2.5 py-1 text-fg hover:bg-accent/30 disabled:opacity-40"
              >
                {discovering ? "searching…" : "find early buyers"}
              </button>
            </form>
            {progress && (
              <div className="shrink-0 px-2.5 py-1 text-dim">
                {progress.phase === "paging" && `walking the mint's history… page ${progress.done + 1}`}
                {progress.phase === "decoding" && `decoding launch window · ${progress.done}/${progress.total}`}
                {progress.phase === "done" && "ranking…"}
              </div>
            )}
            {discoverError && <div className="shrink-0 px-2.5 py-1 text-down">{discoverError}</div>}
            {discovery && !discovery.reached_launch && (
              <div className="shrink-0 border-b border-line/60 bg-down/10 px-2.5 py-1 text-down">
                Page budget ({discovery.max_pages}×1000 signatures) ran out before this mint's first transaction — these
                are the oldest buyers among the most recent {discovery.scanned} transactions, <b>not the launch</b>.
                Raise <span className="text-fg">discover.max_pages</span> or use a faster RPC.
              </div>
            )}
            {candidates && candidates.length === 0 && <Empty>no decodable buys in the launch window</Empty>}
            {candidates && candidates.length > 0 && (
              <div className="min-h-0 flex-1 overflow-y-auto">
                <div className="px-2.5 pt-1.5 text-[0.8rem] text-faint">
                  {candidates.length} early buyers of{" "}
                  <LinkText title={discoveredMint ?? ""} onClick={() => openUrl(solscanToken(discoveredMint ?? ""))}>
                    {discoveredMint ? sym(discoveredMint, meta) : ""}
                  </LinkText>{" "}
                  · {discovery?.scanned ?? 0} txs scanned{discovery?.reached_launch ? " · reached launch" : ""}
                </div>
                <table className="w-full border-collapse">
                  <thead className="text-left text-[0.75rem] uppercase tracking-wider text-faint">
                    <tr>
                      <th className="px-2.5 py-1 font-medium">#</th>
                      <th className="px-1 py-1 font-medium">wallet</th>
                      <th className="px-1 py-1 text-right font-medium" title="time after the mint's first transaction">
                        after launch
                      </th>
                      <th className="px-1 py-1 text-right font-medium">bought</th>
                      <th className="px-1 py-1 text-right font-medium">size</th>
                      <th className="px-1 py-1 font-medium">venue</th>
                      <th className="px-1 py-1 font-medium">in window</th>
                      <th className="px-2.5 py-1 text-right font-medium"></th>
                    </tr>
                  </thead>
                  <tbody>
                    {candidates.map((c) => {
                      const launchTime = discovery?.launch_time ?? candidates[0]?.block_time ?? null;
                      const offset = c.block_time !== null && launchTime !== null ? c.block_time - launchTime : null;
                      return (
                        <tr key={c.wallet} className="group border-b border-line/50 hover:bg-panel">
                          <td className="tnum px-2.5 py-1 text-faint">{c.rank}</td>
                          <td className="px-1 py-1 text-fg" title={c.wallet}>
                            <LinkText title={c.wallet} onClick={() => openUrl(solscanAccount(c.wallet))}>
                              {walletName(c.wallet)}
                            </LinkText>
                          </td>
                          <td className="tnum px-1 py-1 text-right text-dim" title={`${c.slots_after_launch} slots`}>
                            {offset !== null ? fmtOffset(offset) : `+${c.slots_after_launch} slots`}
                          </td>
                          <td className="tnum px-1 py-1 text-right text-fg">{fmtTokens(c.token_amount, c.token_decimals)}</td>
                          <td className="tnum px-1 py-1 text-right text-fg">
                            {fmtSol(c.sol_amount)} <span className="text-faint">◎</span>
                          </td>
                          <td className="px-1 py-1 text-faint">{venueLabel[c.venue] ?? c.venue}</td>
                          <td className="px-1 py-1 text-faint">
                            {c.extra_buys > 0 && <span className="text-up">+{c.extra_buys} buy{c.extra_buys > 1 ? "s" : ""} </span>}
                            {c.sold_in_window && <span className="text-down">sold</span>}
                          </td>
                          <td className="px-2.5 py-1 text-right">
                            {c.tracked ? (
                              <span className="text-faint">tracked</span>
                            ) : (
                              <button
                                className="rounded bg-up/15 px-2 py-0.5 text-up hover:bg-up/25"
                                onClick={() => {
                                  const label = window.prompt("Label (optional)", `early-${c.rank}`);
                                  if (label === null) return;
                                  track(c.wallet, label.trim() || null);
                                }}
                              >
                                track
                              </button>
                            )}
                          </td>
                        </tr>
                      );
                    })}
                  </tbody>
                </table>
              </div>
            )}
            {!candidates && !progress && !discoverError && (
              <Empty>
                paste a mint that ran (e.g. a token you missed). The earliest buyers are your copy-trading candidates —
                track a few and let their stats decide.
              </Empty>
            )}
          </div>
        )}

        {tab === "unknown" &&
          (unknown.length === 0 ? (
            <Empty>no unknown swaps — everything decoded</Empty>
          ) : (
            <table className="w-full border-collapse">
              <tbody>
                {unknown.map((u) => (
                  <tr key={u.signature} className="group border-b border-line/50 hover:bg-panel">
                    <td className="tnum w-[3rem] px-2.5 py-1 text-faint">{ago(u.block_time, now)}</td>
                    <td className="px-1 py-1 text-dim" title={u.wallet}>
                      {walletName(u.wallet)}
                    </td>
                    <td className="px-1 py-1 text-fg" title={u.programs.join("\n")}>
                      {u.programs.map(programName).join(" · ")}
                    </td>
                    <td className="w-[3.4rem] px-1.5 py-1 text-right">
                      <RowActions
                        onCopy={() => copy(u.signature, "signature")}
                        onOpen={() => openUrl(solscanTx(u.signature))}
                        title={u.signature}
                      />
                    </td>
                  </tr>
                ))}
              </tbody>
            </table>
          ))}
      </div>

      {toast && (
        <div
          className={`pointer-events-none absolute bottom-3 left-1/2 -translate-x-1/2 rounded px-3 py-1.5 shadow ${
            toast.kind === "big_buy" || toast.kind === "confluence"
              ? "bg-accent/25 font-semibold text-fg"
              : toast.kind
                ? "bg-panel2 text-fg"
                : "bg-panel2 text-dim"
          }`}
        >
          {toast.text}
        </div>
      )}
    </div>
  );
}

// ── token icon + external links ───────────────────────────────────────────

/** pump.fun PNG when we have it (local cache first, then the remote URL), else a tinted initial. */
function TokenIcon({ mint, meta, size = "1.25rem" }: { mint: string; meta: Record<string, TokenMeta>; size?: string }) {
  const m = meta[mint];
  // 0 = try local, 1 = try remote, 2 = give up
  const [attempt, setAttempt] = useState(0);
  const label = (m?.symbol || m?.name || mint).trim().slice(0, 1).toUpperCase();
  // stable hue per mint
  let h = 0;
  for (let i = 0; i < 8; i++) h = (h * 31 + mint.charCodeAt(i)) % 360;
  const src = attempt === 0 && m?.local ? convertFileSrc(m.local) : attempt <= 1 && m?.image ? m.image : null;
  if (src) {
    return (
      <img
        key={src}
        src={src}
        alt=""
        loading="lazy"
        referrerPolicy="no-referrer"
        onError={() => {
          if (attempt >= 1) invoke("ui_log", { level: "warn", msg: `token image failed: ${mint} ${src}` }).catch(() => {});
          setAttempt((a) => (a === 0 && m?.image ? 1 : 2));
        }}
        className="inline-block shrink-0 rounded-full object-cover align-middle"
        style={{ width: size, height: size, background: "var(--color-line)" }}
      />
    );
  }
  return (
    <span
      className="inline-flex shrink-0 items-center justify-center rounded-full align-middle text-[0.7em] font-semibold text-fg"
      style={{ width: size, height: size, background: `hsl(${h} 35% 28%)` }}
      aria-hidden
    >
      {label}
    </span>
  );
}

/** chart + pump.fun page for a mint */
function TokenLinks({ mint, chart }: { mint: string; chart: string }) {
  return (
    <span className="inline-flex gap-1">
      <button
        className="rounded px-1 text-faint hover:bg-line hover:text-fg"
        title="Open chart"
        onClick={(e) => {
          e.stopPropagation();
          openUrl(chartUrl(chart, mint));
        }}
      >
        📈
      </button>
      <button
        className="rounded px-1 opacity-70 hover:bg-line hover:opacity-100"
        title="Open on pump.fun"
        onClick={(e) => {
          e.stopPropagation();
          openUrl(pumpFunUrl(mint));
        }}
      >
        <img src={pumpfunIcon} alt="pump.fun" className="inline-block h-[1em] w-[1em] align-[-0.15em]" />
      </button>
    </span>
  );
}

// ── tokens tab ────────────────────────────────────────────────────────────

function TokensTab({
  tokens,
  tokenStates,
  meta,
  now,
  windowHours,
  walletName,
  watched,
  onWatch,
  onUnwatch,
  onDiscover,
  onRefresh,
  chart,
  error,
}: {
  tokens: TokenRow[] | null;
  tokenStates: Record<string, TokenStateRow>;
  meta: Record<string, TokenMeta>;
  now: number;
  windowHours: number;
  walletName: (a: string) => string;
  watched: Set<string>;
  onWatch: (m: string) => void;
  onUnwatch: (m: string) => void;
  onDiscover: (m: string) => void;
  onRefresh: () => void;
  chart: string;
  error: string | null;
}) {
  if (error !== null)
    return (
      <Empty>
        tokens failed: {error} ·{" "}
        <button className="underline hover:text-fg" onClick={onRefresh}>
          retry
        </button>
      </Empty>
    );
  if (tokens === null) return <Empty>loading…</Empty>;
  if (tokens.length === 0)
    return <Empty>no token activity from tracked wallets in the last {windowHours}h</Empty>;
  return (
    <div>
      <div className="flex items-center gap-2 px-2.5 pt-1.5 text-[0.8rem] text-faint">
        {tokens.length} tokens touched by your wallets in the last {windowHours}h · sorted by wallets buying, then net SOL in
        <button className="ml-auto rounded px-1.5 text-faint hover:bg-line hover:text-fg" onClick={onRefresh} title="Refresh">
          ↻
        </button>
      </div>
      <table className="w-full border-collapse">
        <thead className="text-left text-[0.75rem] uppercase tracking-wider text-faint">
          <tr>
            <th className="px-2.5 py-1 font-medium">token</th>
            <th className="px-1 py-1 text-right font-medium" title="distinct tracked wallets that bought / sold in the window">
              buyers
            </th>
            <th className="px-1 py-1 text-right font-medium" title="SOL your wallets net put in (bought − sold)">
              net in
            </th>
            <th className="px-1 py-1 text-right font-medium" title="tracked wallets still holding">
              holding
            </th>
            <th className="px-1 py-1 text-right font-medium">mcap</th>
            <th className="px-1 py-1 text-right font-medium" title="bonding-curve progress; ✓ = migrated to PumpSwap">
              curve
            </th>
            <th className="px-1 py-1 text-right font-medium" title="their realized / unrealized on this token">
              pnl
            </th>
            <th className="px-1 py-1 text-right font-medium">last</th>
            <th className="px-2.5 py-1 text-right font-medium"></th>
          </tr>
        </thead>
        <tbody>
          {tokens.map((t) => {
            const st = tokenStates[t.mint] ?? t.state;
            const hot = t.buyers.length >= 2;
            return (
              <tr key={t.mint} className="group border-b border-line/50 hover:bg-panel">
                <td className="whitespace-nowrap px-2.5 py-1">
                  <TokenIcon mint={t.mint} meta={meta} />{" "}
                  <LinkText title={tokenTitle(t.mint, meta)} onClick={() => openUrl(solscanToken(t.mint))}>
                    <span className={hot ? "font-semibold" : ""}>{sym(t.mint, meta)}</span>
                  </LinkText>
                  <span className="ml-1.5 text-faint">
                    {t.buys}b/{t.sells}s
                  </span>
                </td>
                <td className={`tnum px-1 py-1 text-right ${hot ? "text-accent" : "text-fg"}`} title={`buyers: ${t.buyers.map(walletName).join(", ")}\nsellers: ${t.sellers.map(walletName).join(", ")}`}>
                  {t.buyers.length}
                  <span className="text-faint">/{t.sellers.length}</span>
                </td>
                <td className={`tnum px-1 py-1 text-right ${signClass(t.net_flow_lamports)}`} title={`bought ${fmtSol(t.bought_lamports)} · sold ${fmtSol(t.sold_lamports)}`}>
                  {fmtSol(t.net_flow_lamports, { sign: true })}
                </td>
                <td className="tnum px-1 py-1 text-right text-fg" title={`${fmtTokens(t.held_qty, t.token_decimals)} held · value ${fmtSol(t.held_value_lamports)}◎`}>
                  {t.holders > 0 ? t.holders : <span className="text-faint">—</span>}
                </td>
                <td className="tnum px-1 py-1 text-right text-fg">
                  {st?.mcap_lamports ? `${fmtSol(st.mcap_lamports, { digits: 1 })}◎` : <span className="text-faint">—</span>}
                </td>
                <td className="tnum px-1 py-1 text-right">
                  <CurveCell st={st} />
                </td>
                <td className="tnum px-1 py-1 text-right">
                  <span className={signClass(t.realized_lamports)}>{fmtSol(t.realized_lamports, { sign: true })}</span>
                  {t.holders > 0 && (
                    <span className={`ml-1 ${signClass(t.unrealized_lamports)}`}>({fmtSol(t.unrealized_lamports, { sign: true })})</span>
                  )}
                </td>
                <td className="tnum px-1 py-1 text-right text-faint">{ago(t.last_trade, now)}</td>
                <td className="whitespace-nowrap px-2.5 py-1 text-right">
                  <TokenLinks mint={t.mint} chart={chart} />
                  {watched.has(t.mint) || t.watched ? (
                    <button className="rounded px-1.5 text-faint hover:bg-line hover:text-fg" title="Unwatch" onClick={() => onUnwatch(t.mint)}>
                      watching
                    </button>
                  ) : (
                    <button className="rounded bg-accent/15 px-2 py-0.5 text-fg hover:bg-accent/25" onClick={() => onWatch(t.mint)}>
                      watch
                    </button>
                  )}
                  <button className="ml-1 rounded px-1.5 text-faint hover:bg-line hover:text-fg" title="Find this token's earliest buyers" onClick={() => onDiscover(t.mint)}>
                    buyers
                  </button>
                </td>
              </tr>
            );
          })}
        </tbody>
      </table>
    </div>
  );
}

function CurveCell({ st }: { st: TokenStateRow | null | undefined }) {
  if (!st) return <span className="text-faint">—</span>;
  if (!st.is_pump) return <span className="text-faint" title="not a pump.fun token">n/a</span>;
  if (st.complete) return <span className="text-up" title="graduated to PumpSwap">✓ migrated</span>;
  const pct = st.progress_bp / 100;
  return (
    <span title={`${pct.toFixed(1)}% of the bonding curve sold`}>
      <span className="mr-1 inline-block h-[0.5rem] w-[3rem] overflow-hidden rounded-sm bg-line align-middle">
        <span className="block h-full bg-accent" style={{ width: `${Math.min(100, pct)}%` }} />
      </span>
      {Math.round(pct)}%
    </span>
  );
}

// ── watch tab ─────────────────────────────────────────────────────────────

function WatchLine({
  w,
  meta,
  now,
  walletName,
  onUnwatch,
  onDiscover,
  onCopy,
  chart,
}: {
  w: WatchRow;
  meta: Record<string, TokenMeta>;
  now: number;
  walletName: (a: string) => string;
  onUnwatch: () => void;
  onDiscover: () => void;
  onCopy: (t: string, w: string) => void;
  chart: string;
}) {
  const latest = w.latest;
  const mcaps = w.points.map((p) => (p.mcap_lamports ? Number(BigInt(p.mcap_lamports) / 1_000_000n) / 1000 : NaN)).filter((x) => !Number.isNaN(x));
  const first = mcaps[0];
  const last = mcaps[mcaps.length - 1];
  const delta = first && last ? ((last - first) / first) * 100 : null;
  return (
    <tr className="group border-b border-line/50 hover:bg-panel">
      <td className="whitespace-nowrap px-2.5 py-1">
        <TokenIcon mint={w.mint} meta={meta} />{" "}
        <LinkText title={tokenTitle(w.mint, meta)} onClick={() => openUrl(solscanToken(w.mint))}>
          {sym(w.mint, meta)}
        </LinkText>
        <span className="ml-1.5 text-faint">{latest ? ago(latest.t, now) : ""}</span>
      </td>
      <td className="px-1 py-1">
        <Sparkline values={mcaps} />
      </td>
      <td className="tnum px-1 py-1 text-right text-fg">
        {latest?.mcap_lamports ? `${fmtSol(latest.mcap_lamports, { digits: 1 })}◎` : <span className="text-faint">—</span>}
      </td>
      <td className={`tnum px-1 py-1 text-right ${delta === null ? "text-faint" : delta > 0 ? "text-up" : delta < 0 ? "text-down" : "text-dim"}`} title="change since the oldest sample kept">
        {delta === null ? "—" : `${delta > 0 ? "+" : ""}${delta.toFixed(1)}%`}
      </td>
      <td className="tnum px-1 py-1 text-right">
        <CurveCell
          st={
            latest
              ? {
                  fetched_at: latest.t,
                  is_pump: latest.is_pump,
                  complete: latest.complete,
                  progress_bp: latest.progress_bp,
                  mcap_lamports: latest.mcap_lamports,
                  real_sol_lamports: null,
                  price_lamports: latest.price_lamports,
                  price_token_units: latest.price_token_units,
                  pool: null,
                  creator: w.creator,
                }
              : null
          }
        />
      </td>
      <td className="px-1 py-1 text-dim">
        {w.creator ? (
          <LinkText title={w.creator} onClick={() => openUrl(solscanAccount(w.creator!))}>
            {walletName(w.creator)}
          </LinkText>
        ) : (
          <span className="text-faint">—</span>
        )}
      </td>
      <td className="whitespace-nowrap px-2.5 py-1 text-right">
        <TokenLinks mint={w.mint} chart={chart} />
        <button className="rounded px-1.5 text-faint hover:bg-line hover:text-fg" title="Find earliest buyers" onClick={onDiscover}>
          buyers
        </button>
        <button className="ml-1 rounded px-1.5 text-faint hover:bg-line hover:text-fg" title="Copy mint" onClick={() => onCopy(w.mint, "mint")}>
          ⧉
        </button>
        <button className="ml-1 rounded px-1.5 text-faint hover:bg-down/20 hover:text-down" title="Unwatch" onClick={onUnwatch}>
          ✕
        </button>
      </td>
    </tr>
  );
}

function Sparkline({ values }: { values: number[] }) {
  const W = 120;
  const H = 26;
  if (values.length < 2) return <svg width={W} height={H} className="opacity-40"><line x1={0} y1={H / 2} x2={W} y2={H / 2} stroke="currentColor" strokeDasharray="2 3" /></svg>;
  const min = Math.min(...values);
  const max = Math.max(...values);
  const span = max - min || 1;
  const pts = values.map((v, i) => `${(i / (values.length - 1)) * W},${H - 2 - ((v - min) / span) * (H - 4)}`).join(" ");
  const up = values[values.length - 1] >= values[0];
  return (
    <svg width={W} height={H} className={up ? "text-up" : "text-down"}>
      <polyline points={pts} fill="none" stroke="currentColor" strokeWidth={1.5} strokeLinejoin="round" strokeLinecap="round" />
    </svg>
  );
}

// ── wallet row + expandable positions ─────────────────────────────────────

function WalletRows({
  w,
  open,
  now,
  meta,
  simOn,
  onToggle,
  onCopy,
  onUntrack,
}: {
  w: WalletRow;
  open: boolean;
  now: number;
  meta: Record<string, TokenMeta>;
  simOn: boolean;
  onToggle: () => void;
  onCopy: (text: string, what: string) => void;
  onUntrack: () => void;
}) {
  const positions = [...w.open_positions, ...w.closed_positions];
  const s = w.stats;
  const cols = simOn ? 7 : 6;
  return (
    <>
      <tr className="group cursor-pointer border-t border-line/60 hover:bg-panel" onClick={onToggle}>
        <td className="whitespace-nowrap px-2.5 py-1.5 text-fg" title={w.wallet}>
          <span className="mr-1 inline-block w-[0.8em] text-faint">{open ? "▾" : "▸"}</span>
          {w.label ? (
            <>
              <span className="font-semibold">{w.label}</span> <span className="text-faint">{short(w.wallet)}</span>
            </>
          ) : (
            short(w.wallet)
          )}
          <span className="ml-1.5 text-faint">{w.trade_count}</span>
        </td>
        <td className="max-w-[16rem] truncate px-2 py-1.5">
          {w.last_trade ? <TradeCell t={w.last_trade} now={now} meta={meta} /> : <span className="text-faint">—</span>}
        </td>
        <td className="tnum px-2 py-1.5 text-right" title={`${s.wins} wins / ${s.losses} losses over ${s.tokens_with_exits} exited tokens`}>
          <span className={winClass(s.win_rate_bp)}>{fmtBp(s.win_rate_bp)}</span>{" "}
          <span className="text-faint">
            {s.wins}/{s.losses}
          </span>
        </td>
        <td className={`tnum px-2 py-1.5 text-right ${signClass(w.realized_lamports)}`}>
          {fmtSol(w.realized_lamports, { sign: true })}
        </td>
        <td className={`tnum px-2 py-1.5 text-right ${signClass(w.unrealized_lamports)}`}>
          {fmtSol(w.unrealized_lamports, { sign: true })}
        </td>
        {simOn && (
          <td
            className={`tnum px-2 py-1.5 text-right ${w.sim ? signClass(w.sim.realized_lamports) : "text-faint"}`}
            title={w.sim ? `copying (${w.sim.describe}): realized ${fmtSol(w.sim.realized_lamports, { sign: true })}, unrealized ${fmtSol(w.sim.unrealized_lamports, { sign: true })}` : ""}
          >
            {w.sim ? fmtSol(w.sim.realized_lamports, { sign: true }) : "—"}
          </td>
        )}
        <td className="tnum px-2.5 py-1.5 text-right text-dim">{w.open_positions.length}</td>
      </tr>
      {open && (
        <tr className="bg-panel/50">
          <td colSpan={cols} className="p-0">
            <div className="flex items-center gap-3 px-2.5 pt-1.5 text-[0.8rem] text-faint">
              <span className="selectable">{w.wallet}</span>
              <RowActions
                onCopy={() => onCopy(w.wallet, "address")}
                onOpen={() => openUrl(solscanAccount(w.wallet))}
                title={w.wallet}
                always
              />
              <button
                className="ml-auto rounded px-1.5 text-faint hover:bg-down/20 hover:text-down"
                title="Stop tracking this wallet"
                onClick={(e) => {
                  e.stopPropagation();
                  onUntrack();
                }}
              >
                untrack
              </button>
            </div>
            <StatsStrip s={s} />
            {w.sim && <SimStrip sim={w.sim} />}
            {positions.length === 0 ? (
              <div className="px-2.5 pb-2 pt-1 text-faint">no positions yet</div>
            ) : (
              <table className="w-full border-collapse">
                <thead className="text-left text-[0.75rem] uppercase tracking-wider text-faint">
                  <tr>
                    <th className="px-2.5 py-1 pl-[1.9rem] font-medium">token</th>
                    <th className="px-2 py-1 text-right font-medium">held</th>
                    <th className="px-2 py-1 text-right font-medium">hold</th>
                    <th className="px-2 py-1 text-right font-medium">cost</th>
                    <th className="px-2 py-1 text-right font-medium">value</th>
                    <th className="px-2 py-1 text-right font-medium">unrealized</th>
                    <th className="px-2.5 py-1 text-right font-medium">realized</th>
                  </tr>
                </thead>
                <tbody>
                  {positions.map((p) => (
                    <PositionLine key={p.mint} p={p} meta={meta} />
                  ))}
                </tbody>
              </table>
            )}
          </td>
        </tr>
      )}
    </>
  );
}

function StatsStrip({ s }: { s: StatsRow }) {
  return (
    <div className="flex flex-wrap gap-x-4 gap-y-1 px-2.5 pb-1 pt-1.5 text-[0.85rem]">
      <Stat label="win rate" title={`${s.wins} wins / ${s.losses} losses; ${s.tokens_with_exits - s.wins - s.losses} break-even`}>
        <span className={winClass(s.win_rate_bp)}>{fmtBp(s.win_rate_bp)}</span>
        <span className="text-faint">
          {" "}
          {s.wins}w {s.losses}l
        </span>
      </Stat>
      <Stat label="median hold" title="first buy → first sell, median over exited tokens">
        {fmtDuration(s.median_hold_secs)}
      </Stat>
      <Stat label="median buy">{s.median_buy_lamports ? `${fmtSol(s.median_buy_lamports)}◎` : "—"}</Stat>
      <Stat label="max buy">{`${fmtSol(s.max_buy_lamports)}◎`}</Stat>
      <Stat label="volume" title="SOL bought / SOL sold">
        {fmtSol(s.bought_lamports, { digits: 1 })} / {fmtSol(s.sold_lamports, { digits: 1 })}◎
      </Stat>
      <Stat label="24h">
        <span className={signClass(s.realized_24h)}>{fmtSol(s.realized_24h, { sign: true })}</span>
      </Stat>
      <Stat label="7d">
        <span className={signClass(s.realized_7d)}>{fmtSol(s.realized_7d, { sign: true })}</span>
      </Stat>
      <Stat label="30d">
        <span className={signClass(s.realized_30d)}>{fmtSol(s.realized_30d, { sign: true })}</span>
      </Stat>
      <Stat label="tokens">
        {s.tokens_traded}
        <span className="text-faint"> · {s.tokens_with_exits} exited</span>
      </Stat>
      {s.oversold_events > 0 && (
        <Stat label="oversold" title="sells beyond tracked quantity — tokens acquired outside decoded venues; treated as zero-basis">
          <span className="text-down">{s.oversold_events}</span>
        </Stat>
      )}
    </div>
  );
}

function SimStrip({ sim }: { sim: SimRow }) {
  return (
    <div
      className="flex flex-wrap items-baseline gap-x-4 gap-y-1 border-t border-line/40 px-2.5 pb-1.5 pt-1 text-[0.85rem]"
      title="paper trading: prices are only observed at the target's own trades; ignores liquidity and MEV"
    >
      <span className="text-faint">
        if copied <span className="text-dim">({sim.describe})</span>
      </span>
      <Stat label="realized">
        <span className={signClass(sim.realized_lamports)}>{fmtSol(sim.realized_lamports, { sign: true })}</span>
      </Stat>
      <Stat label="unrealized">
        <span className={signClass(sim.unrealized_lamports)}>{fmtSol(sim.unrealized_lamports, { sign: true })}</span>
      </Stat>
      <Stat label="deployed" title="total SOL spent on copied buys / received on copied sells">
        {fmtSol(sim.deployed_lamports, { digits: 2 })} / {fmtSol(sim.returned_lamports, { digits: 2 })}◎
      </Stat>
      <Stat label="win rate">
        <span className={winClass(sim.win_rate_bp)}>{fmtBp(sim.win_rate_bp)}</span>
        <span className="text-faint">
          {" "}
          {sim.wins}w {sim.losses}l
        </span>
      </Stat>
      <Stat label="copied">
        {sim.copied_buys}b/{sim.copied_sells}s
        {sim.skipped_small_buys > 0 && <span className="text-faint"> · {sim.skipped_small_buys} skipped</span>}
      </Stat>
      <Stat label="open">{sim.open_positions}</Stat>
      {sim.missed_exits > 0 && (
        <Stat label="missed exits" title="target sold before our delayed buy filled — we hold the bag">
          <span className="text-down">{sim.missed_exits}</span>
        </Stat>
      )}
      {sim.adverse_entries > 0 && (
        <Stat label="adverse entries" title="next observed price was already below our entry">
          <span className="text-down">{sim.adverse_entries}</span>
        </Stat>
      )}
      {sim.stopped_out > 0 && <Stat label="stops">{sim.stopped_out}</Stat>}
      {sim.took_profit > 0 && <Stat label="take-profits">{sim.took_profit}</Stat>}
    </div>
  );
}

function Stat({ label, title, children }: { label: string; title?: string; children: React.ReactNode }) {
  return (
    <span className="whitespace-nowrap" title={title}>
      <span className="text-faint">{label} </span>
      <span className="tnum text-fg">{children}</span>
    </span>
  );
}

function winClass(bp: number | null): string {
  if (bp === null) return "text-faint";
  return bp >= 5500 ? "text-up" : bp <= 4500 ? "text-down" : "text-fg";
}

function PositionLine({ p, meta }: { p: PositionRow; meta: Record<string, TokenMeta> }) {
  const closed = BigInt(p.qty) === 0n;
  const pct = closed ? "" : fmtPct(p.unrealized_lamports, p.cost_lamports);
  return (
    <tr className={`border-t border-line/40 ${closed ? "text-faint" : ""}`}>
      <td className="whitespace-nowrap px-2.5 py-1 pl-[1.9rem]">
        <TokenIcon mint={p.mint} meta={meta} size="1.05rem" />{" "}
        <LinkText title={tokenTitle(p.mint, meta)} onClick={() => openUrl(solscanToken(p.mint))}>
          {sym(p.mint, meta)}
        </LinkText>
        <span className="ml-1.5 text-faint">
          {p.buys}b/{p.sells}s
        </span>
      </td>
      <td className="tnum px-2 py-1 text-right">{closed ? "—" : fmtTokens(p.qty, p.token_decimals)}</td>
      <td className="tnum px-2 py-1 text-right">{fmtDuration(p.hold_secs)}</td>
      <td className="tnum px-2 py-1 text-right">{closed ? "—" : fmtSol(p.cost_lamports)}</td>
      <td className="tnum px-2 py-1 text-right">{closed ? "—" : fmtSol(p.value_lamports)}</td>
      <td className={`tnum px-2 py-1 text-right ${closed ? "" : signClass(p.unrealized_lamports)}`}>
        {closed ? "—" : fmtSol(p.unrealized_lamports, { sign: true })}
        {pct && <span className="ml-1 text-[0.8rem] opacity-80">{pct}</span>}
      </td>
      <td className={`tnum px-2.5 py-1 text-right ${signClass(p.realized_lamports)}`}>
        {fmtSol(p.realized_lamports, { sign: true })}
      </td>
    </tr>
  );
}

function TradeCell({ t, now, meta }: { t: SwapRow; now: number; meta: Record<string, TokenMeta> }) {
  const buy = t.side === "buy";
  return (
    <span className="whitespace-nowrap">
      <span className={`font-semibold uppercase ${buy ? "text-up" : "text-down"}`}>{t.side}</span>{" "}
      <span className="text-fg">{fmtTokens(t.token_amount, t.token_decimals)}</span>{" "}
      <span className="text-dim" title={tokenTitle(t.mint, meta)}>
        {sym(t.mint, meta)}
      </span>{" "}
      <span className="tnum text-dim">
        {buy ? "−" : "+"}
        {fmtSol(t.sol_amount)}◎
      </span>{" "}
      <span className="text-faint">{ago(t.block_time, now)}</span>
    </span>
  );
}

// ── small pieces ──────────────────────────────────────────────────────────

function KindBadge({ kind }: { kind: AlertRow["kind"] }) {
  const cls =
    kind === "big_buy" || kind === "confluence"
      ? "bg-accent/25 text-fg"
      : kind === "sell"
        ? "bg-down/20 text-down"
        : kind === "migrated" || kind === "progress" || kind === "mcap"
          ? "bg-panel2 text-fg"
          : "bg-up/15 text-up";
  const text = kind === "big_buy" ? "BIG BUY" : kind.toUpperCase();
  return <span className={`rounded px-1.5 py-0.5 text-[0.75rem] font-semibold tracking-wider ${cls}`}>{text}</span>;
}

function RowActions({
  onCopy,
  onOpen,
  title,
  always,
}: {
  onCopy: () => void;
  onOpen: () => void;
  title: string;
  always?: boolean;
}) {
  return (
    <span className={`inline-flex gap-1 ${always ? "" : "opacity-0 group-hover:opacity-100"}`} title={title}>
      <button
        className="rounded px-1 text-faint hover:bg-line hover:text-fg"
        title="Copy"
        onClick={(e) => {
          e.stopPropagation();
          onCopy();
        }}
      >
        ⧉
      </button>
      <button
        className="rounded px-1 text-faint hover:bg-line hover:text-fg"
        title="Open in Solscan"
        onClick={(e) => {
          e.stopPropagation();
          onOpen();
        }}
      >
        ↗
      </button>
    </span>
  );
}

function LinkText({ children, title, onClick }: { children: React.ReactNode; title: string; onClick: () => void }) {
  return (
    <button
      className="text-fg decoration-faint underline-offset-2 hover:underline"
      title={title}
      onClick={(e) => {
        e.stopPropagation();
        onClick();
      }}
    >
      {children}
    </button>
  );
}

function IconButton({
  children,
  title,
  onClick,
  active,
  danger,
}: {
  children: React.ReactNode;
  title: string;
  onClick: () => void;
  active?: boolean;
  danger?: boolean;
}) {
  const color = active ? "text-accent" : "text-dim";
  const hover = danger ? "hover:bg-down/30 hover:text-fg" : "hover:bg-line hover:text-fg";
  return (
    <button title={title} onClick={onClick} className={`rounded px-1.5 py-0.5 leading-none ${color} ${hover}`}>
      {children}
    </button>
  );
}

function TabButton({ children, active, onClick }: { children: React.ReactNode; active: boolean; onClick: () => void }) {
  return (
    <button onClick={onClick} className={active ? "text-fg" : "hover:text-dim"}>
      {children}
    </button>
  );
}

function Banner({ children, tone = "info" }: { children: React.ReactNode; tone?: "info" | "error" }) {
  const cls = tone === "error" ? "bg-down/10 text-down" : "bg-accent/10 text-dim";
  return <div className={`border-b border-line px-2.5 py-1.5 ${cls}`}>{children}</div>;
}

function Empty({ children }: { children: React.ReactNode }) {
  return <div className="p-5 text-center text-faint">{children}</div>;
}

function StatusDot({ status }: { status: ConnState }) {
  const color =
    status.state === "live"
      ? "bg-up"
      : status.state === "disconnected"
        ? "bg-down"
        : status.state === "idle"
          ? "bg-faint"
          : "bg-accent animate-pulse";
  return <span className={`inline-block h-[0.6rem] w-[0.6rem] rounded-full ${color}`} />;
}

function statusText(s: ConnState, name: (addr: string) => string): string {
  switch (s.state) {
    case "idle":
      return "no wallets tracked";
    case "connecting":
      return "connecting…";
    case "subscribed":
      return `subscribed to ${s.wallets} wallet${s.wallets === 1 ? "" : "s"}`;
    case "backfilling":
      return `backfilling ${name(s.wallet)} · ${s.pending} tx`;
    case "live":
      return "live";
    case "disconnected":
      return `disconnected: ${s.reason} · retry in ${Math.round(s.retry_in_ms / 1000)}s`;
  }
}
