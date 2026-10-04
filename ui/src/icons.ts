import {
  ArrowDownUp,
  Clock,
  Settings,
  Check,
  ChevronDown,
  CircleAlert,
  Copy,
  Download,
  ExternalLink,
  Eye,
  EyeOff,
  FolderOpen,
  Info,
  KeyRound,
  Keyboard,
  Languages,
  LayoutGrid,
  List,
  LogIn,
  Minus,
  MoreHorizontal,
  Pencil,
  Plus,
  RefreshCw,
  Search,
  Square,
  Trash2,
  Upload,
  Users,
  X,
  type IconNode,
} from "lucide";

const ICONS: Record<string, IconNode> = {
  "arrow-down-up": ArrowDownUp,
  clock: Clock,
  settings: Settings,
  check: Check,
  "chevron-down": ChevronDown,
  alert: CircleAlert,
  copy: Copy,
  download: Download,
  "external-link": ExternalLink,
  eye: Eye,
  "eye-off": EyeOff,
  "folder-open": FolderOpen,
  info: Info,
  key: KeyRound,
  keyboard: Keyboard,
  languages: Languages,
  "layout-grid": LayoutGrid,
  list: List,
  login: LogIn,
  minus: Minus,
  more: MoreHorizontal,
  pencil: Pencil,
  plus: Plus,
  "refresh-cw": RefreshCw,
  search: Search,
  square: Square,
  trash: Trash2,
  upload: Upload,
  users: Users,
  x: X,
};

const attrs = (values: Record<string, string | number | undefined>): string =>
  Object.entries(values)
    .filter(([, value]) => value !== undefined)
    .map(([name, value]) => `${name}="${value}"`)
    .join(" ");

/** Inline SVG markup for a Lucide icon. */
export function icon(name: string, size = 16): string {
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
