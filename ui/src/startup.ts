/** Decode the first screen, including CSS backgrounds and icon masks, offscreen. */
export async function revealApp(): Promise<void> {
  // Recent-match portraits are lazy-loaded and can number ten per account.
  // Do not keep the whole window on the loader while offscreen portraits load.
  const images = [...document.images]
    .filter(image => image.loading !== "lazy")
    .map(image => image.decode().catch(() => {}));
  const urls = new Set<string>();
  for (const element of document.body.querySelectorAll("*")) {
    if (element.closest("#startup-loader")) continue;
    for (const pseudo of [null, "::before", "::after"]) {
      const style = getComputedStyle(element, pseudo);
      for (const value of [style.backgroundImage, style.maskImage]) {
        for (const match of value.matchAll(/url\(["']?(.*?)["']?\)/g)) urls.add(match[1]);
      }
    }
  }
  for (const src of urls) {
    const image = new Image();
    image.src = src;
    images.push(image.decode().catch(() => {}));
  }
  // Reading layout starts font requests even though the interface is hidden.
  void document.body.offsetHeight;
  await Promise.all([document.fonts.ready, ...images]);
  await new Promise<void>(resolve => requestAnimationFrame(() => resolve()));
  document.querySelectorAll(".card.enter, .card.enter-stagger").forEach(card => card.classList.remove("enter", "enter-stagger"));
  document.body.classList.add("ready");
  document.body.setAttribute("aria-busy", "false");
  document.querySelector("#startup-loader")?.remove();
}

export function failStartup(error: unknown): void {
  console.error("League Accounts startup failed", error);
  document.body.setAttribute("aria-busy", "false");
  const loader = document.querySelector<HTMLElement>("#startup-loader");
  if (loader) {
    loader.dataset.failed = "true";
    loader.removeAttribute("aria-label");
  }
  const message = document.querySelector<HTMLElement>("#startup-error");
  if (message) message.hidden = false;
}
