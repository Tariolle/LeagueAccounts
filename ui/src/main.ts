import "@fontsource-variable/inter";
import "@fontsource/cinzel/600.css";
import "./styles.css";
import "./features.css";
import "./login.css";

import { ApiError, accountLabel, api, events, keyId, keyOf, parseAccountLine, type AccountView, type Key, type Release, type Schedule, type Settings } from "./api";
import { openLoginOverlay, type LoginOverlay } from "./login";
import { LANGUAGES, applyStatic, errorText, lang, localizeRank, setLang, t, tierName, type Lang, type MessageKey } from "./i18n";
import { $, closeMenu, contextMenu, esc, fragment, leave, modal, reducedMotion, toast } from "./dom";
import { hydrateIcons, icon } from "./icons";
import {
  APEX,
  TIER_ORDER,
  canPlayWith,
  compareRanks,
  emblem,
  installEmblemDefs,
  isRanked,
  lpProgress,
  normalizeTier,
  palette,
  rankLabel,
  rankOf,
  type Mode,
} from "./ranks";

type View = "grid" | "list";
type Sort = "rank" | "name" | "level" | "region";

const state = {
  accounts: [] as AccountView[],
  regions: [] as string[],
  mode: "lol" as Mode,
  view: "grid" as View,
  sort: "rank" as Sort,
  search: "",
  regionFilter: "all",
  friendTier: "all",
  friendDivision: "I",
  tierFilter: null as string | null,
  selected: null as string | null,
  pending: new Set<string>(),
  refreshing: false,
  refreshTotal: 0,
  refreshDone: 0,
  copyStage: { id: "", count: 0 },
  settings: null as Settings | null,
  schedule: null as Schedule | null,
};

// ---------------------------------------------------------------- preferences

const PREFS_KEY = "leagueaccounts.prefs";

function loadPrefs(): void {
  try {
    const prefs = JSON.parse(localStorage.getItem(PREFS_KEY) ?? "{}");
    if (prefs.mode === "lol" || prefs.mode === "tft") state.mode = prefs.mode;
    if (prefs.view === "grid" || prefs.view === "list") state.view = prefs.view;
    if (["rank", "name", "level", "region"].includes(prefs.sort)) state.sort = prefs.sort;
  } catch {
    // Storage unavailable: keep defaults.
  }
}

function savePrefs(): void {
  try {
    localStorage.setItem(PREFS_KEY, JSON.stringify({ mode: state.mode, view: state.view, sort: state.sort }));
  } catch {
    // Non-essential.
  }
}

// ---------------------------------------------------------------- helpers

const byId = (id: string) => state.accounts.find((account) => keyId(keyOf(account)) === id);

function splitRiotId(name: string): [string, string] {
  const index = name.lastIndexOf("#");
  return index < 0 ? [name.trim(), ""] : [name.slice(0, index).trim(), name.slice(index + 1).trim()];
}

function visibleAccounts(): AccountView[] {
  const query = state.search.trim().toLowerCase();
  const list = state.accounts.filter((account) => {
    const rank = rankOf(account, state.mode);
    if (query && !`${account.name} ${account.accountId} ${account.description}`.toLowerCase().includes(query)) {
      return false;
    }
    if (state.regionFilter !== "all" && account.regionDisplay !== state.regionFilter) return false;
    if (state.tierFilter && normalizeTier(rank.tier) !== state.tierFilter) return false;
    if (state.mode === "lol" && state.friendTier !== "all") {
      return canPlayWith(rank, state.friendTier, state.friendDivision);
    }
    return true;
  });
  const byName = (a: AccountView, b: AccountView) => accountLabel(a).localeCompare(accountLabel(b), "tr");
  const sorters: Record<Sort, (a: AccountView, b: AccountView) => number> = {
    rank: (a, b) => compareRanks(rankOf(a, state.mode), rankOf(b, state.mode)) || byName(a, b),
    name: byName,
    level: (a, b) => (Number(b.level) || 0) - (Number(a.level) || 0) || byName(a, b),
    region: (a, b) => a.regionDisplay.localeCompare(b.regionDisplay) || byName(a, b),
  };
  return list.sort(sorters[state.sort]);
}

// ---------------------------------------------------------------- card rendering

function historyLine(account: AccountView): string {
  if (state.mode === "tft") {
    return `<span>${t("card.lastSet")}</span><b>${esc(localizeRank(account.tft.lastSet))}</b>`;
  }
  const reached = account.lol.reachedLastSeason || "N/A";
  return `<span>${t("card.lastSeason")}</span><b>${esc(localizeRank(reached))}</b>`;
}

function cardSignature(account: AccountView, pending: boolean): string {
  return JSON.stringify([account, state.mode, pending, lang()]);
}

