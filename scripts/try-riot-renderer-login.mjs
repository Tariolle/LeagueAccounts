// Development experiment: run Riot's existing login form without OS keystrokes.
// Usage: node scripts/try-riot-renderer-login.mjs <saved account ID or exact label>
// Add --verify-fill-only to verify React field state without submitting login.
// Runs Riot Services headlessly with exactly one controlled renderer. Restores
// a normal renderer without DevTools afterward. Does not modify installed files.
import { execFileSync, spawn } from "node:child_process";
import { readFile } from "node:fs/promises";
import path from "node:path";
import net from "node:net";
import puppeteer from "puppeteer-core";
import { localRequest, readPassword } from "./riot-login-support.mjs";

const sleep = ms => new Promise(resolve => setTimeout(resolve, ms));
const status = (step, extra = {}) => console.log(JSON.stringify({ step, ...extra }));
const powershell = source => execFileSync("powershell.exe", ["-NoProfile", "-NonInteractive", "-Command", source], {
  encoding: "utf8", stdio: ["pipe", "pipe", "pipe"], windowsHide: true, timeout: 15_000,
});
let browser;
let socket;
let rendererPath;
let rendererArgs;
let sequence = 0;
const pending = new Map();

async function evaluate(fn, ...args) {
  const id = ++sequence;
  const response = new Promise((resolve, reject) => {
    const timer = setTimeout(() => { pending.delete(id); reject(new Error("renderer_timeout")); }, 10_000);
    pending.set(id, { resolve: value => { clearTimeout(timer); resolve(value); }, reject: () => {
      clearTimeout(timer); reject(new Error("renderer_evaluation_failed"));
    } });
  });
  // Never log the expression: its arguments may include credentials.
  socket.send(JSON.stringify({ id, method: "Runtime.evaluate", params: {
    expression: `(${fn.toString()})(...${JSON.stringify(args)})`, returnByValue: true, awaitPromise: true,
  } }));
  return response;
}

async function targetConnected(account) {
  const session = await localRequest("/player-session-lifecycle/v1/session");
  if (session.code !== 200) throw new Error("client_state_unavailable");
  if (!session.json?.puuid) return false;
  const info = await localRequest("/rso-auth/v1/authorization/userinfo");
  let user = info.json?.userInfo;
  if (typeof user === "string") { try { user = JSON.parse(user); } catch { user = null; } }
  if (!user?.username) return false;
  if (user.username.toLowerCase() !== account.account_id.toLowerCase()) throw new Error("account_mismatch");
  return true;
}

