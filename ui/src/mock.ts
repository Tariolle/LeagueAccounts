// Development-only stand-in for the Tauri backend, used when the UI is opened
// in a plain browser (`npm run dev`).
// Never bundled into release builds. Everything lives only in memory.
import { ApiError, api, events, keyId, parseAccountLine, type AccountView, type BulkAddResult, type Key, type LoginStep, type NewAccount, type RefreshDone, type Settings, type UpdateStatus } from "./api";

const lol = (tier: string, division: string, lp: string, finished = "Gold II", reached = finished) => ({
  tier,
  division,
  lp,
  reachedLastSeason: reached,
  finishedLastSeason: finished,
});
const tft = (tier: string, division: string, lp: string, lastSet = "Unranked") => ({ tier, division, lp, lastSet });

const sample: AccountView[] = [
  ["mainacc01", "Hextech Fox#EUW", "EUW", "412", "main", lol("Diamond", "II", "57", "Emerald I", "Diamond IV 12LP"), tft("Master", "", "142", "Diamond I")],
  ["smurfy_tr", "Kebab Enjoyer#TR1", "TR", "187", "smurf", lol("Emerald", "IV", "23", "Platinum I"), tft("Gold", "III", "64", "Gold IV")],
  ["duoqueen", "Piltover Queen#NA1", "NA", "301", "", lol("Challenger", "", "1124", "Grandmaster 640LP", "Challenger 1302LP"), tft("Unranked", "", "")],
  ["aramonly", "Snowball King#EUW", "EUW", "95", "ARAM", lol("Unranked", "", "", "Unranked"), tft("Platinum", "I", "88", "Silver I")],
  ["ironwill", "Iron Will#EUNE", "EUNE", "38", "", lol("Silver", "I", "12", "Bronze II"), tft("Iron", "II", "40")],
  ["goldenboy", "Golden Boy#KR1", "KR", "256", "Lee Sin OTP", lol("Gold", "III", "76", "Gold I", "Platinum IV"), tft("Emerald", "II", "5", "Platinum II")],
  ["gm_lux", "Lux Main#EUW", "EUW", "520", "", lol("Grandmaster", "", "512", "Master 210LP"), tft("Diamond", "IV", "0", "Emerald I")],
  ["bronzie", "Bronze Spirit#TR1", "TR", "61", "", lol("Bronze", "I", "99", "Iron I"), tft("Bronze", "IV", "17")],
  ["scholarly", "Arcane Scholar#EUNE", "EUNE", "380", "TFT main", lol("Master", "", "210", "Diamond I"), tft("Challenger", "", "1040", "Grandmaster")],
  ["tidalwave", "Tidal Wave#BR1", "BR", "143", "duo with Leo", lol("Platinum", "II", "48", "Gold I"), tft("Grandmaster", "", "520", "Master")],
  ["firstrecall", "First Recall#KR1", "KR", "30", "fresh account", lol("Iron", "IV", "7", "Unranked"), tft("Silver", "II", "36", "Bronze I")],
].map(([accountId, name, regionDisplay, level, description, lolRank, tftRank]) => ({
  accountId: accountId as string,
  name: name as string,
  riotIdNotFound: false,
  region: (regionDisplay as string).toLowerCase(),
  regionDisplay: regionDisplay as string,
  level: level as string,
  description: description as string,
  hasPassword: true,
  lol: lolRank as AccountView["lol"],
  tft: tftRank as AccountView["tft"],
  ban: accountId === "smurfy_tr" ? { until: Date.now() + (6 * 24 + 13) * 3_600_000 } : null,
}));

type Handler<T> = (payload: T) => void;
const listeners = {
  step: [] as Handler<LoginStep>[],
  pending: [] as Handler<Key[]>[],
  update: [] as Handler<AccountView>[],
  done: [] as Handler<RefreshDone>[],
  status: [] as Handler<UpdateStatus>[],
};
const unlisten = () => () => {};
const wait = (ms: number) => new Promise((resolve) => setTimeout(resolve, ms));

const settings: Settings = {
  autoRefresh: true,
  intervalMinutes: 30,
  startMinimized: false,
  launchAtStartup: false,
  loginMethod: "riot",
  launchGame: true,
  autoUpdate: true,
  uiScale: 100,
  riotClientFound: true,
  tftInstalled: true,
};

