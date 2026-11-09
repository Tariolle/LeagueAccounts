# Third-party assets

## League client art



The artwork, fonts and animations under `ui/public/league/` are Riot Games assets, distributed through CommunityDragon's patch **16.20** client archive. They are not covered by this repository's MIT license. Riot Games, Inc. retains all rights to these materials.

`ui/public/league/manifest.json` records the original URL, byte size and SHA-256 of every bundled file. The interface loads these files locally; it makes no runtime requests to CommunityDragon. No sound or video files are included.

The bundle contains:

- League ranked emblems and LoL/TFT miniature crests from `rcp-fe-lol-static-assets`.
- Full-size unranked art from `rcp-fe-lol-shared-components/unranked.png`, referenced by the client's `lol-regalia-emblem-element` (`--regalia-emblem-unranked`). Cards and the login view use this emblem; sidebar filters retain the native miniature crests. Its transparent canvas padding is accounted for in CSS.
- Beaufort for LoL and Spiegel fonts from `rcp-fe-lol-typekit`.
- Client backgrounds, navigation art and the empty-search Poro from `rcp-fe-lol-static-assets`, plus the TFT game selector from `rcp-fe-lol-parties`. Rank crests are static, including on hover.
- Dialog trim, the close-button mount and icon, and the title divider from `rcp-fe-lol-uikit`. The modal frame follows the live client's `lol-uikit-dialog-frame` construction, inspected in Customize My Identity.
- Checkbox sprites, dropdown arrows and checks, navigation highlights, action icons, and tooltip caret from `rcp-fe-lol-uikit`; radio sprites from `rcp-fe-lol-parties`; the loading spinner and utility icons from `rcp-fe-lol-static-assets`; window controls from `rcp-fe-lol-navigation`.

The control styling was checked against the live client's Collection, Profile, Settings, TFT and Runes surfaces: `lol-uikit-flat-button`, `lol-uikit-framed-dropdown`, `lol-uikit-flat-checkbox`, `lol-uikit-navigation-item`, `lol-uikit-scrollable`, and `lol-uikit-tooltip`. Fonts follow the client's typekit mapping. The app retains native HTML buttons, selects, checkboxes and radio inputs for keyboard access, and adapts motion to reduced-motion preferences. It does not require the League client or Pengu Loader to render.

Account frames adapt the live client's `lol-parties` `.tft-player-card`, `.inner-border-1` and `.inner-border-2` construction, inspected through the client on 2026-10-12. These are CSS frames, not image borders: three square 1px rails, a translucent `#010a13` inset and the original `#c89b3c` → `#f0e6d2` hover gradient. Pseudo-elements reproduce the nested rails without changing account content layout. Grid and list cards share this frame; keyboard focus and selection use its hover state. Sidebar icon sizing compensates for the original artwork's transparent padding without altering the source images.

The loading screen plays the client's own `hextech-loading-animation` (Lottie `loader_large_v2`, 300×300, 70 frames at 30 fps). The client embeds it as data in `rcp-fe-lol-shared-components.js` rather than shipping a file, so it lives outside the manifest, in `ui/src/assets/hextech-loader.json` (8,219 bytes, SHA-256 `04cd03caf513e877dd688aa5573ddf373b5ce40b7d06c58d7710327e965717af`). It is vector-only and plays through `lottie-web`'s SVG renderer (MIT), as in the client. Framed dropdowns, the context menu, the ranked countdown and the settings layout follow the live client's `lol-uikit-framed-dropdown`, `lol-uikit-context-menu`, ranked ladder header and settings dialog, inspected on 2026-10-12.

Verify the committed bundle with `npm run assets:verify`. Restore missing files with `node scripts/league-assets.mjs --fetch`. Existing valid files are retained. An upstream hash mismatch requires manual review before changing the manifest; the CDN may re-encode PNGs, so a different hash does not necessarily mean different artwork. To update the patch, change the script's source version, inspect the resulting art and motion, and deliberately regenerate the manifest and showcase.

## CJK fonts

Chinese text needs glyphs the League typekit does not ship. Two open-source families cover it, both under the [SIL Open Font License 1.1](https://openfontlicense.org), which permits bundling and subsetting:

- **Noto Sans SC** (`noto-sans-sc-{regular,medium,bold}.otf`) — the interface face. A sans-serif matches the weight of the Latin text at 12–13px.
- **LXGW WenKai** (霞鹜文楷, `lxgw-wenkai-{regular,medium}.ttf`) — the display face, for the Beaufort-stacked headings where its handwritten strokes are legible and distinctive. It ships no Bold, so it is declared for 400–500 and 600–900 separately.

Neither is a system font: relying on the OS would change the interface per machine, and shipping a Windows font would be a licensing problem.

`node scripts/cjk-font.mjs` generates both families, subset to the characters the interface can actually render — 764 of each font's ~20,000 — which keeps all five files near 1.2 MB instead of 70 MB. Re-run it after changing any UI string. Sources are read from the Windows font directories rather than vendored; a missing one is reported and skipped.

`npm run assets:verify` does not cover these files: `ui/public/league/manifest.json` records Riot's assets and their upstream URLs, and these fonts have neither.

League Accounts is an independent community project, not endorsed by Riot Games. Riot Games, League of Legends and Teamfight Tactics are trademarks or registered trademarks of Riot Games, Inc.