function cardInner(account: AccountView, pending: boolean): string {
  const rank = rankOf(account, state.mode);
  const tier = normalizeTier(rank.tier);
  const [gameName, tag] = splitRiotId(accountLabel(account));
  const lp = isRanked(rank) && rank.lp ? `<span class="lp">${esc(rank.lp)} LP</span>` : "";
  return `
    <div class="card-sheen"></div>
    <div class="card-top">
      <div class="emblem-wrap">${emblem(tier, 58)}</div>
      <div class="identity">
        <div class="name" title="${esc(accountLabel(account))}">
          <span class="game-name">${esc(gameName)}</span>${tag ? `<span class="tag">#${esc(tag)}</span>` : ""}
        </div>
        <div class="meta">
          <span class="chip chip-region">${esc(account.regionDisplay)}</span>
          ${account.level ? `<span class="chip">Lv ${esc(account.level)}</span>` : ""}
          <span class="acc-id" title="${esc(t("card.accountId"))}">${icon("users", 12)}${esc(account.accountId)}</span>
        </div>
      </div>
      <button class="icon-btn card-more" data-act="menu" title="${esc(t("card.more"))}">${icon("more")}</button>
    </div>
    <div class="rank-block${pending ? " is-pending" : ""}">
      ${!account.name || account.riotIdNotFound ? `
      <div class="rank-notice${account.riotIdNotFound ? " rank-not-found" : ""}">${esc(t(account.name ? "card.riotIdNotFound" : "card.noRiotId"))}</div>
      <button class="rank-edit" data-act="edit">${esc(t(account.name ? "card.updateRiotId" : "card.addRiotId"))}</button>` : `
      <div class="rank-line">
        <span class="rank-name">${esc(rankLabel(rank))}</span>${lp}
      </div>
      <div class="lp-bar${APEX.has(tier) ? " apex" : ""}"><span style="--p:${lpProgress(rank)}"></span></div>
      <div class="history">${historyLine(account)}</div>`}
    </div>
    ${account.description ? `<p class="desc" title="${esc(account.description)}">${esc(account.description)}</p>` : `<p class="desc desc-empty"></p>`}
    <div class="card-actions">
      <button class="btn btn-play" data-act="login" title="${esc(t("card.loginTitle"))}">${icon("login", 15)}${esc(t("card.login"))}</button>
      <button class="icon-btn" data-act="copy-id" title="${esc(t("card.copyId"))}">${icon("copy", 15)}</button>
      <button class="icon-btn" data-act="copy-pass" title="${esc(t("card.copyPassword"))}">${icon("key", 15)}</button>
    </div>`;
}

function updateCard(element: HTMLElement, account: AccountView): void {
  const id = keyId(keyOf(account));
  const pending = state.pending.has(id);
  const signature = cardSignature(account, pending);
  const colors = palette(rankOf(account, state.mode).tier);
  element.style.setProperty("--tier", colors.base);
  element.style.setProperty("--tier-light", colors.light);
  element.style.setProperty("--tier-dark", colors.dark);
  element.classList.toggle("selected", state.selected === id);
  element.classList.toggle("apex", APEX.has(normalizeTier(rankOf(account, state.mode).tier)));
  if (element.dataset.signature === signature) return;
  const wasPending = element.dataset.pending === "1";
  element.dataset.signature = signature;
  element.dataset.pending = pending ? "1" : "0";
  element.innerHTML = cardInner(account, pending);
  if (wasPending && !pending && !reducedMotion()) {
    element.classList.remove("rank-arrived");
    void element.offsetWidth;
    element.classList.add("rank-arrived");
  }
}

let firstRender = true;

function renderAccounts(): void {
  const container = $("#accounts");
  const list = visibleAccounts();
  const existing = new Map<string, HTMLElement>();
  container.querySelectorAll<HTMLElement>(":scope > .card").forEach((element) => {
    existing.set(element.dataset.key ?? "", element);
  });

  // FLIP: remember where every card was before reordering.
  const before = new Map<string, DOMRect>();
  if (!reducedMotion()) existing.forEach((element, id) => before.set(id, element.getBoundingClientRect()));

  const wanted = new Set(list.map((account) => keyId(keyOf(account))));
  existing.forEach((element, id) => {
    if (!wanted.has(id)) element.remove();
  });

  let entering = 0;
  list.forEach((account, index) => {
    const id = keyId(keyOf(account));
    let element = existing.get(id);
    if (!element) {
      element = document.createElement("article");
      element.className = "card";
      element.dataset.key = id;
      element.tabIndex = 0;
      element.style.setProperty("--i", String(Math.min(entering++, 14)));
      element.classList.add(firstRender ? "enter-stagger" : "enter");
      element.addEventListener("animationend", (event) => {
        if (event.target === element) element?.classList.remove("enter", "enter-stagger");
      });
    }
    updateCard(element, account);
    if (container.children[index] !== element) container.insertBefore(element, container.children[index] ?? null);
  });

  if (!reducedMotion()) {
    container.querySelectorAll<HTMLElement>(":scope > .card").forEach((element) => {
      const old = before.get(element.dataset.key ?? "");
      if (!old) return;
      const now = element.getBoundingClientRect();
      const dx = old.left - now.left;
      const dy = old.top - now.top;
      if (Math.abs(dx) < 1 && Math.abs(dy) < 1) return;
      element.animate(
        [{ transform: `translate(${dx}px, ${dy}px)` }, { transform: "translate(0, 0)" }],
        { duration: 420, easing: "cubic-bezier(.2,.8,.2,1)" },
      );
    });
  }
  firstRender = false;

  const empty = $("#empty");
  empty.hidden = list.length > 0;
  if (!list.length) {
    const noAccounts = state.accounts.length === 0;
    empty.innerHTML = `
      <div class="empty-art">${emblem(noAccounts ? "Unranked" : "Iron", 88)}</div>
      <h2>${t(noAccounts ? "empty.noneTitle" : "empty.noMatchTitle")}</h2>
      <p>${t(noAccounts ? "empty.noneText" : "empty.noMatchText")}</p>
      ${
        noAccounts
          ? `<button class="btn btn-primary" data-empty="add">${icon("plus")} ${t("side.add")}</button>`
          : `<button class="btn btn-ghost" data-empty="clear">${icon("x")} ${t("empty.clear")}</button>`
      }`;
  }
}

// ---------------------------------------------------------------- sidebar

function renderDistribution(): void {
  const counts = new Map<string, number>();
  state.accounts.forEach((account) => {
    const tier = normalizeTier(rankOf(account, state.mode).tier);
    counts.set(tier, (counts.get(tier) ?? 0) + 1);
  });
  const tiers = TIER_ORDER.filter((tier) => counts.get(tier) || (tier !== "Error" && tier !== "Unranked"));
  const max = Math.max(1, ...counts.values());
  const root = $("#distribution");
  root.innerHTML = tiers
    .map((tier) => {
      const count = counts.get(tier) ?? 0;
      const colors = palette(tier);
      const active = state.tierFilter === tier;
      return `<button class="dist-row${active ? " active" : ""}${count ? "" : " zero"}" data-tier="${tier}" style="--tier:${colors.base};--w:${count / max}" ${count ? "" : "disabled"}>
        <span class="dist-emblem">${emblem(tier, 20)}</span>
        <span class="dist-name">${tierName(tier)}</span>
        <span class="dist-bar"><span></span></span>
        <span class="dist-count">${count}</span>
      </button>`;
    })
    .join("");
}

function renderControls(): void {
  document.documentElement.dataset.mode = state.mode;
  document.querySelectorAll<HTMLButtonElement>(".mode-btn").forEach((button) => {
    button.setAttribute("aria-selected", String(button.dataset.mode === state.mode));
  });
  document.querySelectorAll<HTMLButtonElement>("[data-view]").forEach((button) => {
    button.classList.toggle("active", button.dataset.view === state.view);
  });
  const accounts = $("#accounts");
  accounts.classList.toggle("grid", state.view === "grid");
  accounts.classList.toggle("list", state.view === "list");
  $("#friend-wrap").classList.toggle("collapsed", state.mode !== "lol");
  $<HTMLSelectElement>("#sort").value = state.sort;
}

function render(): void {
  populateRegions();
  renderSchedule();
  renderControls();
  renderAccounts();
  renderDistribution();
}

// ---------------------------------------------------------------- actions

function upsert(account: AccountView): void {
  const id = keyId(keyOf(account));
  const index = state.accounts.findIndex((candidate) => keyId(keyOf(candidate)) === id);
  if (index >= 0) state.accounts[index] = account;
  else state.accounts.push(account);
}

const describe = (error: unknown): string =>
  error instanceof ApiError ? errorText(error.code, error.detail) : errorText("unknown", String(error));

async function guard<T>(work: () => Promise<T>, failTitle: MessageKey): Promise<T | undefined> {
  try {
    return await work();
  } catch (error) {
    toast("error", t(failTitle), describe(error), 5000);
    return undefined;
  }
}

async function copyId(account: AccountView): Promise<void> {
  if ((await guard(() => api.copyAccountId(keyOf(account)), "toast.copyFailed")) !== undefined) {
    toast("success", t("toast.idCopied"), account.accountId);
  }
}

async function copyPassword(account: AccountView): Promise<void> {
  const ttl = await guard(() => api.copyPassword(keyOf(account)), "toast.passFailed");
  if (ttl !== undefined) toast("success", t("toast.passCopied"), t("toast.passCopiedText", { seconds: ttl }));
}

let loginBusy = false;
let activeLogin: LoginOverlay | null = null;

/** Ask before closing an open League client/match that belongs to another account. */
async function confirmCloseLeague(account: AccountView, inGame: boolean): Promise<boolean> {
  const answer = await modal({
    title: t(inGame ? "login.inGameTitle" : "login.closeTitle"),
    body: `<div class="warning${inGame ? " danger" : ""}">${icon("alert", 18)}<p>${esc(
      t(inGame ? "login.inGameText" : "login.closeText", { name: accountLabel(account) }),
    )}</p></div>`,
    actions: [
      { label: t("common.cancel"), value: "cancel" },
      { label: t("login.closeConfirm"), value: "close", kind: inGame ? "danger" : "primary" },
    ],
  });
  return answer === "close";
}

async function login(account: AccountView): Promise<void> {
  if (loginBusy) return;
  loginBusy = true;
  try {
    await runLogin(account);
  } finally {
    loginBusy = false;
    activeLogin = null;
  }
}

async function runLogin(account: AccountView, switchAccount = false, closeRunning = false): Promise<void> {
  const viaRiot = state.settings?.loginMethod !== "previous";
  const key = keyOf(account);

  // Never sign another account in under an open League client or match.
  if (viaRiot && !closeRunning) {
    const status = await guard(() => api.gameStatus(key), "toast.loginFailed");
    if (!status) return;
    if ((status.clientOpen || status.inGame) && !status.sameAccount) {
      if (!(await confirmCloseLeague(account, status.inGame))) return;
      return runLogin(account, true, true);
    }
  }

  const launching = Boolean(viaRiot && state.settings?.launchGame);
  const overlay = openLoginOverlay({
    account,
    tier: rankOf(account, state.mode).tier,
    viaRiot,
    closeLeague: closeRunning,
    switchAccount,
    launchGame: launching,
    onCancel: () => void api.cancelLogin(),
  });
  activeLogin = overlay;
  try {
    const result = await api.login(key, state.mode === "tft", switchAccount, closeRunning);
    if (result === "signedIn") {
      overlay.succeed(t("login.success"), launching ? t("toast.launchingGame") : accountLabel(account));
    } else if (result === "alreadySignedIn") {
      overlay.succeed(t("toast.alreadySignedIn"), launching ? t("toast.launchingGame") : accountLabel(account));
    } else if (result === "typed") {
      if (viaRiot) overlay.finishInClient();
      else overlay.succeed(t("toast.loggedIn"), accountLabel(account));
    } else if (result === "otherAccount") {
      overlay.close();
      const answer = await modal({
        title: t("login.switchTitle"),
        body: `<p class="hint">${esc(t("login.switchText", { name: accountLabel(account) }))}</p>`,
        actions: [
          { label: t("common.cancel"), value: "cancel" },
          { label: t("login.switchConfirm"), value: "switch", kind: "primary" },
        ],
      });
      activeLogin = null;
      if (answer === "switch") await runLogin(account, true, closeRunning);
      return;
    }
  } catch (error) {
    if (error instanceof ApiError && error.code === "login_cancelled") {
      overlay.close();
    } else if (error instanceof ApiError && error.code === "league_running") {
      // The client was opened between the check and the login: ask again.
      overlay.close();
      activeLogin = null;
      return runLogin(account, switchAccount, false);
    } else {
      overlay.fail(describe(error));
    }
  }
}

function select(id: string | null, focus = false): void {
  state.selected = id;
  document.querySelectorAll<HTMLElement>("#accounts > .card").forEach((element) => {
    const selected = element.dataset.key === id;
    element.classList.toggle("selected", selected);
    if (selected && focus) {
      element.focus({ preventScroll: true });
      element.scrollIntoView({ block: "nearest", behavior: reducedMotion() ? "auto" : "smooth" });
    }
  });
}

async function editAccount(account: AccountView): Promise<void> {
  await modal({
    title: t("edit.title"),
    body: `
      <label class="field"><span>${t("edit.riotId")} <em>${t("add.optional")}</em></span>
        <input name="name" value="${esc(account.name)}" placeholder="${esc(t("add.riotIdPh"))}" spellcheck="false" /></label>
      <label class="field"><span>${t("edit.description")}</span>
        <input name="description" value="${esc(account.description)}" placeholder="${esc(t("edit.descriptionPh"))}" /></label>
      <p class="hint">${t("edit.hint")}</p>`,
    actions: [
      { label: t("common.cancel"), value: "cancel" },
      { label: t("common.save"), value: "save", kind: "primary" },
    ],
    onSubmit: async (value, root) => {
      if (value !== "save") return true;
      const name = $<HTMLInputElement>('[name="name"]', root).value;
      const description = $<HTMLInputElement>('[name="description"]', root).value;
      const updated = await guard(() => api.updateAccount(keyOf(account), name, description), "toast.saveFailed");
      if (!updated) return false;
      upsert(updated);
      render();
      toast("success", t("toast.updated"));
      return true;
    },
  });
}

async function deleteAccount(account: AccountView): Promise<void> {
  const answer = await modal({
    title: t("delete.title"),
    body: `
      <div class="confirm-account">
        ${emblem(rankOf(account, state.mode).tier, 44)}
        <div><strong>${esc(accountLabel(account))}</strong><span>${esc(account.accountId)} · ${esc(account.regionDisplay)}</span></div>
      </div>
      <p class="hint">${t("delete.hint")}</p>`,
    actions: [
      { label: t("common.cancel"), value: "cancel" },
      { label: t("delete.confirm"), value: "delete", kind: "danger" },
    ],
  });
  if (answer !== "delete") return;
  const id = keyId(keyOf(account));
  if ((await guard(() => api.deleteAccount(keyOf(account)), "toast.deleteFailed")) === undefined) return;
  const element = document.querySelector<HTMLElement>(`#accounts > .card[data-key="${CSS.escape(id)}"]`);
  if (element) await leave(element, "card-out");
  state.accounts = state.accounts.filter((candidate) => keyId(keyOf(candidate)) !== id);
  if (state.selected === id) state.selected = null;
  render();
  toast("success", t("toast.deleted"), accountLabel(account));
}

