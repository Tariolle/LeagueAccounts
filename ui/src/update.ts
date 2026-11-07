// Automatic updates: the launch update screen, the restart notice once an
// update was installed in the background, and the manual download banner
// when it can't be installed automatically.
import { ApiError, api, events, type Release, type UpdateStatus } from "./api";
import { $, esc, fragment, leave, toast } from "./dom";
import { errorText, lang, t } from "./i18n";
import { icon } from "./icons";

const DISMISSED_UPDATE = "leagueaccounts.dismissedUpdate";
/** Hold the loading screen up to this long from the start of the launch check. */
const LAUNCH_HOLD_MS = 3000;
const RESTART_SECONDS = 10;
const IDLE_POLL_MS = 15_000;

let current: UpdateStatus | null = null;
/** A dialog, menu, login or rank refresh is in progress. */
let busy: () => boolean = () => false;
const changed = new Set<() => void>();

let screen: HTMLElement | null = null;
let revealAfterScreen: (() => Promise<void>) | null = null;
/** The app was shown before the download started: track it in a banner. */
let progressBanner: HTMLElement | null = null;
/** "Continue in the background" or dismissing the progress banner. */
let silent = false;
let holding = false;
let notice: HTMLElement | null = null;
let watcher: number | undefined;
/** "Later" on the restart notice: the new version starts next launch. */
let declined = false;

const describe = (error: unknown): string =>
  error instanceof ApiError ? errorText(error.code, error.detail) : errorText("unknown", String(error));

/** Subscribe before reading the state, so no change in between is missed. */
export async function wireUpdates(options: { busy: () => boolean }): Promise<void> {
  busy = options.busy;
  await events.onUpdateStatus(apply);
  apply(await api.updateStatus());
}

function apply(status: UpdateStatus): void {
  if (current && status.seq < current.seq) return;
  const previous = current;
  current = status;
  changed.forEach((listener) => listener());
  renderScreen();
  renderProgress();
  renderResult();
  const { phase, release } = status;
  if (phase === "downloading" && status.launch && !screen && !silent) showProgress();
  if (phase !== "downloading") removeProgress();
  if ((phase === "available" || phase === "failed") && previous?.phase !== phase && release) {
    if (screen) void closeScreen();
    showUpdateBanner(release);
  }
  if (phase === "ready" && previous?.phase !== "ready") {
    if (screen) void restart();
    else if (!holding) watchForRestart();
  }
}

// ---------------------------------------------------------------- launch screen

const launchPending = (): boolean =>
  !!current?.launch && (current.phase === "checking" || (current.phase === "idle" && current.seq === 0));

/**
 * Hold the loading screen briefly for the launch check. When it found an update,
 * show the update screen instead of the app and return true; `reveal` then runs
 * if the user continues in the background or the install fails.
 */
export async function launchUpdate(reveal: () => Promise<void>): Promise<boolean> {
  holding = true;
  // Start the budget here, not at navigation: a cold WebView and a large
  // account list can take longer than the hold by the time this runs.
  if (launchPending()) {
    await new Promise<void>((resolve) => {
      const finish = () => {
        changed.delete(check);
        clearTimeout(timer);
        resolve();
      };
      const check = () => {
        if (!launchPending()) finish();
      };
      const timer = setTimeout(finish, LAUNCH_HOLD_MS);
      changed.add(check);
    });
  }
  holding = false;
  const phase = current?.phase;
  if (!current?.launch || (phase !== "downloading" && phase !== "ready")) {
    if (phase === "ready") watchForRestart();
    return false;
  }
  const loader = document.querySelector<HTMLElement>("#startup-loader");
  if (!loader) return false;
  revealAfterScreen = reveal;
  // The app was never shown, so the loading screen is the only progress UI.
  removeProgress();
  loader.setAttribute("aria-label", t("update.screen"));
  screen = fragment(`
    <div class="update-screen">
      <p class="update-screen-title">${esc(t("update.screen"))}</p>
      <div class="update-meter" role="progressbar" aria-valuemin="0" aria-valuemax="100" aria-label="${esc(t("update.screen"))}"><span></span></div>
      <p class="update-screen-detail"></p>
      <button type="button" class="text-button">${esc(t("update.background"))}</button>
    </div>`);
  loader.append(screen);
  $("button", screen).addEventListener("click", () => {
    silent = true;
    void closeScreen();
  });
  renderScreen();
  if (phase === "ready") void restart();
  return true;
}

