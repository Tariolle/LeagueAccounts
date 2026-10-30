// Full-screen sign-in progress: shows each step reported by the backend
// ("login-step" events) as it happens.

import { accountLabel, type AccountView, type LoginStep } from "./api";
import { $, esc, fragment, leave } from "./dom";
import { t, type MessageKey } from "./i18n";
import { icon } from "./icons";
import { hextechLoader } from "./loader";
import { emblem } from "./ranks";

type StepState = "pending" | "active" | "done" | "error";

const LABELS: Record<LoginStep, MessageKey> = {
  closeLeague: "login.step.closeLeague",
  openClient: "login.step.openClient",
  signOut: "login.step.signOut",
  signIn: "login.step.signIn",
  verification: "login.step.verification",
  focus: "login.step.focus",
  type: "login.step.type",
  launchGame: "login.step.launchGame",
  updating: "login.step.updating",
  done: "login.step.done",
};

export interface LoginOverlay {
  step(step: LoginStep): void;
  succeed(title: string, detail: string): void;
  finishInClient(): void;
  fail(message: string, title?: string): void;
  close(): void;
}

export function openLoginOverlay(options: {
  account: AccountView;
  tier: string;
  viaRiot: boolean;
  closeLeague: boolean;
  launchGame: boolean;
  onCancel: () => void;
}): LoginOverlay {
  // Starting the client and waiting for its session are one action, and so
  // are finding the form, typing and confirming: four separate rows only
  // repeated what the title already said.
  const steps: LoginStep[] = options.viaRiot
    ? [
        ...(options.closeLeague ? (["closeLeague"] as LoginStep[]) : []),
        "openClient",
        "signOut",
        "signIn",
        ...(options.launchGame ? (["launchGame"] as LoginStep[]) : []),
      ]
    : ["focus", "type"];
  const states = new Map<LoginStep, StepState>(steps.map((step) => [step, "pending"]));
  // A patch replaces the launch label; it is not a separate row.
  let updating = false;
  const label = (step: LoginStep) => {
    if (step === "launchGame" && updating) return t("login.step.updating");
    return step === "focus" && !options.viaRiot ? t("login.step.focusPrevious") : t(LABELS[step]);
  };

  const overlay = fragment(`
    <div class="login-overlay" role="dialog" aria-modal="true" aria-live="polite" tabindex="-1">
      <div class="login-stage">
        <div class="login-identity">
        <div class="login-orbit">
          <div class="login-loader" aria-hidden="true"></div>
          <div class="login-emblem" hidden>${emblem(options.tier, 240, document.documentElement.dataset.mode === "tft" ? "tft" : "lol")}</div>
        </div>
        <h2 class="login-title">${esc(t("login.title"))}</h2>
        <p class="login-account">${esc(accountLabel(options.account))}</p>
        </div>
        <div class="login-details">
        <div class="login-progress" role="progressbar" aria-label="${esc(t("login.title"))}" aria-valuemin="0" aria-valuemax="${steps.length}" aria-valuenow="0"><span></span></div>
        <ol class="login-steps">
          ${steps
            .map(
              (step, index) => `<li data-step="${step}" class="pending" style="--i:${index}">
                <span class="step-icon"></span><span class="step-label">${esc(label(step))}</span>
              </li>`,
            )
            .join("")}
        </ol>
        <p class="login-message" hidden></p>
        <div class="login-actions">
          <button class="btn btn-ghost" data-login="cancel">${esc(t("common.cancel"))}</button>
          <button class="btn btn-primary" data-login="close" hidden>${esc(t("common.close"))}</button>
        </div>
        </div>
      </div>
    </div>`);
  $("#modal-root").append(overlay);
  const progress = $(".login-progress", overlay);
  // The client's loading animation plays while signing in; the crest replaces it with the result.
  let stopLoader: (() => void) | null = hextechLoader($(".login-loader", overlay));
  const showCrest = () => {
    if (!stopLoader) return;
    stopLoader();
    stopLoader = null;
    $(".login-loader", overlay).remove();
    $(".login-emblem", overlay).hidden = false;
  };

  const render = () => {
    let done = 0;
    steps.forEach((step) => {
      const state = states.get(step) ?? "pending";
      const item = $(`[data-step="${step}"]`, overlay);
      // The launch row is renamed while the game is patching.
      const text = label(step);
      const caption = $(".step-label", item);
      if (caption.textContent !== text) caption.textContent = text;
      if (item.className === state) return;
      item.className = state;
      const icons: Record<StepState, string> = {
        pending: "",
        active: "",
        done: icon("check", 13),
        error: icon("x", 13),
      };
      $(".step-icon", item).innerHTML = icons[state];
    });
    steps.forEach((step) => {
      if (states.get(step) === "done") done++;
    });
    progress.style.setProperty("--login-progress", String(done / steps.length));
    progress.setAttribute("aria-valuenow", String(done));
    const current = steps.find(step => states.get(step) === "active");
    progress.setAttribute("aria-valuetext", current ? label(current) : t(done === steps.length ? "login.step.done" : "login.title"));
  };

  // The result stays on screen until the user closes it.
  const finish = (kind: "success" | "info" | "error", message: string) => {
    showCrest();
    overlay.classList.add(`is-${kind}`);
    progress.setAttribute("aria-valuetext", $(".login-title", overlay).textContent ?? "");
    const text = $(".login-message", overlay);
    text.textContent = message;
    text.hidden = !message;
    $('[data-login="cancel"]', overlay).hidden = true;
    const close = $<HTMLButtonElement>('[data-login="close"]', overlay);
    close.hidden = false;
    close.focus();
  };

  const api: LoginOverlay = {
    step(step) {
      const message = $(".login-message", overlay);
      message.hidden = step !== "verification";
      if (step === "verification") {
        message.textContent = t("login.step.verification");
        progress.setAttribute("aria-valuetext", message.textContent);
        return;
      }
      if (step === "updating") {
        // A patch belongs to the launch row: keep it active and say what is
        // happening instead of adding a row that is usually skipped.
        if (!states.has("launchGame")) return;
        updating = true;
        const index = steps.indexOf("launchGame");
        steps.forEach((candidate, position) => {
          const state = states.get(candidate);
          if (position < index && state !== "error") states.set(candidate, "done");
        });
        states.set("launchGame", "active");
        render();
        return;
      }
      if (step === "launchGame") updating = false;
      if (step === "done") {
        updating = false;
        steps.forEach((candidate) => {
          if (states.get(candidate) !== "error") states.set(candidate, "done");
        });
        render();
        return;
      }
      if (!states.has(step)) return;
      const index = steps.indexOf(step);
      steps.forEach((candidate, position) => {
        const state = states.get(candidate);
        // Steps passed over (such as signing out when nobody was signed in) count as done.
        if (position < index && state !== "error") states.set(candidate, "done");
        if (position > index) states.set(candidate, "pending");
      });
      states.set(step, "active");
      render();
    },
    succeed(title, detail) {
      api.step("done");
      $(".login-title", overlay).textContent = title;
      finish("success", detail);
    },
    finishInClient() {
      updating = false;
      steps.forEach((step) => {
        if (states.get(step) === "active" && step !== "signIn") states.set(step, "done");
      });
      render();
      $(".login-title", overlay).textContent = t("login.finishTitle");
      finish("info", t("toast.finishSignIn"));
    },
    fail(message, title = t("login.failed")) {
      updating = false;
      steps.forEach((step) => {
        if (states.get(step) === "active") states.set(step, "error");
      });
      render();
      $(".login-title", overlay).textContent = title;
      finish("error", message);
    },
    close() {
      showCrest();
      document.removeEventListener("keydown", onKey, true);
      if (overlay.isConnected) void leave(overlay, "login-out");
    },
  };

  const onKey = (event: KeyboardEvent) => {
    if (event.key === "Tab") {
      event.preventDefault();
      event.stopPropagation();
      const button = overlay.querySelector<HTMLButtonElement>("button:not([hidden]):not(:disabled)");
      (button ?? overlay).focus();
      return;
    }
    if (event.key !== "Escape") return;
    event.stopPropagation();
    if ($('[data-login="close"]', overlay).hidden) options.onCancel();
    else api.close();
  };
  document.addEventListener("keydown", onKey, true);
  $('[data-login="cancel"]', overlay).addEventListener("click", () => {
    $('[data-login="cancel"]', overlay).setAttribute("disabled", "");
    options.onCancel();
  });
  $('[data-login="close"]', overlay).addEventListener("click", () => api.close());
  $('[data-login="cancel"]', overlay).focus();
  render();
  return api;
}