function openMenu(account: AccountView, x: number, y: number): void {
  select(keyId(keyOf(account)));
  contextMenu(x, y, [
    ...(account.name ? [{
      label: t(state.mode === "lol" ? "menu.opgg" : "menu.opggTft"),
      icon: "external-link",
      run: () => void guard(() => api.openProfile(keyOf(account), state.mode === "tft"), "toast.openFailed"),
    }] : []),
    { label: t("menu.edit"), icon: "pencil", run: () => void editAccount(account) },
    "sep",
    { label: t("menu.delete"), icon: "trash", danger: true, hint: "Del", run: () => void deleteAccount(account) },
  ], "account");
}

async function refreshAll(): Promise<void> {
  if (state.refreshing || state.schedule?.running) return;
  state.refreshing = true;
  state.refreshTotal = 0;
  state.refreshDone = 0;
  renderSchedule();
  const count = await guard(() => api.refreshRanks(), "toast.refreshFailed");
  if (count === undefined) {
    state.refreshing = false;
    renderSchedule();
    return;
  }
  state.refreshTotal = count;
  renderSchedule();
}

// ---------------------------------------------------------------- updates

const DISMISSED_UPDATE = "leagueaccounts.dismissedUpdate";

function showUpdate(release: Release, force = false): void {
  try {
    if (!force && localStorage.getItem(DISMISSED_UPDATE) === release.version) return;
  } catch {
    // Storage unavailable: show it.
  }
  document.querySelector(".update-banner")?.remove();
  const banner = fragment(`
    <div class="update-banner" role="status">
      <div class="update-body">
        <strong>${esc(t("update.available"))}</strong>
        <span>${esc(t("update.text", { version: release.version }))}</span>
      </div>
      <button class="btn btn-primary" data-update="open">${icon("download", 15)}${esc(t("update.download"))}</button>
      <button class="icon-btn" data-update="later" title="${esc(t("update.later"))}">${icon("x")}</button>
    </div>`);
  $("#toasts").before(banner);
  banner.addEventListener("click", (event) => {
    const action = (event.target as HTMLElement).closest<HTMLElement>("[data-update]")?.dataset.update;
    if (action === "open") void guard(() => api.openRelease(release.url), "toast.openFailed");
    if (action === "later") {
      try {
        localStorage.setItem(DISMISSED_UPDATE, release.version);
      } catch {
        // Non-essential.
      }
      void leave(banner, "toast-out");
    }
  });
}