const megabytes = (bytes: number): string =>
  new Intl.NumberFormat(lang(), { minimumFractionDigits: 1, maximumFractionDigits: 1 }).format(bytes / 1048576);

function renderScreen(): void {
  if (!screen || !current) return;
  const installed = current.phase === "ready" || current.phase === "restarting";
  const fraction = installed ? 1 : current.total ? Math.min(1, current.received / current.total) : 0;
  // CSSOM, not a style attribute: the packaged CSP blocks inline style attributes.
  $("span", $(".update-meter", screen)).style.transform = `scaleX(${fraction})`;
  $(".update-meter", screen).setAttribute("aria-valuenow", String(Math.floor(fraction * 100)));
  $(".update-screen-detail", screen).textContent = installed
    ? t("update.applying")
    : current.total
      ? `${megabytes(current.received)} / ${megabytes(current.total)} MB`
      : "";
}

/**
 * The launch check outlived the loading screen: show the download where the
 * app already is, so a slow check still has visible progress.
 */
function showProgress(): void {
  // Only after the app is visible: before that, launchUpdate owns the screen.
  if (progressBanner || !current?.release || !document.body.classList.contains("ready")) return;
  document.querySelector(".update-banner")?.remove();
  const label = t("update.downloading", { version: current.release.version });
  const element = fragment(`
    <div class="update-banner" role="status" data-update-progress>
      <div class="update-body">
        <strong>${esc(label)}</strong>
        <div class="update-meter" role="progressbar" aria-valuemin="0" aria-valuemax="100" aria-label="${esc(label)}"><span></span></div>
        <span class="update-progress-detail"></span>
      </div>
      <button class="icon-btn" data-update-progress="close" aria-label="${esc(t("common.close"))}" data-tooltip="${esc(t("common.close"))}">${icon("x")}</button>
    </div>`);
  progressBanner = element;
  $("#toasts").before(element);
  // Only the close button dismisses: the body is status text, not a control.
  $("button[data-update-progress='close']", element).addEventListener("click", () => {
    silent = true;
    removeProgress();
  });
  renderProgress();
}

function renderProgress(): void {
  if (!progressBanner || !current) return;
  const fraction = current.total ? Math.min(1, current.received / current.total) : 0;
  $("span", $(".update-meter", progressBanner)).style.transform = `scaleX(${fraction})`;
  $(".update-meter", progressBanner).setAttribute("aria-valuenow", String(Math.floor(fraction * 100)));
  $(".update-progress-detail", progressBanner).textContent = current.total
    ? `${megabytes(current.received)} / ${megabytes(current.total)} MB`
    : "";
}

function removeProgress(): void {
  progressBanner?.remove();
  progressBanner = null;
}

/** Show the app; an installed update then waits for an idle moment instead. */
async function closeScreen(): Promise<void> {
  const reveal = revealAfterScreen;
  screen?.remove();
  screen = null;
  revealAfterScreen = null;
  await reveal?.();
  if (current?.phase === "ready") watchForRestart();
}

async function restart(): Promise<boolean> {
  try {
    await api.restartToUpdate();
    return true;
  } catch {
    // A login started meanwhile: ask again at the next idle moment.
    if (screen) await closeScreen();
    else watchForRestart();
    return false;
  }
}

// ---------------------------------------------------------------- restart notice

function watchForRestart(): void {
  if (declined || watcher !== undefined) return;
  const tick = async () => {
    if (notice || busy()) return;
    const idle = await api.updateIdle().catch(() => false);
    if (idle && !notice && !busy() && !declined) showRestartNotice();
  };
  watcher = window.setInterval(() => void tick(), IDLE_POLL_MS);
  void tick();
}

