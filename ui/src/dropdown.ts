/* The client's lol-uikit-framed-dropdown, layered over a native <select>.
   The select stays the source of truth (value, name, change events, i18n updates),
   so forms and existing listeners keep working; this replaces presentation and input.
   Structure and states follow rcp-fe-lol-uikit framed-dropdown/component-style.styl. */

let uid = 0;
let openDropdown: { close: () => void } | null = null;

export function observeSelects(): void {
  document.querySelectorAll("select").forEach(enhance);
  new MutationObserver((records) => {
    for (const record of records) {
      for (const node of record.addedNodes) {
        if (node instanceof HTMLSelectElement) enhance(node);
        else if (node instanceof Element) node.querySelectorAll("select").forEach(enhance);
      }
    }
  }).observe(document.body, { childList: true, subtree: true });
}

function enhance(select: HTMLSelectElement): void {
  if (select.dataset.framed) return;
  select.dataset.framed = "1";
  const id = `framed-dropdown-${++uid}`;
  const frame = select.closest<HTMLElement>(".select-wrap");

  const host = document.createElement("span");
  host.className = "framed-dropdown";
  const current = document.createElement("span");
  current.className = "framed-dropdown-current";
  current.setAttribute("role", "combobox");
  current.setAttribute("aria-haspopup", "listbox");
  current.setAttribute("aria-expanded", "false");
  current.setAttribute("aria-controls", `${id}-list`);
  const list = document.createElement("div");
  list.className = "framed-dropdown-options";
  list.id = `${id}-list`;
  list.setAttribute("role", "listbox");
  list.popover = "manual";
  // As in the client: the frame clips the slide-open reveal, and only the inner list
  // scrolls, so no scrollbar flashes while the frame is still growing.
  const scroller = document.createElement("div");
  scroller.className = "framed-dropdown-scroll";
  list.append(scroller);
  host.append(current, list);
  select.after(host);
  select.classList.add("framed-native");
  select.tabIndex = -1;

  // The flyout is in the top layer (never clipped by scrolling dialogs) and anchored
  // flush under the framed box at its width, as in the client. CSSOM survives the CSP.
  const anchor = frame ?? host;
  // A frame can hold several selects (Friend elo: tier + division); keep every name.
  const names = anchor.style.getPropertyValue("anchor-name");
  anchor.style.setProperty("anchor-name", names ? `${names}, --${id}` : `--${id}`);
  list.style.setProperty("position-anchor", `--${id}`);

  let active = -1;
  let typed = "";
  let typedAt = 0;
  const options = () => [...select.options];
  const enabled = (index: number) => index >= 0 && index < select.options.length && !select.options[index].disabled;
  const isOpen = () => list.matches(":popover-open");

  function label(): string {
    const own = select.getAttribute("aria-label");
    if (own) return own;
    const field = select.labels?.[0]?.querySelector("span")?.textContent;
    return field?.trim() ?? "";
  }

  function sync(): void {
    const selected = select.selectedOptions[0];
    current.textContent = selected?.text ?? "";
    current.setAttribute("aria-label", [label(), selected?.text].filter(Boolean).join(": "));
    const disabled = select.disabled;
    host.classList.toggle("is-disabled", disabled);
    frame?.classList.toggle("is-disabled", disabled && frame.querySelectorAll("select:not(:disabled)").length === 0);
    current.tabIndex = disabled ? -1 : 0;
    current.toggleAttribute("aria-disabled", disabled);
    if (disabled && isOpen()) close();
    if (isOpen()) renderOptions();
  }

  function renderOptions(): void {
    scroller.replaceChildren(
      ...options().map((option, index) => {
        const item = document.createElement("div");
        item.className = "framed-dropdown-option";
        item.id = `${id}-option-${index}`;
        item.setAttribute("role", "option");
        item.setAttribute("aria-selected", String(index === select.selectedIndex));
        if (option.disabled) item.setAttribute("aria-disabled", "true");
        item.classList.toggle("is-active", index === active);
        item.dataset.index = String(index);
        item.textContent = option.text;
        return item;
      }),
    );
  }

  function highlight(index: number): void {
    active = index;
    list.querySelectorAll(".framed-dropdown-option").forEach((item, i) => item.classList.toggle("is-active", i === index));
    const item = scroller.children[index] as HTMLElement | undefined;
    if (item) {
      current.setAttribute("aria-activedescendant", item.id);
      // Scroll only the inner list: scrollIntoView would also move the clipping frame mid-reveal.
      const top = item.offsetTop;
      const bottom = top + item.offsetHeight;
      if (top < scroller.scrollTop) scroller.scrollTop = top;
      else if (bottom > scroller.scrollTop + scroller.clientHeight) scroller.scrollTop = bottom - scroller.clientHeight;
    } else current.removeAttribute("aria-activedescendant");
  }

  /**
   * Show the flyout, preferring below the frame and flipping above only when
   * the room below cannot hold a usable list.
   *
   * CSS anchor positioning picks a side but cannot size the list to it: a
   * popover anchored near the bottom of the window grew past the viewport, and
   * its last rows were unreachable because nothing behind it scrolls. Measuring
   * here lets the list take exactly the room it has, so it either fits or
   * scrolls to its end.
   *
   * Opening downwards is the convention every other menu follows, so a short
   * list stays below even when the space above happens to be larger; flipping
   * is reserved for the case where below is genuinely unusable.
   */
  function place(): void {
    const box = (frame ?? host).getBoundingClientRect();
    const margin = 8;
    const below = window.innerHeight - box.bottom - margin * 2;
    const above = box.top - margin * 2;
    const natural = scroller.scrollHeight;
    // A few rows are enough to read and scroll through; below this, flipping up
    // reads better than a two-row window.
    const usable = 6 * 31;
    const downwards = below >= Math.min(natural, usable) || above < usable;
    const room = Math.max(120, Math.min(downwards ? below : above, natural));
    list.style.maxHeight = `${room}px`;
    list.style.top = downwards ? `${box.bottom}px` : "auto";
    list.style.bottom = downwards ? "auto" : `${window.innerHeight - box.top}px`;
  }

  function open(): void {
    if (select.disabled || isOpen()) return;
    openDropdown?.close();
    active = select.selectedIndex;
    renderOptions();
    list.showPopover();
    place();
    highlight(active);
    current.setAttribute("aria-expanded", "true");
    frame?.classList.add("is-open");
    host.classList.add("is-open");
    openDropdown = { close };
    window.addEventListener("keydown", captureEscape, true);
    document.addEventListener("pointerdown", outside, true);
  }

  function close(): void {
    if (!isOpen()) return;
    list.hidePopover();
    // Placement was measured for this frame; the next open measures again.
    list.style.removeProperty("max-height");
    list.style.removeProperty("top");
    list.style.removeProperty("bottom");
    current.setAttribute("aria-expanded", "false");
    current.removeAttribute("aria-activedescendant");
    frame?.classList.remove("is-open");
    host.classList.remove("is-open");
    if (openDropdown?.close === close) openDropdown = null;
    window.removeEventListener("keydown", captureEscape, true);
    document.removeEventListener("pointerdown", outside, true);
  }

  function choose(index: number): void {
    if (!enabled(index)) return;
    if (index !== select.selectedIndex) {
      select.selectedIndex = index;
      select.dispatchEvent(new Event("input", { bubbles: true }));
      select.dispatchEvent(new Event("change", { bubbles: true }));
    }
    sync();
  }

  function step(from: number, delta: number): number {
    for (let i = from + delta; i >= 0 && i < select.options.length; i += delta) if (enabled(i)) return i;
    return from;
  }

  function typeahead(key: string): number {
    const now = Date.now();
    typed = now - typedAt > 700 ? key : typed + key;
    typedAt = now;
    const texts = options().map((option) => option.text.toLocaleLowerCase());
    const start = isOpen() ? active : select.selectedIndex;
    const order = [...texts.keys()].map((i) => (start + 1 + i) % texts.length);
    const query = typed.toLocaleLowerCase();
    return order.find((i) => enabled(i) && texts[i].startsWith(query)) ?? -1;
  }

  // Modal dialogs listen for Escape in the document capture phase; window capture runs first.
  function captureEscape(event: KeyboardEvent): void {
    if (event.key !== "Escape") return;
    event.preventDefault();
    event.stopPropagation();
    close();
    current.focus();
  }

  function outside(event: PointerEvent): void {
    const target = event.target as Node;
    if (!host.contains(target) && !(frame?.contains(target) ?? false)) close();
  }

  current.addEventListener("keydown", (event) => {
    if (event.ctrlKey || event.metaKey) return;
    const key = event.key;
    if (key === "Tab") {
      close();
      return;
    }
    // Keep list keys away from the app's global card shortcuts (Delete, arrows, Escape).
    event.stopPropagation();
    if (!isOpen()) {
      if (key === "Enter" || key === " " || key === "F4" || (event.altKey && (key === "ArrowDown" || key === "ArrowUp"))) {
        event.preventDefault();
        open();
      } else if (key === "ArrowDown" || key === "ArrowUp") {
        event.preventDefault();
        choose(step(select.selectedIndex, key === "ArrowDown" ? 1 : -1));
      } else if (key === "Home" || key === "End") {
        event.preventDefault();
        choose(key === "Home" ? step(-1, 1) : step(select.options.length, -1));
      } else if (key.length === 1 && key !== " ") {
        const match = typeahead(key);
        if (match >= 0) choose(match);
      }
      return;
    }
    if (key === "ArrowDown" || key === "ArrowUp") {
      event.preventDefault();
      if (event.altKey) close();
      else highlight(step(active, key === "ArrowDown" ? 1 : -1));
    } else if (key === "Home" || key === "End" || key === "PageUp" || key === "PageDown") {
      event.preventDefault();
      highlight(key === "Home" || key === "PageUp" ? step(-1, 1) : step(select.options.length, -1));
    } else if (key === "Enter" || key === " ") {
      event.preventDefault();
      choose(active);
      close();
    } else if (key.length === 1) {
      const match = typeahead(key);
      if (match >= 0) highlight(match);
    }
  });
  current.addEventListener("click", (event) => {
    // Inside a <label>, the click would also activate the hidden select, whose
    // click reaches the frame handler below and toggles the list straight back.
    event.preventDefault();
    if (isOpen()) close();
    else open();
  });
  current.addEventListener("blur", (event) => {
    if (!list.contains(event.relatedTarget as Node | null)) close();
  });
  // Clicking the frame's label ("Sort", "Friend elo") opens its first available dropdown.
  // Registered once per frame; the lookup happens at click time, after every select is enhanced.
  if (frame && !frame.dataset.framedClick) {
    frame.dataset.framedClick = "1";
    frame.addEventListener("click", (event) => {
      if ((event.target as Element).closest(".framed-dropdown")) return;
      const first = frame.querySelector<HTMLElement>(".framed-dropdown:not(.is-disabled) .framed-dropdown-current");
      // Focus first, as a <label> does for its select, so the keyboard follows the click.
      first?.focus();
      first?.click();
    });
  }
  list.addEventListener("pointerdown", (event) => event.preventDefault());
  list.addEventListener("pointermove", (event) => {
    const item = (event.target as HTMLElement).closest<HTMLElement>(".framed-dropdown-option");
    if (item && Number(item.dataset.index) !== active && enabled(Number(item.dataset.index))) highlight(Number(item.dataset.index));
  });
  list.addEventListener("click", (event) => {
    const item = (event.target as HTMLElement).closest<HTMLElement>(".framed-dropdown-option");
    if (!item || !enabled(Number(item.dataset.index))) return;
    // choose() re-renders the options, detaching the clicked one: the frame's label
    // handler would no longer see it inside the dropdown and reopen it. A surrounding
    // <label> would reopen it too, through the hidden select, unless the click is cancelled.
    event.stopPropagation();
    event.preventDefault();
    choose(Number(item.dataset.index));
    close();
    current.focus();
  });
  // Focus requests aimed at the hidden select (dialog autofocus, <label> clicks) land here.
  select.addEventListener("focus", () => current.focus());

  // Options and labels are rebuilt on language change; values are also set in code.
  new MutationObserver(sync).observe(select, { childList: true, subtree: true, characterData: true, attributes: true, attributeFilter: ["disabled", "aria-label"] });
  const proto = HTMLSelectElement.prototype;
  for (const prop of ["value", "selectedIndex"] as const) {
    const descriptor = Object.getOwnPropertyDescriptor(proto, prop);
    if (!descriptor?.get || !descriptor.set) continue;
    const { get, set } = descriptor;
    Object.defineProperty(select, prop, {
      configurable: true,
      get() { return get.call(this); },
      set(value) { set.call(this, value); sync(); },
    });
  }
  select.addEventListener("change", sync);
  sync();
}