async function checkUpdateNow(button: HTMLButtonElement, output: HTMLElement): Promise<void> {
  button.disabled = true;
  output.textContent = "…";
  const status = await guard(() => api.checkUpdate(), "toast.updateFailed");
  button.disabled = false;
  if (!status) {
    output.textContent = "";
    return;
  }
  if (status.newer && status.latest) {
    output.textContent = t("update.text", { version: status.latest.version });
    showUpdate(status.latest, true);
  } else {
    output.textContent = status.latest ? t("update.latest") : t("update.none");
  }
}

// ---------------------------------------------------------------- schedule & settings

function renderSchedule(): void {
  const root = $<HTMLButtonElement>("#schedule");
  const schedule = state.schedule;
  const running = Boolean(schedule?.running || state.refreshing);
  let text: string;
  let progress = 0;
  if (running) {
    text = state.refreshTotal
      ? t("refresh.busy", { done: state.refreshDone, total: state.refreshTotal })
      : t("schedule.running");
    progress = state.refreshTotal ? state.refreshDone / state.refreshTotal : 1;
  } else if (!schedule || !schedule.enabled) {
    text = t("schedule.off");
  } else if (schedule.nextAt) {
    const remaining = schedule.nextAt - Date.now();
    const total = schedule.intervalMinutes * 60_000;
    progress = Math.min(1, Math.max(0, 1 - remaining / total));
    if (remaining <= 30_000) {
      text = t("schedule.soon");
    } else {
      const minutes = Math.round(remaining / 60_000);
      const relative = new Intl.RelativeTimeFormat(lang(), { numeric: "auto" });
      text = t("schedule.next", {
        time: minutes >= 60 ? relative.format(Math.round(minutes / 60), "hour") : `${Math.max(1, minutes)} ${t("settings.minutes")}`,
      });
    }
  } else {
    text = t("schedule.soon");
  }
  root.classList.toggle("off", !schedule?.enabled && !running);
  root.classList.toggle("running", running);
  root.disabled = running;
  root.setAttribute("aria-busy", String(running));
  root.style.setProperty("--p", String(progress));
  $(".schedule-text", root).textContent = text;
}