async function main() {
  const label = process.argv[2]?.toLowerCase();
  if (!label) throw new Error("account_label_required");
  const accounts = JSON.parse(await readFile(path.join(process.env.APPDATA, "LeagueAccounts/league_accounts.json"), "utf8"));
  const matches = accounts.filter(a => [a.name, a.account_id].some(s => s?.toLowerCase() === label));
  if (matches.length !== 1) throw new Error("account_not_unique");
  const account = matches[0];
  // Check before closing any window: an existing target session needs no work.
  try {
    if (await targetConnected(account)) { status("target_account_already_connected"); return; }
  } catch (error) {
    if (error.code !== "ENOENT" && error.message !== "account_mismatch") throw error;
  }
  const tasks = powershell("@(Get-CimInstance Win32_Process | Where-Object {$_.Name -in @('LeagueClient.exe','League of Legends.exe','TFTClient.exe')}).Count");
  if (Number(tasks.trim())) throw new Error("game_running");
  const installs = JSON.parse(await readFile(path.join(process.env.ProgramData, "Riot Games/RiotClientInstalls.json"), "utf8"));
  const servicePath = installs.rc_default;
  if (!servicePath || path.basename(servicePath).toLowerCase() !== "riotclientservices.exe") throw new Error("client_not_installed");
  const renderers = Number(powershell("@(Get-CimInstance Win32_Process | Where-Object {$_.Name -eq 'Riot Client.exe' -and $_.CommandLine -notmatch '--type='}).Count").trim());
  if (renderers) {
    const headlessService = powershell("[bool](Get-CimInstance Win32_Process | Where-Object {$_.Name -eq 'RiotClientServices.exe' -and $_.CommandLine -match '(?:^|\\s)--headless(?:\\s|$)'})").trim() === "True";
    if (!headlessService) throw new Error("close_riot_client_first");
    powershell("Get-CimInstance Win32_Process | Where-Object {$_.Name -eq 'Riot Client.exe' -and $_.CommandLine -notmatch '--type='} | ForEach-Object {Stop-Process -Id $_.ProcessId -ErrorAction Stop}");
    // Services can exit asynchronously when the last UI connection disappears.
    await sleep(4000);
  }
  const serviceCount = Number(powershell("@(Get-Process -Name RiotClientServices -ErrorAction SilentlyContinue).Count").trim());
  if (!serviceCount) {
    const service = spawn(servicePath, ["--headless"], { detached: true, stdio: "ignore", windowsHide: true });
    service.unref();
    let ready = false;
    for (let i = 0; i < 40; i++) {
      try { ready = (await localRequest("/player-session-lifecycle/v1/session")).code === 200; } catch {}
      if (ready) break;
      await sleep(500);
    }
    if (!ready) throw new Error("client_state_unavailable");
  }
  const session = await localRequest("/player-session-lifecycle/v1/session");
  if (session.code !== 200) throw new Error("client_state_unavailable");
  if (session.json?.puuid) {
    let matchesTarget = false;
    try { matchesTarget = await targetConnected(account); }
    catch (error) { if (error.message !== "account_mismatch") throw error; }
    if (matchesTarget) { status("target_account_already_connected"); return; }
    const logout = await localRequest("/player-session-lifecycle/v1/session", "DELETE");
    if (logout.code < 200 || logout.code >= 300) throw new Error("sign_out_failed");
    status("previous_session_signed_out");
  }

  const lock = (await readFile(path.join(process.env.LOCALAPPDATA, "Riot Games/Riot Client/Config/lockfile"), "utf8")).trim().split(":");
  if (lock.length !== 5 || !/^\d+$/.test(lock[1]) || !/^\d+$/.test(lock[2])) throw new Error("client_state_unavailable");
  const machine = await localRequest("/riotclient/machine-id");
  const hostSession = await localRequest("/product-session/v1/host-session/id");
  if (machine.code !== 200 || typeof machine.json !== "string" || hostSession.code !== 200 || typeof hostSession.json !== "string") throw new Error("client_state_unavailable");
  const clientRoot = path.dirname(servicePath);
  rendererPath = path.join(clientRoot, "RiotClientElectron/Riot Client.exe");
  rendererArgs = [
    `--app-port=${lock[2]}`, `--remoting-auth-token=${lock[3]}`, `--app-pid=${lock[1]}`,
    `--app-root=${clientRoot}`, `--user-data-root=${path.join(process.env.LOCALAPPDATA, "Riot Games/Riot Client")}`,
    `--log-dir=${path.join(process.env.LOCALAPPDATA, "Riot Games/Riot Client/Logs/Riot Client UX Logs")}`,
    `--machine-id=${machine.json}`, `--session-id=${hostSession.json}`, "--crashpad-environment=prod", "--enable-hardware-acceleration",
  ];
  // Chromium treats port 0 as an automation launch. Allocate a concrete local
  // debugger port, matching an ordinary developer-attached Riot renderer.
  const debugPort = await new Promise((resolve, reject) => {
    const listener = net.createServer();
    listener.once("error", reject);
    listener.listen(0, "127.0.0.1", () => {
      const port = listener.address().port;
      listener.close(error => error ? reject(error) : resolve(port));
    });
  });
  browser = await puppeteer.launch({ executablePath: rendererPath, args: [
    ...rendererArgs, "--allow-chrome-dev-tools", "--remote-debugging-address=127.0.0.1", `--remote-debugging-port=${debugPort}`,
  ], ignoreDefaultArgs: true, headless: false, defaultViewport: null, timeout: 25_000, waitForInitialPage: false });
  const endpoint = new URL(browser.wsEndpoint());
  if (endpoint.hostname !== "127.0.0.1") throw new Error("debugger_not_loopback");
  // Electron exposes its page through /json/list but Puppeteer's automatic
  // target discovery does not see it. Connect directly to the page's CDP socket.
  let target;
  for (let i = 0; i < 40 && !target; i++) {
    const pages = await (await fetch(`http://127.0.0.1:${endpoint.port}/json/list`, { signal: AbortSignal.timeout(3000) })).json();
    target = pages.find(p => p.type === "page" && p.title === "Riot Client"
      && /^http:\/\/127\.0\.0\.1:\d+\//.test(p.url));
    if (!target) await sleep(500);
  }
  if (!target) throw new Error("renderer_not_found");
  const targetSocket = new URL(target.webSocketDebuggerUrl);
  if (targetSocket.hostname !== "127.0.0.1" || targetSocket.port !== endpoint.port) throw new Error("debugger_not_loopback");
  socket = new WebSocket(targetSocket);
  await new Promise((resolve, reject) => {
    const timer = setTimeout(() => reject(new Error("renderer_timeout")), 5000);
    socket.onopen = () => { clearTimeout(timer); resolve(); };
    socket.onerror = () => { clearTimeout(timer); reject(new Error("renderer_connection_failed")); };
  });
  socket.onmessage = event => {
    const message = JSON.parse(event.data);
    const request = pending.get(message.id);
    if (!request) return;
    pending.delete(message.id);
    if (message.error || message.result?.exceptionDetails) request.reject();
    else request.resolve(message.result?.result?.value);
  };
  socket.onclose = () => {
    for (const request of pending.values()) request.reject();
    pending.clear();
  };
  status("native_renderer_attached");
  const activeRenderers = Number(powershell("@(Get-CimInstance Win32_Process | Where-Object {$_.Name -eq 'Riot Client.exe' -and $_.CommandLine -notmatch '--type='}).Count").trim());
  if (activeRenderers !== 1) throw new Error("multiple_renderers");
  status("single_renderer_verified");
  status("renderer_context", { webdriver: await evaluate(() => navigator.webdriver === true) });
  let ready = false;
  for (let i = 0; i < 40; i++) {
    ready = await evaluate(() => !!document.querySelector('input[name="username"]')
      && !!document.querySelector('input[name="password"]'));
    if (ready) break;
    await sleep(500);
  }
  if (!ready) throw new Error("login_form_not_ready");
  await sleep(1500);
  await loginAttempt(account);
}

async function loginAttempt(account) {
  let password = readPassword(account);
  if (!password) throw new Error("credential_unavailable");
  await evaluate(() => { delete window.__leagueAccountsAuthDiagnostic; });
  await evaluate(() => window.riotInvoke({ request: JSON.stringify({ name: "Window.Minimize", params: [] }) }));
  const filled = await evaluate((username, password) => {
    for (const [name, value] of [["username", username], ["password", password]]) {
      const input = document.querySelector(`input[name="${name}"]`);
      const key = Object.keys(input).find(k => k.startsWith("__reactProps$"));
      if (typeof input[key]?.onChange !== "function" || input.disabled) return false;
      input.value = value;
      input[key].onChange({ target: input, currentTarget: input });
    }
    return true;
  }, account.account_id, password);
  if (!filled) throw new Error("login_handlers_unavailable");
  await sleep(300);
  const verified = await evaluate((username, password) => {
    return [["username", username], ["password", password]].every(([name, value]) => {
      const input = document.querySelector(`input[name="${name}"]`);
      const key = Object.keys(input).find(k => k.startsWith("__reactProps$"));
      return input.value === value && input[key]?.value === value;
    });
  }, account.account_id, password);
  const monitored = await evaluate(async (username, password) => {
    const script = [...document.scripts].find(s => new URL(s.src || location.href).pathname.endsWith("/renderer.js"));
    if (!script) return false;
    const source = await (await fetch(script.src)).text();
    const exportName = source.slice(-30000).match(/getBinding as ([\w$]+)/)?.[1];
    if (!exportName) return false;
    const module = await import(script.src);
    const binding = module[exportName]();
    const original = binding.post;
    binding.post = async function(route, body, ...rest) {
      if (route !== "/rso-authenticator/v1/authentication/riot-identity/complete") return original.call(this, route, body, ...rest);
      const diagnostic = { usernameMatches: body?.username === username, passwordMatches: body?.password === password };
      password = "";
      window.__leagueAccountsAuthDiagnostic = diagnostic;
      try {
        const result = await original.call(this, route, body, ...rest);
        const error = typeof result?.error === "string" ? result.error.toLowerCase() : "";
        diagnostic.result = error.includes("captcha") ? "captcha_rejected" : error.includes("auth_failure") ? "auth_failure"
          : error.includes("rate") ? "rate_limited" : error ? "other_error"
            : ["success", "auth", "multifactor"].includes(result?.type) ? result.type : "other";
        return result;
      } finally { binding.post = original; }
    };
    return true;
  }, account.account_id, password);
  password = "";
  if (!verified) throw new Error("credential_state_mismatch");
  if (!monitored) throw new Error("request_monitor_unavailable");
  status("native_credential_state_verified");
  if (process.argv.includes("--verify-fill-only")) return;
  const submitted = await evaluate(() => {
    const form = document.querySelector('input[name="password"]')?.form;
    if (!form) return false;
    form.requestSubmit();
    return true;
  });
  if (!submitted) throw new Error("login_handlers_unavailable");
  status("native_form_submitted_without_keystrokes");
  let challengeShown = false;
  let requestReported = false;
  let resultReported = false;
  for (let i = 0; i < 300; i++) {
    const diagnostic = await evaluate(() => window.__leagueAccountsAuthDiagnostic ?? null);
    if (diagnostic && !requestReported) {
      requestReported = true;
      status("native_request_verified", { usernameMatches: diagnostic.usernameMatches === true, passwordMatches: diagnostic.passwordMatches === true });
    }
    if (diagnostic?.result && !resultReported) {
      resultReported = true;
      status("native_request_result", { category: ["captcha_rejected", "auth_failure", "rate_limited", "other_error", "success", "auth", "multifactor", "other"].includes(diagnostic.result) ? diagnostic.result : "other" });
    }
    if (await targetConnected(account)) { status("target_account_connected"); return; }
    const auth = await localRequest("/rso-authenticator/v1/authentication");
    if (auth.json?.error) {
      const error = String(auth.json.error).toLowerCase();
      status("native_auth_response", { category: error.includes("captcha") ? "captcha_rejected"
        : error.includes("auth_failure") ? "auth_failure" : error.includes("rate") ? "rate_limited" : "other" });
      throw new Error(error.includes("auth_failure") ? "native_auth_failure" : "authentication_rejected");
    }
    const formReset = await evaluate(() => {
      const input = document.querySelector('input[name="password"]');
      return !!input && !input.value && !!document.querySelector('[data-testid="login-form-error-tooltip"]');
    });
    if (formReset) throw new Error(diagnostic?.result === "auth_failure" ? "native_auth_failure" : "native_login_reset");
    const challenge = await evaluate(() => {
      const overlay = document.querySelector('[aria-label="hcaptcha challenge"]');
      return !!overlay && overlay.getBoundingClientRect().width > 0 && getComputedStyle(overlay).visibility !== "hidden"
        && getComputedStyle(overlay).opacity !== "0";
    });
    if (challenge && !challengeShown) {
      challengeShown = true;
      await evaluate(() => window.riotInvoke({ request: JSON.stringify({ name: "Window.Restore", params: [] }) }));
      status("awaiting_human_captcha_in_riot_client");
    }
    if (auth.json?.type === "multifactor" && !challengeShown) {
      challengeShown = true;
      await evaluate(() => window.riotInvoke({ request: JSON.stringify({ name: "Window.Restore", params: [] }) }));
      status("awaiting_human_mfa_in_riot_client");
    }
    await sleep(1000);
  }
  throw new Error("sign_in_timeout");
}

try { await main(); }
catch (error) {
  const known = new Set(["account_label_required", "account_not_unique", "game_running", "client_not_installed", "close_riot_client_first", "multiple_renderers", "client_state_unavailable", "sign_out_failed",
    "debugger_not_loopback", "renderer_not_found", "renderer_timeout", "renderer_connection_failed", "renderer_evaluation_failed",
    "login_form_not_ready", "credential_unavailable", "login_handlers_unavailable", "credential_state_mismatch", "request_monitor_unavailable", "authentication_rejected", "native_login_reset", "account_mismatch", "sign_in_timeout", "native_auth_failure"]);
  status("experiment_failed", { reason: known.has(error?.message) ? error.message : "unexpected_error" });
  process.exitCode = 1;
} finally {
  socket?.close();
  for (const request of pending.values()) request.reject();
  pending.clear();
  await browser?.close().catch(() => {});
  if (rendererPath && rendererArgs) {
    const normalRenderer = spawn(rendererPath, rendererArgs, { detached: true, stdio: "ignore", windowsHide: true });
    normalRenderer.on("error", () => status("normal_renderer_restore_failed"));
    normalRenderer.unref();
    status("normal_renderer_restored");
  }
}
