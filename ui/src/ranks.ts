import type { AccountView } from "./api";
import { t, tierName } from "./i18n";

export type Mode = "lol" | "tft";

export const TIER_ORDER = [
  "Challenger",
  "Grandmaster",
  "Master",
  "Diamond",
  "Emerald",
  "Platinum",
  "Gold",
  "Silver",
  "Bronze",
  "Iron",
  "Unranked",
  "Error",
] as const;

export const APEX = new Set(["Master", "Grandmaster", "Challenger"]);

interface Palette {
  base: string;
  light: string;
  dark: string;
}

/** Tier colours: base for text/glow, light/dark for the emblem gradient. */
export const TIERS: Record<string, Palette> = {
  Iron: { base: "#9a8f8a", light: "#c9bdb7", dark: "#4a403c" },
  Bronze: { base: "#c48357", light: "#efb489", dark: "#6a3f26" },
  Silver: { base: "#b4c3d0", light: "#eef4f9", dark: "#5e6b78" },
  Gold: { base: "#e8bd52", light: "#ffe7a3", dark: "#8a6316" },
  Platinum: { base: "#45cbb9", light: "#a6f3e7", dark: "#17675e" },
  Emerald: { base: "#34d27a", light: "#a6f7c6", dark: "#13643a" },
  Diamond: { base: "#7d98ff", light: "#cdd8ff", dark: "#2c3d9e" },
  Master: { base: "#c27cf5", light: "#ebcbff", dark: "#5d2a8a" },
  Grandmaster: { base: "#f25d68", light: "#ffbcc1", dark: "#7d1720" },
  Challenger: { base: "#f7d478", light: "#fff4cc", dark: "#2a6f9a" },
  Unranked: { base: "#6b7487", light: "#9aa3b5", dark: "#2b313d" },
  Error: { base: "#e5484d", light: "#ff9a9d", dark: "#5a1a1c" },
};

export interface RankLike {
  tier: string;
  division: string;
  lp: string;
}

export function normalizeTier(tier: string): string {
  const match = TIER_ORDER.find((name) => name.toLowerCase() === tier.trim().toLowerCase());
  return match ?? "Unranked";
}

export const palette = (tier: string): Palette => TIERS[normalizeTier(tier)];

export function rankOf(account: AccountView, mode: Mode, queue: "solo" | "flex" = "solo"): RankLike {
  return mode === "tft" ? account.tft : queue === "solo" ? account.lol
    : account.flex ?? { tier: "Error", division: "", lp: "" };
}

export function isRanked(rank: RankLike): boolean {
  const tier = normalizeTier(rank.tier);
  return tier !== "Unranked" && tier !== "Error";
}

export function rankLabel(rank: RankLike): string {
  const tier = normalizeTier(rank.tier);
  if (tier === "Unranked") return t("rank.unranked");
  if (tier === "Error") return t("rank.error");
  return APEX.has(tier) || !rank.division ? tierName(tier) : `${tierName(tier)} ${rank.division}`;
}

const DIVISIONS: Record<string, number> = { I: 0, "1": 0, II: 1, "2": 1, III: 2, "3": 2, IV: 3, "4": 3 };

/** Highest rank first, then division I→IV, then LP descending. */
export function rankScore(rank: RankLike): [number, number, number] {
  const tierIndex = TIER_ORDER.indexOf(normalizeTier(rank.tier) as (typeof TIER_ORDER)[number]);
  const division = DIVISIONS[rank.division.trim().toUpperCase()] ?? 9;
  const lp = Number.parseInt(rank.lp, 10) || 0;
  return [tierIndex, division, -lp];
}

export function compareRanks(a: RankLike, b: RankLike): number {
  const [x, y] = [rankScore(a), rankScore(b)];
  return x[0] - y[0] || x[1] - y[1] || x[2] - y[2];
}

/** LP progress in [0, 1]: within the division, or towards 1000 LP for apex tiers. */
export function lpProgress(rank: RankLike): number {
  const lp = Number.parseInt(rank.lp, 10);
  if (!Number.isFinite(lp) || !isRanked(rank)) return 0;
  const tier = normalizeTier(rank.tier);
  return Math.max(0.03, Math.min(1, APEX.has(tier) ? lp / 1000 : lp / 100));
}

function divisionNumber(division: string): number | null {
  const index = DIVISIONS[division.trim().toUpperCase()];
  return index === undefined ? null : index + 1;
}

/** League ranked duo rules ("Friend Elo"), ported from the original app. */
export function canPlayWith(rank: RankLike, friendTier: string, friendDivision: string): boolean {
  const tier = normalizeTier(rank.tier).toLowerCase();
  const friend = friendTier.toLowerCase();
  const own = divisionNumber(rank.division);
  const theirs = divisionNumber(friendDivision);
  switch (friend) {
    case "master":
      return tier === "master" || (tier === "diamond" && own === 1);
    case "diamond":
      if (tier === "diamond" && own !== null && theirs !== null && Math.abs(own - theirs) <= 2) return true;
      return (tier === "emerald" && own === 1 && theirs === 4) || (tier === "master" && theirs === 1);
    case "emerald":
      return tier === "emerald" || tier === "platinum" || (theirs === 1 && tier === "diamond" && own === 4);
    case "platinum":
      return ["platinum", "emerald", "gold"].includes(tier);
    case "iron":
      return ["iron", "bronze", "silver"].includes(tier);
    case "bronze":
    case "silver":
    case "gold": {
      const index = TIER_ORDER.findIndex((name) => name.toLowerCase() === friend);
      const allowed = [TIER_ORDER[index - 1], TIER_ORDER[index]];
      if (index < TIER_ORDER.length - 3) allowed.push(TIER_ORDER[index + 1]);
      return allowed.some((name) => name?.toLowerCase() === tier);
    }
    default:
      return false;
  }
}

/** Riot's ranked art: small crests stay crisp in filters; cards use the full emblem in both
    games, as the client does for RANKED_TFT. */
export function emblem(tierName: string, size = 56, mode: Mode = "lol"): string {
  const tier = normalizeTier(tierName);
  const asset = (tier === "Error" ? "Unranked" : tier).toLowerCase();
  const mini = size <= 32;
  const src = mini ? `/league/crests/${asset}${mode === "tft" ? "-tft" : ""}.svg` : `/league/ranks/${asset}.png`;
  return `<span class="emblem ${mini ? "emblem-mini" : "emblem-full"}${!mini && asset === "unranked" ? " emblem-unranked" : ""}" style="--emblem-size:${size}px" aria-hidden="true"><img src="${src}" alt="" draggable="false" /></span>`;
}