async function openSettings(): Promise<void> {
  const current = await guard(() => api.getSettings(), "toast.settingsFailed");
  if (!current) return;
  const languageOptions = LANGUAGES.map(
    (language) => `<option value="${language.code}"${language.code === lang() ? " selected" : ""}>${esc(language.name)}</option>`,
  ).join("");
  const toggle = (name: string, checked: boolean, label: string, hint = "", disabled = false) => `
    <label class="toggle-row${disabled ? " disabled" : ""}">
      <span class="toggle-text"><strong>${esc(label)}</strong>${hint ? `<span>${esc(hint)}</span>` : ""}</span>
      <input type="checkbox" name="${name}" ${checked ? "checked" : ""} ${disabled ? "disabled" : ""}/>
      <span class="switch"></span>
    </label>`;
  await modal({
    title: t("settings.title"),
    wide: true,
    body: `
      <section class="settings-section">
        <h3>${esc(t("settings.general"))}</h3>
        <label class="field inline"><span>${esc(t("settings.language"))}</span>
          <div class="select-wrap"><select name="language">${languageOptions}</select></div></label>
        ${toggle("launchAtStartup", current.launchAtStartup, t("settings.startup"), t("settings.startupHint"))}
        ${toggle("startMinimized", current.startMinimized, t("settings.minimized"), t("settings.minimizedHint"))}
      </section>
      <section class="settings-section">
        <h3>${esc(t("settings.refresh"))}</h3>
        ${toggle("autoRefresh", current.autoRefresh, t("settings.autoRefresh"))}
        <div class="interval-row">
          <span>${esc(t("settings.interval"))}</span>
          <div class="interval-presets">${[5, 15, 30, 60, 120].map((value) => `<button type="button" data-interval="${value}">${value}</button>`).join("")}</div>
          <label class="interval-input"><input name="intervalMinutes" type="number" min="5" max="1440" step="5" value="${current.intervalMinutes}"/><span>${esc(t("settings.minutes"))}</span></label>
        </div>
        <p class="hint">${esc(t("settings.intervalHint", { min: 5, max: 1440 }))}</p>
      </section>
      <section class="settings-section">
        <h3>${esc(t("settings.login"))}</h3>
        <label class="radio-row"><input type="radio" name="loginMethod" value="riot" ${current.loginMethod === "riot" ? "checked" : ""}/>
          <span class="toggle-text"><strong>${esc(t("settings.methodRiot"))}</strong><span>${esc(current.riotClientFound ? t("settings.methodRiotHint") : t("settings.clientMissing"))}</span></span></label>
        <label class="radio-row"><input type="radio" name="loginMethod" value="previous" ${current.loginMethod === "previous" ? "checked" : ""}/>
          <span class="toggle-text"><strong>${esc(t("settings.methodPrevious"))}</strong><span>${esc(t("settings.methodPreviousHint"))}</span></span></label>
        ${toggle("launchGame", current.launchGame, t("settings.launchGame"), t("settings.launchGameHint"))}
      </section>
      <section class="settings-section">
        <h3>${esc(t("settings.updates"))}</h3>
        ${toggle("checkUpdates", current.checkUpdates, t("settings.checkUpdates"))}
        <div class="update-row">
          <span>${esc(t("settings.version", { version: $("#version").textContent?.replace(/^v/, "") ?? "" }))}</span>
          <span class="update-result"></span>
          <button type="button" class="btn btn-ghost" data-check-update>${icon("refresh-cw", 14)}${esc(t("settings.checkNow"))}</button>
        </div>
      </section>`,
    actions: [
      { label: t("common.cancel"), value: "cancel" },
      { label: t("common.save"), value: "save", kind: "primary" },
    ],
    onOpen: (root) => {
      const interval = $<HTMLInputElement>('[name="intervalMinutes"]', root);
      const autoRefresh = $<HTMLInputElement>('[name="autoRefresh"]', root);
      const launchGame = $<HTMLInputElement>('[name="launchGame"]', root);
      const sync = () => {
        root.querySelectorAll<HTMLElement>("[data-interval]").forEach((button) => button.classList.toggle("active", button.dataset.interval === interval.value));
        root.querySelector(".interval-row")?.classList.toggle("disabled", !autoRefresh.checked);
        const riot = $<HTMLInputElement>('[name="loginMethod"][value="riot"]', root).checked;
        launchGame.disabled = !riot;
        launchGame.closest(".toggle-row")?.classList.toggle("disabled", !riot);
      };
      root.addEventListener("click", (event) => {
        const check = (event.target as HTMLElement).closest<HTMLButtonElement>("[data-check-update]");
        if (check) void checkUpdateNow(check, $(".update-result", root));
        const preset = (event.target as HTMLElement).closest<HTMLElement>("[data-interval]");
        if (preset) {
          interval.value = preset.dataset.interval ?? "30";
          sync();
        }
      });
      root.addEventListener("input", sync);
      root.addEventListener("change", sync);
      sync();
    },
    onSubmit: async (value, root) => {
      if (value !== "save") return true;
      const checked = (name: string) => $<HTMLInputElement>(`[name="${name}"]`, root).checked;
      const minutes = Math.round(Number($<HTMLInputElement>('[name="intervalMinutes"]', root).value) || 30);
      const saved = await guard(
        () =>
          api.updateSettings({
            autoRefresh: checked("autoRefresh"),
            intervalMinutes: Math.min(1440, Math.max(5, minutes)),
            startMinimized: checked("startMinimized"),
            launchAtStartup: checked("launchAtStartup"),
            loginMethod: $<HTMLInputElement>('[name="loginMethod"]:checked', root).value as Settings["loginMethod"],
            launchGame: checked("launchGame"),
            checkUpdates: checked("checkUpdates"),
          }),
        "toast.settingsFailed",
      );
      if (!saved) return false;
      state.settings = saved;
      const language = $<HTMLSelectElement>('[name="language"]', root).value as Lang;
      if (language !== lang()) changeLanguage(language);
      toast("success", t("settings.saved"));
      return true;
    },
  });
}

// ---------------------------------------------------------------- add drawer

function openAddDrawer(tab: "single" | "multi" = "single"): void {
  if (document.querySelector(".drawer-overlay")) return;
  const regionOptions = state.regions
    .map((region) => `<option${region === lastRegion() ? " selected" : ""}>${esc(region)}</option>`)
    .join("");
  const overlay = fragment(`
    <div class="drawer-overlay">
      <aside class="drawer" role="dialog" aria-modal="true" aria-label="${esc(t("add.title"))}">
        <header class="drawer-head">
          <div>
            <h2>${t("add.title")}</h2>
            <p>${t("add.subtitle")}</p>
          </div>
          <button class="icon-btn" data-close aria-label="${esc(t("common.close"))}">${icon("x")}</button>
        </header>
        <div class="tabs" role="tablist">
          <span class="tab-pill"></span>
          <button role="tab" data-tab="single">${t("add.tabSingle")}</button>
          <button role="tab" data-tab="multi">${t("add.tabMulti")}</button>
        </div>
        <div class="tab-panels">
          <form class="tab-panel" data-panel="single" novalidate>
            <label class="field"><span>${t("add.accountId")}</span>
              <input name="accountId" autocomplete="off" spellcheck="false" placeholder="${esc(t("add.accountIdPh"))}" /></label>
            <label class="field"><span>${t("edit.riotId")} <em>${t("add.optional")}</em></span>
              <input name="name" autocomplete="off" spellcheck="false" placeholder="${esc(t("add.riotIdPh"))}" /></label>
            <div class="field-row">
              <label class="field"><span>${t("add.region")}</span><div class="select-wrap full"><select name="region">${regionOptions}</select></div></label>
            </div>
            <label class="field"><span>${t("add.password")}</span>
              <div class="password">
                <input name="password" type="password" autocomplete="new-password" />
                <button type="button" class="icon-btn" data-reveal title="${esc(t("add.show"))}">${icon("eye", 15)}</button>
              </div></label>
            <label class="field"><span>${t("edit.description")} <em>${t("add.optional")}</em></span>
              <input name="description" autocomplete="off" placeholder="${esc(t("edit.descriptionPh"))}" /></label>
            <button class="btn btn-primary btn-block" type="submit">${icon("plus")} ${t("add.submit")}</button>
          </form>
          <form class="tab-panel" data-panel="multi" novalidate>
            <label class="field"><span>${t("add.multiLabel")}</span>
              <textarea name="text" rows="9" spellcheck="false" placeholder="${esc(t("add.multiPh"))}"></textarea></label>
            <p class="hint">${t("add.multiHint", { format: "<code>AccountID--Name#TAG--password</code>", alt: "<code>---</code>" })} <span class="multi-count"></span></p>
            <label class="field"><span>${t("add.multiRegion")}</span><div class="select-wrap full"><select name="region">${regionOptions}</select></div></label>
            <button class="btn btn-primary btn-block" type="submit">${icon("users")} ${t("add.multiSubmit")}</button>
          </form>
        </div>
      </aside>
    </div>`);
  $("#drawer-root").append(overlay);
  const drawer = $(".drawer", overlay);

  const setTab = (name: string) => {
    drawer.dataset.tab = name;
    drawer.querySelectorAll<HTMLElement>("[data-tab]").forEach((button) => {
      button.setAttribute("aria-selected", String(button.dataset.tab === name));
    });
    drawer.querySelector<HTMLInputElement>(`[data-panel="${name}"] input, [data-panel="${name}"] textarea`)?.focus();
  };
  setTab(tab);
  drawer.querySelectorAll<HTMLElement>("[data-tab]").forEach((button) => {
    button.addEventListener("click", () => setTab(button.dataset.tab ?? "single"));
  });

  const close = () => {
    document.removeEventListener("keydown", onKey, true);
    void leave(overlay, "drawer-out");
  };
  const onKey = (event: KeyboardEvent) => {
    if (event.key === "Escape") {
      event.stopPropagation();
      close();
    }
  };
  document.addEventListener("keydown", onKey, true);
  overlay.addEventListener("mousedown", (event) => {
    if (event.target === overlay) close();
  });
  $("[data-close]", overlay).addEventListener("click", close);

  const reveal = $("[data-reveal]", overlay);
  reveal.addEventListener("click", () => {
    const input = $<HTMLInputElement>('[name="password"]', overlay);
    const hidden = input.type === "password";
    input.type = hidden ? "text" : "password";
    reveal.innerHTML = icon(hidden ? "eye-off" : "eye", 15);
  });

  const textarea = $<HTMLTextAreaElement>("textarea", overlay);
  const counter = $(".multi-count", overlay);
  textarea.addEventListener("input", () => {
    const lines = textarea.value.split("\n").filter((line) => line.trim());
    const valid = lines.filter((line) => parseAccountLine(line));
    counter.textContent = lines.length ? t("add.multiCount", { valid: valid.length, total: lines.length }) : "";
  });

  const single = $<HTMLFormElement>('[data-panel="single"]', overlay);
  single.addEventListener("submit", async (event) => {
    event.preventDefault();
    const data = new FormData(single);
    const value = (name: string) => String(data.get(name) ?? "");
    const missing = ["accountId", "password"].filter((name) => !value(name).trim());
    single.querySelectorAll("input").forEach((input) => input.classList.toggle("invalid", missing.includes(input.name)));
    if (missing.length) {
      shake(drawer);
      return;
    }
    const button = $<HTMLButtonElement>('button[type="submit"]', single);
    button.disabled = true;
    rememberRegion(value("region"));
    const added = await guard(
      () =>
        api.addAccount({
          accountId: value("accountId"),
          name: value("name"),
          region: value("region"),
          password: value("password"),
          description: value("description"),
        }),
      "toast.addFailed",
    );
    button.disabled = false;
    if (!added) return;
    upsert(added);
    state.selected = keyId(keyOf(added));
    render();
    single.reset();
    $<HTMLSelectElement>('[name="region"]', single).value = lastRegion();
    $<HTMLInputElement>('[name="accountId"]', single).focus();
    toast("success", t("toast.added"), added.name ? t("toast.addedText", { name: added.name }) : added.accountId);
  });

  const multi = $<HTMLFormElement>('[data-panel="multi"]', overlay);
  multi.addEventListener("submit", async (event) => {
    event.preventDefault();
    const data = new FormData(multi);
    const text = String(data.get("text") ?? "");
    const region = String(data.get("region") ?? "");
    if (!text.trim()) {
      shake(drawer);
      return;
    }
    rememberRegion(region);
    const result = await guard(() => api.multiAdd(text, region), "toast.multiFailed");
    if (!result) return;
    state.accounts = await api.listAccounts();
    render();
    textarea.value = "";
    counter.textContent = "";
    toast(
      result.added ? "success" : "info",
      t("toast.multiAdded", { count: result.added }),
      result.skipped ? t("toast.multiSkipped", { count: result.skipped }) : "",
    );
    if (result.added) close();
  });
}

