import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";

export interface Key {
  accountId: string;
  region: string;
}

export interface LolRank {
  tier: string;
  division: string;
  lp: string;
  reachedLastSeason: string;
  finishedLastSeason: string;
}

export interface TftRank {
  tier: string;
  division: string;
  lp: string;
  lastSet: string;
}

export type LoginMethod = "riot" | "previous";

export interface Settings {
  autoRefresh: boolean;
  intervalMinutes: number;
  startMinimized: boolean;
  launchAtStartup: boolean;
  loginMethod: LoginMethod;
  launchGame: boolean;
  /** Install new releases automatically; when off, only notify. */
  autoUpdate: boolean;
  /** Interface size in percent; the window zooms and resizes with it. */
  uiScale: number;
  /** Empty until the welcome dialog has been answered once. */
  uiLanguage: string;
  riotClientFound: boolean;
  tftInstalled: boolean;
}

// `uiLanguage` is set through its own command, not the Settings form, so it is
// not part of what the form submits.
export type SettingsInput = Omit<Settings, "riotClientFound" | "tftInstalled" | "uiLanguage">;

export interface Schedule {
  enabled: boolean;
  intervalMinutes: number;
  nextAt: number | null;
  lastAt: number | null;
  running: boolean;
}

export type LoginStep =
  | "closeLeague"
  | "openClient"
  | "signOut"
  | "signIn"
  | "verification"
  | "focus"
  | "type"
  | "launchGame"
  | "updating"
  | "done";

export interface GameStatus {
  clientOpen: boolean;
  inGame: boolean;
  sameAccount: boolean;
}

export interface Release {
  version: string;
  name: string;
  url: string;
  notes: string;
  publishedAt: string;
}

export type UpdatePhase =
  | "idle"
  | "checking"
  | "upToDate"
  | "available"
  | "downloading"
  | "ready"
  | "restarting"
  | "failed";

/** "available" and "failed" fall back to the download banner; "ready" is installed. */
export interface UpdateStatus {
  phase: UpdatePhase;
  current: string;
  /** The latest release when known, newer or not. */
  release: Release | null;
  received: number;
  total: number;
  /** The check the app starts with, which may hold the loading screen. */
  launch: boolean;
  /** Increases with every change; an older snapshot is ignored. */
  seq: number;
}

export type LoginResult = "signedIn" | "typed" | "alreadySignedIn";

export interface RankProvenance {
  source: string;
  fetchedAt: number | null;
  sourceUpdatedAt: number | null;
  freshness: "fresh" | "stale" | "unknown";
  /** Set by the backend from its single age threshold. */
  freshUntil: number | null;
  renewalAttempted: boolean;
  renewalSucceeded: boolean;
  error: "profile_not_found" | "rate_limited" | "provider_unavailable" | "parsing_failed" | "forbidden" | "timeout" | "renewal_failed" | null;
}

export interface AccountView {
  rankProvenance?: RankProvenance | null;
  tftProvenance?: RankProvenance | null;
  accountId: string;
  name: string;
  riotIdNotFound: boolean;
  region: string;
  regionDisplay: string;
  description: string;
  level: string;
  hasPassword: boolean;
  /** Skip Riot's profile-update flow at sign-in (VN accounts). */
  skipProfileUpdate: boolean;
  /** Product locale this account starts the game in; null = Riot's choice. */
  locale: string | null;
  lol: LolRank;
  tft: TftRank;
  /** League suspension reported at the last Riot sign-in; `until` is null when permanent. */
  ban?: Ban | null;
}

export interface Ban {
  until: number | null;
}

export const accountLabel = (account: AccountView): string => account.name || account.accountId;

/** Backend failure: a stable code the UI translates, plus optional detail. */
export interface ErrorPayload {
  code: string;
  detail: string | null;
}

export class ApiError extends Error {
  constructor(
    readonly code: string,
    readonly detail: string | null = null,
  ) {
    super(code);
  }
}

export interface RefreshDone {
  error: ErrorPayload | null;
  /** True for a full refresh, false for a single-account refresh. */
  exclusive: boolean;
  auto: boolean;
}

export interface RefreshStarted {
  total: number;
  auto: boolean;
}

export interface LocaleOption {
  code: string;
  name: string;
  /// False when the client home page does not render in this locale.
  homePageWorks: boolean;
  /// False when the Riot Client substitutes another locale at launch.
  launchable: boolean;
}

export interface Bootstrap {
  accounts: AccountView[];
  regions: string[];
  locales: LocaleOption[];
  version: string;
  loggingAvailable: boolean;
  loadError: ErrorPayload | null;
  settings: Settings;
  schedule: Schedule;
}

export interface BatchResult {
  added: number;
  skipped: number;
}

export interface BulkAddResult {
  added: number;
  skippedLines: number[];
}

export interface UpdateAccount {
  name: string;
  description: string;
  locale: string | null;
  password?: string;
  skipProfileUpdate?: boolean;
}

export interface NewAccount {
  accountId: string;
  name: string;
  region: string;
  password: string;
  description: string;
  locale?: string | null;
  skipProfileUpdate?: boolean;
}