// Updates: `?scene=updating` finds one at launch (the update screen),
// `?scene=restart` installs one in the background (the restart notice).
const release = { version: "3.1.0", name: "v3.1.0", url: "https://github.com/", notes: "", publishedAt: "" };
let update: UpdateStatus = { phase: "checking", current: "3.0.3-dev", release: null, received: 0, total: 0, launch: true, seq: 1 };
const setUpdate = (change: Partial<UpdateStatus>) => {
  update = { ...update, ...change, seq: update.seq + 1 };
  listeners.status.forEach((handler) => handler(update));
};
/** About six seconds of simulated download, then installed. */
function simulateInstall(launch: boolean): void {
  const total = 14_680_064;
  setUpdate({ phase: "downloading", release, received: 0, total, launch });
  // At launch, start progressing once the update screen or the app is showing: a slow cold
  // start must not reach the launch check after the install already finished (and restarted).
  let shown = !launch;
  const timer = setInterval(() => {
    shown ||= document.querySelector("#startup-loader .update-screen") !== null || document.body.classList.contains("ready");
    if (!shown) return;
    const received = Math.min(total, update.received + total / 30);
    setUpdate({ received });
    if (received < total) return;
    clearInterval(timer);
    setUpdate({ phase: "ready" });
  }, 200);
}