function shake(element: HTMLElement): void {
  element.classList.remove("shake");
  void element.offsetWidth;
  element.classList.add("shake");
}

function lastRegion(): string {
  try {
    return localStorage.getItem("leagueaccounts.region") ?? "EUW";
  } catch {
    return "EUW";
  }
}

function rememberRegion(region: string): void {
  try {
    localStorage.setItem("leagueaccounts.region", region);
  } catch {
    // Non-essential.
  }
}

// ---------------------------------------------------------------- side actions

async function exportData(): Promise<void> {
  const answer = await modal({
    title: t("export.title"),
    body: `<div class="warning">${icon("alert", 18)}<p>${t("export.warning")}</p></div>`,
    actions: [
      { label: t("common.cancel"), value: "cancel" },
      { label: t("export.confirm"), value: "export", kind: "primary" },
    ],
  });
  if (answer !== "export") return;
  const path = await guard(() => api.exportData(t("export.dialog")), "toast.exportFailed");
  if (path) toast("success", t("toast.exported"), path, 4500);
}

async function importData(): Promise<void> {
  const result = await guard(() => api.importData(t("import.dialog")), "toast.importFailed");
  if (!result) return;
  state.accounts = await api.listAccounts();
  render();
  toast(
    "success",
    t("toast.imported", { count: result.added }),
    result.skipped ? t("toast.importSkipped", { count: result.skipped }) : "",
  );
}

function showShortcuts(): void {
  const rows: [string, string][] = [
    ["Ctrl + Shift + V", t("sc.login")],
    ["Ctrl + C", t("sc.copy")],
    ["Delete", t("sc.delete")],
    ["↑ ↓ / ← →", t("sc.nav")],
    ["Ctrl + F", t("sc.search")],
    ["Ctrl + N", t("sc.add")],
    ["Ctrl + R / F5", t("sc.refresh")],
    ["Ctrl + 1 / 2", t("sc.mode")],
    ["Ctrl + ,", t("sc.settings")],
    [t("sc.dblclick"), t("sc.dblclickText")],
    [t("sc.rightclick"), t("card.more")],
  ];
  void modal({
    title: t("shortcuts.title"),
    body: `<div class="shortcuts">${rows.map(([keys, label]) => `<kbd>${esc(keys)}</kbd><span>${esc(label)}</span>`).join("")}</div>`,
    actions: [{ label: t("common.ok"), value: "ok", kind: "primary" }],
  });
}

// ---------------------------------------------------------------- language

/** Translate static markup and the option lists built in code. */
function applyLanguage(): void {
  applyStatic();
  const friendTier = $<HTMLSelectElement>("#friend-tier");
  const selectedTier = friendTier.value || state.friendTier;
  friendTier.innerHTML =
    `<option value="all">${esc(t("toolbar.all"))}</option>` +
    ["Master", "Diamond", "Emerald", "Platinum", "Gold", "Silver", "Bronze", "Iron"]
      .map((tier) => `<option value="${tier}">${esc(tierName(tier))}</option>`)
      .join("");
  friendTier.value = selectedTier;
}

function openLangMenu(): void {
  const rect = $("#lang").getBoundingClientRect();
  contextMenu(
    rect.left,
    rect.bottom + 6,
    LANGUAGES.map((language) => ({
      label: language.name,
      icon: language.code === lang() ? "check" : "languages",
      run: () => changeLanguage(language.code),
    })),
    "language",
  );
}

function changeLanguage(next: Lang): void {
  if (next === lang()) return;
  setLang(next);
  applyLanguage();
  render();
}

// ---------------------------------------------------------------- wiring

