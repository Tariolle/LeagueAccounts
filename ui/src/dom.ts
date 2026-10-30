import { t } from "./i18n";
import { hydrateIcons, icon } from "./icons";

const ESCAPES: Record<string, string> = {
  "&": "&amp;",
  "<": "&lt;",
  ">": "&gt;",
  '"': "&quot;",
  "'": "&#39;",
};

/** Escape untrusted text (account names, descriptions) for HTML templates. */
export const esc = (value: string): string => value.replace(/[&<>"']/g, (char) => ESCAPES[char]);

export const $ = <T extends HTMLElement = HTMLElement>(selector: string, root: ParentNode = document): T => {
  const element = root.querySelector<T>(selector);
  if (!element) throw new Error(`Missing element: ${selector}`);
  return element;
};

export function fragment(html: string): HTMLElement {
  const template = document.createElement("template");
  template.innerHTML = html.trim();
  const element = template.content.firstElementChild as HTMLElement;
  hydrateIcons(element);
  return element;
}

export const reducedMotion = (): boolean => window.matchMedia("(prefers-reduced-motion: reduce)").matches;

/** Remove an element after its exit animation (or immediately without motion). */
export function leave(element: HTMLElement, className = "leaving"): Promise<void> {
  return new Promise((resolve) => {
    if (reducedMotion()) {
      element.remove();
      resolve();
      return;
    }
    element.classList.add(className);
    const done = () => {
      element.remove();
      resolve();
    };
    element.addEventListener("animationend", done, { once: true });
    setTimeout(done, 450);
  });
}

// ---------------------------------------------------------------- toasts

type ToastKind = "success" | "error" | "info";

const TOAST_ICONS: Record<ToastKind, string> = { success: "check", error: "alert", info: "info" };

export function toast(kind: ToastKind, title: string, message = "", duration = 3200): void {
  const root = $("#toasts");
  const element = fragment(`
    <div class="toast toast-${kind}" role="status" style="--life:${duration}ms">
      <span class="toast-icon">${icon(TOAST_ICONS[kind], 16)}</span>
      <div class="toast-body">
        <strong>${esc(title)}</strong>
        ${message ? `<span>${esc(message)}</span>` : ""}
      </div>
      <button class="toast-close" aria-label="${esc(t("common.close"))}">${icon("x", 14)}</button>
      <span class="toast-life"></span>
    </div>`);
  root.append(element);
  while (root.children.length > 4) root.firstElementChild?.remove();
  const dismiss = () => void leave(element, "toast-out");
  element.querySelector("button")?.addEventListener("click", dismiss);
  setTimeout(dismiss, duration);
}

// ---------------------------------------------------------------- modals

export interface ModalAction {
  label: string;
  kind?: "primary" | "danger" | "ghost";
  value: string;
}

/**
 * Show a modal; resolves with the clicked action value, or null on dismiss.
 * `onSubmit` can validate a form and keep the modal open by returning false.
 */
export function modal(options: {
  title: string;
  body: string;
  actions: ModalAction[];
  wide?: boolean;
  /// Stack the footer actions vertically; for dialogs whose labels are long
  /// enough that a single row would overflow the modal width.
  stacked?: boolean;
  onOpen?: (root: HTMLElement) => void;
  onSubmit?: (value: string, root: HTMLElement) => boolean | Promise<boolean>;
}): Promise<string | null> {
  return new Promise((resolve) => {
    const overlay = fragment(`
      <div class="overlay">
        <div class="modal${options.wide ? " modal-wide" : ""}" role="dialog" aria-modal="true" aria-label="${esc(options.title)}">
          <header class="modal-head">
            <h2>${esc(options.title)}</h2>
          </header>
          <div class="modal-close-mount">
            <button class="modal-close" data-value="__close" aria-label="${esc(t("common.close"))}"><span></span></button>
          </div>
          <form class="modal-body" novalidate>${options.body}</form>
          <footer class="modal-foot${options.stacked ? " is-stacked" : ""}">
            ${options.actions
              .map(
                (action) =>
                  `<button class="btn btn-${action.kind ?? "ghost"}" data-value="${esc(action.value)}">${esc(action.label)}</button>`,
              )
              .join("")}
          </footer>
        </div>
      </div>`);
    const previousFocus = document.activeElement as HTMLElement | null;
    $("#modal-root").append(overlay);
    options.onOpen?.(overlay);
    const first = overlay.querySelector<HTMLElement>("input, textarea, select, .btn-primary, .btn-danger");
    first?.focus();

    let closed = false;
    const close = (value: string | null) => {
      if (closed) return;
      closed = true;
      document.removeEventListener("keydown", onKey, true);
      void leave(overlay, "overlay-out");
      previousFocus?.focus?.();
      resolve(value);
    };
    const submit = async (value: string) => {
      if (options.onSubmit && !(await options.onSubmit(value, overlay))) return;
      close(value);
    };
    const onKey = (event: KeyboardEvent) => {
      // A child dialog owns the keyboard until it closes. Ignore its exit
      // animation too, so the editor becomes active as soon as it is dismissed.
      const top = [...document.querySelectorAll("#modal-root > .overlay:not(.overlay-out)")].at(-1);
      if (closed || top !== overlay) return;
      if (event.key === "Tab") {
        const controls = [...overlay.querySelectorAll<HTMLElement>("button:not(:disabled), input:not(:disabled), select:not(:disabled), textarea:not(:disabled), [tabindex='0']")]
          .filter(control => control.getClientRects().length > 0);
        const first = controls[0];
        const last = controls.at(-1);
        if (event.shiftKey && document.activeElement === first) { event.preventDefault(); last?.focus(); }
        else if (!event.shiftKey && document.activeElement === last) { event.preventDefault(); first?.focus(); }
      }
      if (event.key === "Escape") {
        event.preventDefault();
        event.stopImmediatePropagation();
        close(null);
      }
    };
    document.addEventListener("keydown", onKey, true);
    overlay.addEventListener("mousedown", (event) => {
      if (event.target === overlay) close(null);
    });
    overlay.querySelectorAll<HTMLButtonElement>("[data-value]").forEach((button) => {
      button.addEventListener("click", (event) => {
        event.preventDefault();
        const value = button.dataset.value ?? "";
        if (value === "__close") close(null);
        else void submit(value);
      });
    });
    overlay.querySelector("form")?.addEventListener("submit", (event) => {
      event.preventDefault();
      const primary = options.actions.find((action) => action.kind === "primary" || action.kind === "danger");
      if (primary) void submit(primary.value);
    });
  });
}

// ---------------------------------------------------------------- context menu

export interface MenuItem {
  label: string;
  icon: string;
  danger?: boolean;
  run: () => void;
}

// The client's context menus have no separators or shortcut chips.
export function contextMenu(x: number, y: number, items: MenuItem[]): void {
  closeMenu();
  const menu = fragment(`
    <div class="menu" role="menu">
      ${items
        .map(
          (item, index) => `<button class="menu-item${item.danger ? " danger" : ""}" role="menuitem" data-index="${index}">
                 ${icon(item.icon, 15)}<span>${esc(item.label)}</span>
               </button>`,
        )
        .join("")}
    </div>`);
  $("#menu-root").append(menu);
  const { innerWidth, innerHeight } = window;
  const rect = menu.getBoundingClientRect();
  menu.style.left = `${Math.min(x, innerWidth - rect.width - 8)}px`;
  menu.style.top = `${Math.min(y, innerHeight - rect.height - 8)}px`;
  menu.addEventListener("click", (event) => {
    const button = (event.target as HTMLElement).closest<HTMLElement>("[data-index]");
    if (!button) return;
    const item = items[Number(button.dataset.index)];
    closeMenu();
    item.run();
  });
  setTimeout(() => {
    document.addEventListener("mousedown", outside, true);
    document.addEventListener("keydown", escape, true);
    window.addEventListener("blur", closeMenu);
  });
}

function outside(event: MouseEvent) {
  if (!(event.target as HTMLElement).closest(".menu")) closeMenu();
}

function escape(event: KeyboardEvent) {
  if (event.key === "Escape") {
    event.stopPropagation();
    closeMenu();
  }
}

export function closeMenu(): void {
  document.removeEventListener("mousedown", outside, true);
  document.removeEventListener("keydown", escape, true);
  window.removeEventListener("blur", closeMenu);
  document.querySelectorAll<HTMLElement>("#menu-root .menu").forEach((menu) => void leave(menu, "menu-out"));
}