function stopWatching(): void {
  clearInterval(watcher);
  watcher = undefined;
}

function showRestartNotice(): void {
  const version = current?.release?.version ?? "";
  let seconds = RESTART_SECONDS;
  document.querySelector(".update-banner")?.remove();
  const element = fragment(`
    <div class="update-banner" role="status" data-update-notice>
      <div class="update-body">
        <strong>${esc(t("update.ready"))}</strong>
        <span aria-live="off"></span>
      </div>
      <button class="btn btn-primary" data-update="restart">${icon("refresh-cw", 15)}${esc(t("update.restartNow"))}</button>
      <button class="btn btn-ghost" data-update="later">${esc(t("update.later"))}</button>
    </div>`);
  notice = element;
  const countdown = $(".update-body span", element);
  const render = () => (countdown.textContent = t("update.restarting", { seconds }));
  const close = () => {
    clearInterval(timer);
    if (notice === element) notice = null;
    void leave(element, "toast-out");
  };
  const restartNow = async () => {
    clearInterval(timer);
    element.querySelectorAll("button").forEach((button) => (button.disabled = true));
    if (!(await restart())) close();
  };
  // A match may have started during the countdown; Restart now skips this.
  const restartIfIdle = async () => {
    clearInterval(timer);
    if (await api.updateIdle().catch(() => false)) await restartNow();
    else close();
  };
  const timer = setInterval(() => {
    // Something started: ask again once the app is idle.
    if (busy()) return close();
    seconds -= 1;
    if (seconds > 0) render();
    else void restartIfIdle();
  }, 1000);
  render();
  $("#toasts").before(element);
  element.addEventListener("click", (event) => {
    const action = (event.target as HTMLElement).closest<HTMLElement>("[data-update]")?.dataset.update;
    if (action === "restart") void restartNow();
    if (action === "later") {
      declined = true;
      stopWatching();
      close();
      toast("info", t("update.nextLaunch", { version }), "", 6000);
    }
  });
}

// ---------------------------------------------------------------- download banner

/** "Update available" with a link to the release page, for a manual download. */
export function showUpdateBanner(release: Release, force = false): void {
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
      <button class="icon-btn" data-update="later" aria-label="${esc(t("update.later"))}" data-tooltip="${esc(t("update.later"))}">${icon("x")}</button>
    </div>`);
  $("#toasts").before(banner);
  banner.addEventListener("click", (event) => {
    const action = (event.target as HTMLElement).closest<HTMLElement>("[data-update]")?.dataset.update;
    if (action === "open") {
      api.openRelease(release.url).catch((error) => toast("error", t("toast.openFailed"), describe(error), 5000));
    }
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

// ---------------------------------------------------------------- Settings › Check now

export async function checkUpdateNow(button: HTMLButtonElement, output: HTMLElement): Promise<void> {
  button.disabled = true;
  output.dataset.live = "";
  output.textContent = "…";
  try {
    apply(await api.checkUpdate());
    const { phase, release } = current!;
    if ((phase === "available" || phase === "failed") && release) showUpdateBanner(release, true);
    renderResult();
  } catch (error) {
    delete output.dataset.live;
    output.textContent = "";
    toast("error", t("toast.updateFailed"), describe(error), 5000);
  } finally {
    button.disabled = false;
  }
}

/** Keep the Settings line current while a check or install it started runs. */
function renderResult(): void {
  const output = document.querySelector<HTMLElement>(".update-result[data-live]");
  if (!output || !current) return;
  const version = current.release?.version ?? "";
  const text: Record<UpdateStatus["phase"], string> = {
    idle: "",
    checking: "…",
    upToDate: t(current.release ? "update.latest" : "update.none"),
    available: t("update.text", { version }),
    failed: t("update.text", { version }),
    downloading: t("update.downloading", { version }),
    ready: t("update.installed", { version }),
    restarting: t("update.applying"),
  };
  output.textContent = text[current.phase];
}