function wire(): void {
  document.querySelectorAll<HTMLButtonElement>(".mode-btn").forEach((button) => {
    button.addEventListener("click", () => setMode(button.dataset.mode as Mode));
  });
  document.querySelectorAll<HTMLButtonElement>("[data-view]").forEach((button) => {
    button.addEventListener("click", () => {
      state.view = button.dataset.view as View;
      savePrefs();
      firstRender = true;
      $("#accounts").innerHTML = "";
      render();
    });
  });

  const search = $<HTMLInputElement>("#search");
  search.addEventListener("input", () => {
    state.search = search.value;
    renderAccounts();
  });

  const regionFilter = $<HTMLSelectElement>("#region-filter");
  regionFilter.addEventListener("change", () => {
    state.regionFilter = regionFilter.value;
    render();
  });
  const friendTier = $<HTMLSelectElement>("#friend-tier");
  const friendDivision = $<HTMLSelectElement>("#friend-division");
  friendDivision.innerHTML = ["I", "II", "III", "IV"].map((division) => `<option>${division}</option>`).join("");
  const onFriend = () => {
    state.friendTier = friendTier.value;
    state.friendDivision = friendDivision.value;
    friendDivision.disabled = state.friendTier === "all" || state.friendTier === "Master";
    render();
  };
  friendTier.addEventListener("change", onFriend);
  friendDivision.addEventListener("change", onFriend);
  friendDivision.disabled = true;

  const sort = $<HTMLSelectElement>("#sort");
  sort.addEventListener("change", () => {
    state.sort = sort.value as Sort;
    savePrefs();
    renderAccounts();
  });

  $("#schedule").addEventListener("click", () => void refreshAll());
  $("#open-add").addEventListener("click", () => openAddDrawer());
  $("#lang").addEventListener("click", openLangMenu);
  document.querySelectorAll<HTMLButtonElement>("[data-action]").forEach((button) => {
    button.addEventListener("click", () => {
      const action = button.dataset.action;
      if (action === "import") void importData();
      if (action === "export") void exportData();
      if (action === "logs") void guard(() => api.openLogsFolder(), "toast.logsFailed");
      if (action === "shortcuts") showShortcuts();
      if (action === "settings") void openSettings();
    });
  });

  $("#distribution").addEventListener("click", (event) => {
    const row = (event.target as HTMLElement).closest<HTMLElement>("[data-tier]");
    if (!row) return;
    state.tierFilter = state.tierFilter === row.dataset.tier ? null : (row.dataset.tier ?? null);
    render();
  });

  $("#empty").addEventListener("click", (event) => {
    const button = (event.target as HTMLElement).closest<HTMLElement>("[data-empty]");
    if (button?.dataset.empty === "add") openAddDrawer();
    if (button?.dataset.empty === "clear") clearFilters();
  });

  const accounts = $("#accounts");
  accounts.addEventListener("click", (event) => {
    const card = (event.target as HTMLElement).closest<HTMLElement>(".card");
    if (!card) return;
    const account = byId(card.dataset.key ?? "");
    if (!account) return;
    select(card.dataset.key ?? null);
    const action = (event.target as HTMLElement).closest<HTMLElement>("[data-act]")?.dataset.act;
    if (action === "login") void login(account);
    if (action === "copy-id") void copyId(account);
    if (action === "copy-pass") void copyPassword(account);
    if (action === "menu") {
      const rect = (event.target as HTMLElement).closest("button")!.getBoundingClientRect();
      openMenu(account, rect.left, rect.bottom + 6);
    }
    if (action === "edit") void editAccount(account);
  });
  accounts.addEventListener("dblclick", (event) => {
    if ((event.target as HTMLElement).closest("button")) return;
    const card = (event.target as HTMLElement).closest<HTMLElement>(".card");
    const account = card && byId(card.dataset.key ?? "");
    if (account) void editAccount(account);
  });
  accounts.addEventListener("contextmenu", (event) => {
    const card = (event.target as HTMLElement).closest<HTMLElement>(".card");
    const account = card && byId(card.dataset.key ?? "");
    if (!account) return;
    event.preventDefault();
    openMenu(account, event.clientX, event.clientY);
  });
  // Pointer-tracked highlight on cards.
  accounts.addEventListener("pointermove", (event) => {
    const card = (event.target as HTMLElement).closest<HTMLElement>(".card");
    if (!card) return;
    const rect = card.getBoundingClientRect();
    card.style.setProperty("--mx", `${event.clientX - rect.left}px`);
    card.style.setProperty("--my", `${event.clientY - rect.top}px`);
  });

  document.addEventListener("contextmenu", (event) => {
    if (!(event.target as HTMLElement).closest("input, textarea")) event.preventDefault();
  });
  document.addEventListener("keydown", onKeyDown);
  window.addEventListener("scroll", closeMenu, true);
}

function clearFilters(): void {
  state.search = "";
  state.regionFilter = "all";
  state.friendTier = "all";
  state.tierFilter = null;
  $<HTMLInputElement>("#search").value = "";
  $<HTMLSelectElement>("#region-filter").value = "all";
  $<HTMLSelectElement>("#friend-tier").value = "all";
  $<HTMLSelectElement>("#friend-division").disabled = true;
  render();
}

function setMode(mode: Mode): void {
  if (state.mode === mode) return;
  state.mode = mode;
  savePrefs();
  if (state.tierFilter) state.tierFilter = null;
  const container = $("#accounts");
  container.classList.remove("mode-swap");
  void container.offsetWidth;
  container.classList.add("mode-swap");
  render();
}

function populateRegions(): void {
  const regions = [...new Set(state.accounts.map((account) => account.regionDisplay))];
  const all = state.regions.filter((region) => regions.includes(region));
  const select = $<HTMLSelectElement>("#region-filter");
  select.innerHTML = `<option value="all">${esc(t("toolbar.allRegions"))}</option>` + all.map((region) => `<option>${esc(region)}</option>`).join("");
  if (!all.includes(state.regionFilter)) state.regionFilter = "all";
  select.value = state.regionFilter;
}

function moveSelection(step: number, columns: boolean): void {
  const cards = [...document.querySelectorAll<HTMLElement>("#accounts > .card")];
  if (!cards.length) return;
  let index = cards.findIndex((card) => card.dataset.key === state.selected);
  let delta = step;
  if (columns && state.view === "grid" && cards.length > 1) {
    const top = cards[0].getBoundingClientRect().top;
    const perRow = cards.filter((card) => Math.abs(card.getBoundingClientRect().top - top) < 4).length;
    delta = step * perRow;
  }
  index = index < 0 ? 0 : Math.max(0, Math.min(cards.length - 1, index + delta));
  select(cards[index].dataset.key ?? null, true);
}

