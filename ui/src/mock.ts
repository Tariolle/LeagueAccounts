// Development-only stand-in for the Tauri backend, used when the UI is opened
// in a plain browser (`npm run dev`). Never bundled into release builds.
import { api, events, keyId, parseAccountLine, type AccountView, type BulkAddResult, type Key, type NewAccount, type RefreshDone } from "./api";

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
}));

type Handler<T> = (payload: T) => void;
const listeners = {
  step: [] as Handler<string>[],
  pending: [] as Handler<Key[]>[],
  update: [] as Handler<AccountView>[],
  done: [] as Handler<RefreshDone>[],
};
const unlisten = () => () => {};
const wait = (ms: number) => new Promise((resolve) => setTimeout(resolve, ms));

const settings = {
  autoRefresh: true,
  intervalMinutes: 30,
  startMinimized: false,
  launchAtStartup: false,
  loginMethod: "riot" as const,
  launchGame: true,
  riotClientFound: true,
  tftInstalled: true,
};

export function installMock(): void {
  let accounts = structuredClone(sample);
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
      version: "3.0.0",
      loggingAvailable: true,
      loadError: null,
      settings,
      schedule: { enabled: true, intervalMinutes: 30, nextAt: Date.now() + 23 * 60_000, lastAt: Date.now() - 7 * 60_000, running: false },
    }),
    getSettings: async () => settings,
    updateSettings: async (input: object) => Object.assign(settings, input),
    login: async () => {
      const steps = ["openClient", "waitAuth", "findWindow", "focus", "type", "confirm", "launchGame", "done"];
      for (const step of steps) {
        listeners.step.forEach((handler) => handler(step));
        await wait(step === "waitAuth" || step === "confirm" ? 1400 : 700);
      }
      return "signedIn";
    },
    cancelLogin: async () => {},
    gameStatus: async () => ({ clientOpen: false, inGame: false, sameAccount: false }),
    checkUpdate: async () => ({
      current: "3.0.0",
      newer: true,
      latest: { version: "3.1.0", name: "v3.1.0", url: "https://github.com/", notes: "", publishedAt: "" },
    }),
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
  });
  Object.assign(events, {
    onLoginStep: async (handler: Handler<string>) => (listeners.step.push(handler), unlisten()),
    onUpdateAvailable: async () => unlisten(),
    onRefreshStarted: async () => unlisten(),
    onSchedule: async () => unlisten(),
    onRankPending: async (handler: Handler<Key[]>) => (listeners.pending.push(handler), unlisten()),
    onRankUpdate: async (handler: Handler<AccountView>) => (listeners.update.push(handler), unlisten()),
    onRefreshDone: async (handler: Handler<RefreshDone>) => (listeners.done.push(handler), unlisten()),
  });
}
