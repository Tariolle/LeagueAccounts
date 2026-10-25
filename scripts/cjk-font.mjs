// Subsets LXGW WenKai to the CJK characters this UI actually ships.
//
// The League fonts carry no Chinese glyphs, so the browser used to fall back to
// whatever the OS offered and Chinese text changed shape between machines.
// LXGW WenKai (SIL OFL 1.1) fixes that, but its full file is 25 MB; the UI uses
// fewer than 600 CJK characters, so a subset keeps the bundle at ~0.7 MB.
//
// Run after changing any UI string (they live in `ui/src/i18n.ts`) or any
// Rust string the UI renders: `node scripts/cjk-font.mjs`.
//
// The source font is not vendored: it is read from the Windows font directory,
// where the LXGW WenKai installer puts it. Pass a path as the first argument to
// use a copy from elsewhere.
import { readFile } from "node:fs/promises";
import { existsSync } from "node:fs";
import { resolve } from "node:path";
import { execFileSync } from "node:child_process";
import { fileURLToPath } from "node:url";

const root = resolve(fileURLToPath(new URL(".", import.meta.url)), "..");
const output = resolve(root, "ui/public/league/fonts");

/** Every character the interface can render. */
async function characters() {
  const text = [];
  // Every dictionary lives in this one file since the interface was inlined,
  // so it is also the complete list of strings the UI can render.
  text.push(await readFile(resolve(root, "ui/src/i18n.ts"), "utf8"));
  // Locale display names and other user-visible strings live in Rust.
  for (const file of ["src/locales.rs", "src/riot_client.rs", "src/vn_hosts.rs"]) {
    text.push(await readFile(resolve(root, file), "utf8"));
  }
  const chars = new Set();
  for (const chunk of text) for (const char of chunk) chars.add(char);
  // Printable ASCII, CJK punctuation and the symbols the UI composes.
  for (let code = 0x20; code < 0x7f; code++) chars.add(String.fromCharCode(code));
  for (const char of "，。、；：？！（）《》「」『』【】…—·％＃＠") chars.add(char);
  return [...chars].filter((char) => char.codePointAt(0) > 0x1f).join("");
}

const text = await characters();
// Both families are SIL OFL 1.1 and subset to the same characters. WenKai
// carries the display sizes; Noto Sans SC is the UI face, because WenKai's
// handwritten strokes read too thin at 12–13px on the dark background.
const SOURCES = [
  ["lxgw-wenkai-regular.ttf", "LXGWWenKai-Regular.ttf", "Microsoft/Windows/Fonts"],
  ["lxgw-wenkai-medium.ttf", "LXGWWenKai-Medium.ttf", "Microsoft/Windows/Fonts"],
  ["noto-sans-sc-regular.otf", "Noto Sans SC (TrueType).otf", "Fonts"],
  ["noto-sans-sc-medium.otf", "Noto Sans SC Medium (TrueType).otf", "Fonts"],
  ["noto-sans-sc-bold.otf", "Noto Sans SC Bold (TrueType).otf", "Fonts"],
];
for (const [target, name, directory] of SOURCES) {
  // Windows keeps user-installed fonts under LOCALAPPDATA and the rest under
  // the system directory; try both rather than requiring a manual copy.
  const candidates = [
    resolve(process.env.LOCALAPPDATA, directory, name),
    resolve(process.env.WINDIR ?? "C:/Windows", "Fonts", name),
  ];
  const path = candidates.find((candidate) => existsSync(candidate));
  if (!path) {
    console.warn(`${target}: ${name} not found, skipped`);
    continue;
  }
  // pyftsubset ships with fontTools; it is the only tool that writes a
  // compact subset while keeping the layout features CJK needs.
  execFileSync("pyftsubset", [
    path,
    `--output-file=${resolve(output, target)}`,
    `--text=${text}`,
    "--layout-features=*",
    "--name-IDs=*",
    "--notdef-outline",
  ], { stdio: "inherit" });
  const bytes = await readFile(resolve(output, target));
  console.log(`${target}: ${(bytes.length / 1024).toFixed(0)} KiB, ${[...text].length} characters`);
}