export function installMock(): void {
  const scene = new URLSearchParams(location.search).get("scene");
  // This scene represents an update already found at launch. Seed that state
  // before bootstrap: a cold Vite load may consume the launch-check hold before
  // a delayed mock timer fires. Real backend state is also read at bootstrap.
  if (scene === "updating") simulateInstall(true);
  else setTimeout(() => {
    if (scene === "restart") setUpdate({ phase: "ready", release, launch: false });
    else setUpdate({ phase: "upToDate", release: { ...release, version: "3.0.3" } });
  }, scene === "restart" ? 1500 : 300);
  let accounts = structuredClone(sample);
  if (scene === "flex-ranks") {
    accounts = accounts.slice(0, 4);
    accounts[0].flex = { tier: "Gold", division: "II", lp: "40", lastSeason: "Silver I", rankedStats: { wins: 20, losses: 10, winRate: 67 } };
    accounts[1].flex = { tier: "Diamond", division: "IV", lp: "1", lastSeason: "Platinum II", rankedStats: { wins: 15, losses: 15, winRate: 50 } };
    accounts[2].flex = { tier: "Unranked", division: "", lp: "", lastSeason: "N/A", rankedStats: null };
    const flexCheckedAt = Date.now();
    const flexFresh = { source: "OP.GG", fetchedAt: flexCheckedAt, sourceUpdatedAt: flexCheckedAt, freshness: "fresh" as const, freshUntil: flexCheckedAt + 900_000, renewalAttempted: false, renewalSucceeded: false, error: null };
    accounts[0].flexProvenance = flexFresh;
    accounts[1].flexProvenance = { ...flexFresh, freshness: "stale", error: "parsing_failed" };
    accounts[2].flexProvenance = flexFresh;
    // The fourth account predates Flex support: unknown, rather than Unranked.
  }
  if (scene === "startup-lazy") {
    accounts[0].rankedStats = { wins: 47, losses: 33, winRate: 59 };
    accounts[0].recentGames = Array.from({ length: 10 }, (_, index) => ({
      champion: `Champion ${index + 1}`,
      iconUrl: `/startup-lazy-${index}.png`,
      won: index % 2 === 0,
    }));
  }
  if (scene === "ranked-cards") {
    [10, 2, 0, 12].forEach((count, index) => {
      accounts[index].description = "";
      accounts[index].rankedStats = { wins: 47, losses: 33, winRate: 49 + index };
      accounts[index].rankProvenance = { source: "OP.GG", fetchedAt: Date.now(), sourceUpdatedAt: Date.now(), freshness: "fresh", freshUntil: Date.now() + 900000, renewalAttempted: false, renewalSucceeded: false, error: null };
      accounts[index].recentGames = Array.from({ length: count }, (_, game) => ({
        champion: `Champion ${game + 1}`, iconUrl: "/league/ranks/iron.png", won: game % 2 === 0,
      }));
    });
  }
  const find = (key: Key) => accounts.find((account) => keyId(account) === keyId(key));
  const fakeFetch = (targets: AccountView[], exclusive = false) => {
    listeners.pending.forEach((handler) => handler(targets.map((account) => ({ accountId: account.accountId, region: account.region }))));
    targets.forEach((account, index) => {
      setTimeout(() => {
        const stored = find(account);
        if (stored?.name === account.name) {
          Object.assign(stored, account);
          listeners.update.forEach((handler) => handler(stored));
        }
        if (index === targets.length - 1) listeners.done.forEach((handler) => handler({ error: null, exclusive, auto: false }));
      }, 500 + index * 260);
    });
  };

  Object.assign(api, {
    bootstrap: async () => ({
      accounts,
      regions: ["EUW", "EUNE", "NA", "KR", "TR", "BR"],
      version: "3.0.3-dev",
      loggingAvailable: true,
      loadError: null,
      settings,
      schedule: { enabled: true, intervalMinutes: 30, nextAt: Date.now() + 23 * 60_000, lastAt: Date.now() - 7 * 60_000, running: false },
    }),
    getSettings: async () => settings,
    updateSettings: async (input: object) => Object.assign(settings, input),
    login: async (key: Key, _tft: boolean, _closeRunning: boolean, launchBanned: boolean) => {
      const ban = accounts.find((account) => keyId(account) === keyId(key))?.ban;
      const steps: LoginStep[] = ["openClient", "waitAuth", "connect", "findWindow", "type", "confirm", "launchGame", "done"];
      for (const step of steps) {
        if (ban && settings.launchGame && !launchBanned && step === "launchGame") throw new ApiError("account_banned", ban.until == null ? "" : String(ban.until));
        listeners.step.forEach((handler) => handler(step));
        await wait(step === "waitAuth" || step === "confirm" ? 1400 : 700);
      }
      return "signedIn";
    },
    cancelLogin: async () => {},
    gameStatus: async () => ({ clientOpen: false, inGame: false, sameAccount: false }),
    updateStatus: async () => update,
    checkUpdate: async () => {
      await wait(400);
      if (update.phase === "ready" || update.phase === "downloading") return update;
      if (settings.autoUpdate) simulateInstall(false);
      else setUpdate({ phase: "available", release, launch: false });
      return update;
    },
    updateIdle: async () => true,
    restartToUpdate: async () => {
      if (update.phase !== "ready") throw new ApiError("update_not_ready");
      setUpdate({ phase: "restarting" });
      // The new version starts like a fresh launch, without the demo scene.
      setTimeout(() => location.replace(location.pathname), 800);
    },
    openRelease: async () => {},
    listAccounts: async () => accounts,
    addAccount: async (input: NewAccount) => {
      const account: AccountView = {
        accountId: input.accountId,
        name: input.name.trim(),
        riotIdNotFound: false,
        region: input.region.toLowerCase(),
        regionDisplay: input.region,
        description: input.description,
        level: "",
        hasPassword: true,
        lol: lol("Unranked", "", "", "N/A"),
        tft: tft("Unranked", "", "", "N/A"),
      };
      accounts.push(account);
      if (account.name) setTimeout(() => fakeFetch([{ ...account, level: "30", lol: lol("Silver", "II", "45", "Bronze I") }]), 50);
      return account;
    },
    multiAdd: async (text: string, region: string): Promise<BulkAddResult> => {
      let added = 0;
      const skippedLines: number[] = [];
      for (const [index, line] of text.split("\n").entries()) {
        if (!line.trim()) continue;
        const input = parseAccountLine(line);
        if (!input || accounts.some((account) => account.accountId.toLowerCase() === input.accountId.toLowerCase() && account.region === region.toLowerCase())) {
          skippedLines.push(index);
          continue;
        }
        await api.addAccount({ ...input, region, description: "" });
        added++;
      }
      return { added, skippedLines };
    },
    updateAccount: async (key: Key, name: string, description: string) => {
      const account = find(key)!;
      name = name.trim();
      const renamed = account.name !== name;
      Object.assign(account, { name, description });
      if (renamed) {
        Object.assign(account, { riotIdNotFound: false, level: "", lol: lol("Unranked", "", "", "N/A"), tft: tft("Unranked", "", "", "N/A") });
        const updated = name ? { ...account, level: "30", lol: lol("Silver", "II", "45", "Bronze I") } : { ...account };
        setTimeout(() => fakeFetch([updated]), 50);
      }
      return account;
    },
    deleteAccount: async (key: Key) => {
      accounts = accounts.filter((account) => keyId(account) !== keyId(key));
    },
    refreshRanks: async () => {
      setTimeout(() => fakeFetch(accounts, true), 50);
      return accounts.length;
    },
    copyAccountId: async () => {},
    copyPassword: async () => 30,
    exportData: async () => "C:\\Users\\demo\\credentials.json",
    importData: async () => null,
    openProfile: async () => {},
    openLogsFolder: async () => {},
  } satisfies typeof api);
  Object.assign(events, {
    onLoginStep: async (handler: Handler<LoginStep>) => (listeners.step.push(handler), unlisten()),
    onUpdateStatus: async (handler: Handler<UpdateStatus>) => (listeners.status.push(handler), unlisten()),
    onRefreshStarted: async () => unlisten(),
    onSchedule: async () => unlisten(),
    onRankPending: async (handler: Handler<Key[]>) => (listeners.pending.push(handler), unlisten()),
    onRankUpdate: async (handler: Handler<AccountView>) => (listeners.update.push(handler), unlisten()),
    onRefreshDone: async (handler: Handler<RefreshDone>) => (listeners.done.push(handler), unlisten()),
    onAccountUpdate: async () => unlisten(),
  } satisfies typeof events);
}
