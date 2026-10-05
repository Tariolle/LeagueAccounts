// Full-screen sign-in progress: shows each step reported by the backend
// ("login-step" events) as it happens.

import { accountLabel, type AccountView, type LoginStep } from "./api";
import { $, esc, fragment, leave } from "./dom";
import { t, type MessageKey } from "./i18n";
import { icon } from "./icons";
import { emblem } from "./ranks";

type StepState = "pending" | "active" | "done" | "skipped" | "error";

const LABELS: Record<LoginStep, MessageKey> = {
  closeLeague: "login.step.closeLeague",
  openClient: "login.step.openClient",
  waitAuth: "login.step.waitAuth",
  signOut: "login.step.signOut",
  findWindow: "login.step.findWindow",
  focus: "login.step.focus",
  type: "login.step.type",
  confirm: "login.step.confirm",
  launchGame: "login.step.launchGame",
  done: "login.step.done",
};

export interface LoginOverlay {
  step(step: LoginStep): void;
  succeed(title: string, detail: string): void;
  finishInClient(): void;
  fail(message: string): void;
  close(): void;
}

export function openLoginOverlay(options: {
  account: AccountView;
  tier: string;
  viaRiot: boolean;
  closeLeague: boolean;
  switchAccount: boolean;
  launchGame: boolean;
  onCancel: () => void;
}): LoginOverlay {
  const steps: LoginStep[] = options.viaRiot
    ? [
        ...(options.closeLeague ? (["closeLeague"] as LoginStep[]) : []),
        "openClient",
        "waitAuth",
        ...(options.switchAccount ? (["signOut"] as LoginStep[]) : []),
        "findWindow",
        "focus",
        "type",
        "confirm",
        ...(options.launchGame ? (["launchGame"] as LoginStep[]) : []),
      ]
    : ["focus", "type"];
  const states = new Map<LoginStep, StepState>(steps.map((step) => [step, "pending"]));
  const label = (step: LoginStep) =>
    step === "focus" && !options.viaRiot ? t("login.step.focusPrevious") : t(LABELS[step]);

  const overlay = fragment(`
    <div class="login-overlay" role="dialog" aria-modal="true" aria-live="polite" tabindex="-1">
      <div class="login-stage">
        <div class="login-orbit">
          <span class="ring ring-1"></span><span class="ring ring-2"></span><span class="ring ring-3"></span>
          <div class="login-emblem">${emblem(options.tier, 104)}</div>
        </div>
        <h2 class="login-title">${esc(t("login.title"))}</h2>
        <p class="login-account">${esc(accountLabel(options.account))}</p>
        <div class="login-progress"><span></span></div>
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
    </div>`);
  $("#modal-root").append(overlay);

  const render = () => {
    let done = 0;
    steps.forEach((step) => {
      const state = states.get(step) ?? "pending";
      const item = $(`[data-step="${step}"]`, overlay);
      if (item.className === state) return;
      item.className = state;
      const icons: Record<StepState, string> = {
        pending: "",
        active: "",
        done: icon("check", 13),
        skipped: icon("minus", 13),
        error: icon("x", 13),
      };
      $(".step-icon", item).innerHTML = icons[state];
    });
    steps.forEach((step) => {
      if (states.get(step) === "done" || states.get(step) === "skipped") done++;
    });
    $(".login-progress span", overlay).style.width = `${(done / steps.length) * 100}%`;
  };

  // The result stays on screen until the user closes it.
  const finish = (kind: "success" | "info" | "error", message: string) => {
    overlay.classList.add(`is-${kind}`);
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
      if (step === "done") {
        steps.forEach((candidate) => {
          const state = states.get(candidate);
          if (state === "active") states.set(candidate, "done");
          if (state === "pending") states.set(candidate, "skipped");
        });
        render();
        return;
      }
      if (!states.has(step)) return;
      const index = steps.indexOf(step);
      steps.forEach((candidate, position) => {
        const state = states.get(candidate);
        if (position < index) {
          if (state === "active") states.set(candidate, "done");
          if (state === "pending") states.set(candidate, "skipped");
        }
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
      steps.forEach((step) => {
        if (states.get(step) === "active" && step !== "confirm") states.set(step, "done");
      });
      render();
      $(".login-title", overlay).textContent = t("login.finishTitle");
      finish("info", t("toast.finishSignIn"));
    },
    fail(message) {
      steps.forEach((step) => {
        if (states.get(step) === "active") states.set(step, "error");
      });
      render();
      $(".login-title", overlay).textContent = t("login.failed");
      finish("error", message);
    },
    close() {
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
