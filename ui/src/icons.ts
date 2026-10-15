import {
  ArrowDownUp,
  ChevronDown,
  EyeOff,
  FolderOpen,
  Keyboard,
  LayoutGrid,
  List,
  type IconNode,
} from "lucide";

const ICONS: Record<string, IconNode> = {
  "arrow-down-up": ArrowDownUp,
  "chevron-down": ChevronDown,
  "eye-off": EyeOff,
  "folder-open": FolderOpen,
  keyboard: Keyboard,
  "layout-grid": LayoutGrid,
  list: List,
};

const attrs = (values: Record<string, string | number | undefined>): string =>
  Object.entries(values)
    .filter(([, value]) => value !== undefined)
    .map(([name, value]) => `${name}="${value}"`)
    .join(" ");

// Client masks retain their original silhouettes and take the control's state color.
const CLIENT_ICONS: Record<string, string> = {
  clock: "clock.svg", settings: "icon_settings.png", check: "check.svg",
  alert: "warning.svg", copy: "icon_copy.png", download: "icon_export.png",
  upload: "icon_import.png", "external-link": "external-link-mask.png",
  eye: "preview.svg", info: "info-icon.svg", key: "lock.svg",
  login: "icon_next.png", minus: "icon_remove.png", more: "ellipses.svg",
  pencil: "icon_edit.png", plus: "icon_plus.png", "refresh-cw": "refresh.png",
  search: "search.png", trash: "icon_delete.png", users: "profile.svg",
  x: "control-close.png",
};

/** League's own icon artwork, with OS/utility fallbacks where no client equivalent exists. */
export function icon(name: string, size = 16): string {
  const asset = CLIENT_ICONS[name];
  if (asset) return `<span class="icon native-icon" style="width:var(--icon-size,${size}px);height:var(--icon-size,${size}px);--icon:url('/league/icons/${asset}')" aria-hidden="true"></span>`;
  const node = ICONS[name];
  if (!node) return "";
  const children = node
    .map(([tag, childAttrs]) => `<${tag} ${attrs(childAttrs as Record<string, string>)}/>`)
    .join("");
  return `<svg class="icon" xmlns="http://www.w3.org/2000/svg" width="${size}" height="${size}" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">${children}</svg>`;
}

/** Replace every `<span data-icon="name">` placeholder inside `root`. */
export function hydrateIcons(root: ParentNode = document): void {
  root.querySelectorAll<HTMLElement>("[data-icon]").forEach((element) => {
    if (element.dataset.hydrated) return;
    element.insertAdjacentHTML("afterbegin", icon(element.dataset.icon ?? ""));
    element.dataset.hydrated = "1";
  });
}