function onKeyDown(event: KeyboardEvent): void {
  const target = event.target as HTMLElement;
  const typing = target.matches("input, textarea, select");
  const overlayOpen = Boolean(document.querySelector(".overlay, .drawer-overlay, .login-overlay"));
  const ctrl = event.ctrlKey || event.metaKey;
  const key = event.key.toLowerCase();

  if ((ctrl && key === "r") || event.key === "F5") {
    event.preventDefault();
    if (!overlayOpen) void refreshAll();
    return;
  }
  if (overlayOpen) return;
  if (ctrl && key === "f") {
    event.preventDefault();
    $<HTMLInputElement>("#search").select();
    return;
  }
  if (ctrl && key === ",") {
    event.preventDefault();
    void openSettings();
    return;
  }
  if (ctrl && key === "n") {
    event.preventDefault();
    openAddDrawer();
    return;
  }
  if (ctrl && (key === "1" || key === "2")) {
    event.preventDefault();
    setMode(key === "1" ? "lol" : "tft");
    return;
  }
  const selected = state.selected ? byId(state.selected) : undefined;
  if (ctrl && event.shiftKey && key === "v" && selected) {
    event.preventDefault();
    void login(selected);
    return;
  }
  if (typing) {
    if (event.key === "Escape" && target.id === "search") {
      target.blur();
    }
    return;
  }
  if (ctrl && key === "c" && selected && !window.getSelection()?.toString()) {
    event.preventDefault();
    const id = state.selected ?? "";
    if (state.copyStage.id !== id) state.copyStage = { id, count: 0 };
    void (state.copyStage.count % 2 === 0 ? copyId(selected) : copyPassword(selected));
    state.copyStage.count++;
    return;
  }
  if (event.key === "Delete" && selected) {
    event.preventDefault();
    void deleteAccount(selected);
    return;
  }
  if (event.key === "Escape") {
    select(null);
    return;
  }
  const arrows: Record<string, [number, boolean]> = {
    ArrowDown: [1, true],
    ArrowUp: [-1, true],
    ArrowRight: [1, false],
    ArrowLeft: [-1, false],
  };
  if (event.key in arrows) {
    event.preventDefault();
    const [step, columns] = arrows[event.key];
    moveSelection(step, columns && state.view === "grid");
  }
}

async function wireWindow(): Promise<void> {
  const { getCurrentWindow } = await import("@tauri-apps/api/window");
  const appWindow = getCurrentWindow();
  const buttons: Record<string, [string, () => Promise<void>]> = {
    minimize: ["minus", () => appWindow.minimize()],
    maximize: ["square", () => appWindow.toggleMaximize()],
    close: ["x", () => appWindow.close()],
  };
  document.querySelectorAll<HTMLButtonElement>("[data-win]").forEach((button) => {
    const [name, run] = buttons[button.dataset.win ?? ""];
    button.innerHTML = icon(name, name === "square" ? 12 : 15);
    button.addEventListener("click", () => void run());
  });
}

async function wireEvents(): Promise<void> {
  await events.onLoginStep((step) => activeLogin?.step(step));
  await events.onUpdateAvailable((release) => showUpdate(release));
  await events.onRefreshStarted(({ total }) => {
    state.refreshing = true;
    state.refreshTotal = total;
    state.refreshDone = 0;
    renderSchedule();
  });
  await events.onSchedule((schedule) => {
    state.schedule = schedule;
    renderSchedule();
  });
  await events.onRankPending((keys: Key[]) => {
    keys.forEach((key) => state.pending.add(keyId(key)));
    renderAccounts();
  });
  await events.onRankUpdate((account) => {
    state.pending.delete(keyId(keyOf(account)));
    upsert(account);
    if (state.refreshing) state.refreshDone = Math.min(state.refreshTotal, state.refreshDone + 1);
    renderAccounts();
    renderDistribution();
    renderSchedule();
  });
  await events.onRefreshDone(({ error, exclusive, auto }) => {
    if (error) toast("error", t("toast.ranksSaveFailed"), describe(new ApiError(error.code, error.detail)), 5000);
    // Only a full refresh ends the progress state; single-account refreshes
    // (after adding or renaming) may finish while it is still running.
    if (exclusive && state.refreshing) {
      state.refreshing = false;
      state.pending.clear();
      if (!error && auto) toast("info", t("toast.autoRefreshed"), t("toast.ranksUpdatedText", { count: state.refreshTotal }), 2600);
      else if (!error) toast("success", t("toast.ranksUpdated"), t("toast.ranksUpdatedText", { count: state.refreshTotal }));
    }
    render();
  });
}

const IN_TAURI = "__TAURI_INTERNALS__" in window;

/** Dev-only: `?scene=` opens a screen with demo data (used for README screenshots). */
function runDemoScene(): void {
  document.querySelectorAll<HTMLButtonElement>("[data-win]").forEach((button) => {
    const names: Record<string, string> = { minimize: "minus", maximize: "square", close: "x" };
    button.innerHTML = icon(names[button.dataset.win ?? ""], button.dataset.win === "maximize" ? 12 : 15);
  });
  const scene = new URLSearchParams(location.search).get("scene");
  const byName = (part: string) => state.accounts.find((account) => account.name.includes(part))!;
  const wait = (ms: number) => new Promise((resolve) => setTimeout(resolve, ms));
  void (async () => {
    await wait(300);
    if (scene === "tft") setMode("tft");
    if (scene === "list") {
      state.view = "list";
      $("#accounts").innerHTML = "";
      render();
    }
    if (scene === "settings") void openSettings();
    if (scene === "add") openAddDrawer();
    if (scene === "login") void login(byName("Hextech"));
    if (scene === "update") showUpdate({ version: "3.1.0", name: "v3.1.0", url: "", notes: "", publishedAt: "" }, true);
    if (scene === "menu") {
      const card = document.querySelector<HTMLElement>("#accounts > .card:nth-child(2)")!;
      const rect = card.getBoundingClientRect();
      openMenu(byName("Lux"), rect.left + 180, rect.top + 60);
    }
  })();
}

async function start(): Promise<void> {
  if (!IN_TAURI && import.meta.env.DEV) (await import("./mock")).installMock();
  loadPrefs();
  setLang(lang());
  applyLanguage();
  installEmblemDefs(document.getElementById("emblem-defs") as unknown as SVGDefsElement);
  hydrateIcons();
  wire();
  const boot = await api.bootstrap();
  state.accounts = boot.accounts;
  state.regions = boot.regions;
  state.settings = boot.settings;
  state.schedule = boot.schedule;
  $("#version").textContent = `v${boot.version}`;
  render();
  if (boot.loadError) toast("error", t("toast.loadFailed"), errorText(boot.loadError.code, boot.loadError.detail), 8000);
  if (!boot.loggingAvailable) toast("info", t("toast.noLogging"), t("toast.noLoggingText"));
  await Promise.all([wireEvents(), IN_TAURI ? wireWindow() : Promise.resolve()]);
  renderSchedule();
  setInterval(renderSchedule, 20_000);
  if (!IN_TAURI && import.meta.env.DEV) runDemoScene();
  document.body.classList.add("ready");
}

start().catch((error) => {
  document.body.classList.add("ready");
  toast("error", t("toast.startFailed"), String(error), 10000);
});