export function parseAccountLine(line: string): Pick<NewAccount, "accountId" | "name" | "password"> | undefined {
  // Mirrors `parse_account_line` in Rust: `--` separates fields, two fields
  // mean no Riot ID yet, and a three-field line needs all three filled. A
  // password that starts with `--` therefore fails to parse rather than
  // silently losing its dashes.
  const parts = line.split("--").map((part) => part.trim());
  if (parts.length === 2 && parts[0] && parts[1]) {
    return { accountId: parts[0], name: "", password: parts[1] };
  }
  if (parts.length === 3 && parts[0] && parts[1] && parts[2]) {
    return { accountId: parts[0], name: parts[1], password: parts[2] };
  }
  return undefined;
}

export const keyOf = (account: AccountView): Key => ({
  accountId: account.accountId,
  region: account.region,
});

export const keyId = (key: Key): string => `${key.region}:${key.accountId}`;

/** Tauri rejects with the command's `Err(AppError)`; normalize to ApiError. */
async function call<T>(command: string, args?: Record<string, unknown>): Promise<T> {
  try {
    return await invoke<T>(command, args);
  } catch (error) {
    if (error && typeof error === "object" && "code" in error) {
      const payload = error as ErrorPayload;
      throw new ApiError(payload.code, payload.detail);
    }
    throw new ApiError("unknown", String(error));
  }
}

export const api = {
  bootstrap: () => call<Bootstrap>("bootstrap"),
  listAccounts: () => call<AccountView[]>("list_accounts"),
  addAccount: (input: NewAccount) => call<AccountView>("add_account", { input }),
  multiAdd: (text: string, region: string, locale: string | null) =>
    call<BulkAddResult>("multi_add", { text, region, locale }),
  /** `input.password` left undefined keeps the stored credential. */
  updateAccount: (key: Key, input: UpdateAccount) =>
    call<AccountView>("update_account", { key, input }),
  deleteAccount: (key: Key) => call<void>("delete_account", { key }),
  refreshRanks: (force = false) => call<number>("refresh_ranks", { force }),
  copyAccountId: (key: Key) => call<void>("copy_account_id", { key }),
  copyPassword: (key: Key) => call<number>("copy_password", { key }),
  /** `launchBanned` launches the game even if Riot reports a League ban. */
  login: (key: Key, tft: boolean, closeRunning: boolean, launchBanned: boolean) =>
    call<LoginResult>("login", { key, tft, closeRunning, launchBanned }),
  cancelLogin: () => call<void>("cancel_login"),
  gameStatus: (key: Key) => call<GameStatus>("game_status", { key }),
  updateStatus: () => call<UpdateStatus>("update_status"),
  checkUpdate: () => call<UpdateStatus>("check_update"),
  /** No login, rank refresh or match in progress. */
  updateIdle: () => call<boolean>("update_idle"),
  restartToUpdate: () => call<void>("restart_to_update"),
  openRelease: (url: string) => call<void>("open_release", { url }),
  getSettings: () => call<Settings>("get_settings"),
  updateSettings: (input: SettingsInput) => call<Settings>("update_settings", { input }),
  /** Durable copy of the interface language, for when the webview cache is lost. */
  setUiLanguage: (language: string) => call<void>("set_ui_language", { language }),
  exportData: (title: string) => call<string | null>("export_data", { title }),
  importData: (title: string) => call<BatchResult | null>("import_data", { title }),
  openProfile: (key: Key, tft: boolean) => call<void>("open_profile", { key, tft }),
  openLogsFolder: () => call<void>("open_logs_folder"),
  /** Opens ABC's Bilibili page, where the zh-cn-home-locale plugin lives. */
  openHomeFixAuthor: () => call<void>("open_home_fix_author"),
  /** Opens Pengu Loader's site; the plugin needs it installed. */
  openPenguLoader: () => call<void>("open_pengu_loader"),
};

export const events = {
  onLoginStep: (handler: (step: LoginStep) => void): Promise<UnlistenFn> =>
    listen<LoginStep>("login-step", (event) => handler(event.payload)),
  onUpdateStatus: (handler: (status: UpdateStatus) => void): Promise<UnlistenFn> =>
    listen<UpdateStatus>("update-status", (event) => handler(event.payload)),
  onRefreshStarted: (handler: (started: RefreshStarted) => void): Promise<UnlistenFn> =>
    listen<RefreshStarted>("refresh-started", (event) => handler(event.payload)),
  onSchedule: (handler: (schedule: Schedule) => void): Promise<UnlistenFn> =>
    listen<Schedule>("schedule", (event) => handler(event.payload)),
  onRankPending: (handler: (keys: Key[]) => void): Promise<UnlistenFn> =>
    listen<Key[]>("rank-pending", (event) => handler(event.payload)),
  onRankUpdate: (handler: (account: AccountView) => void): Promise<UnlistenFn> =>
    listen<AccountView>("rank-update", (event) => handler(event.payload)),
  onRefreshDone: (handler: (done: RefreshDone) => void): Promise<UnlistenFn> =>
    listen<RefreshDone>("refresh-done", (event) => handler(event.payload)),
  onAccountUpdate: (handler: (account: AccountView) => void): Promise<UnlistenFn> =>
    listen<AccountView>("account-update", (event) => handler(event.payload)),
};
