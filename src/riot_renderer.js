// Runs only inside the owned Riot renderer. Never return credentials or tokens.
(() => {
  const state = { result: "pending" };
  const field = name => document.querySelector(`input[name="${name}"]`);
  const props = input => input?.[Object.keys(input).find(k => k.startsWith("__reactProps$"))];
  const windowAction = name => window.riotInvoke({ request: JSON.stringify({ name, params: [] }) });
  globalThis.__leagueAccountsLogin = {
    ready: () => !!field("username") && !!field("password")
      && document.querySelector('[data-testid="login-form"]')?.dataset.loggingIn === "false",
    async fill(username, password) {
      if (navigator.webdriver || !this.ready()) return false;
      const script = [...document.scripts].find(s => new URL(s.src || location.href).pathname.endsWith("/renderer.js"));
      if (!script) return false;
      const source = await (await fetch(script.src)).text();
      const exportName = source.slice(-30000).match(/getBinding as ([\w$]+)/)?.[1];
      if (!exportName) return false;
      const module = await import(script.src);
      const binding = module[exportName]();
      const original = binding.post;
      // Capture fixed response categories before Riot resets its auth session.
      // Recheck the actual submitted account as well as the visible field state.
      binding.post = async function(route, body, ...rest) {
        if (route !== "/rso-authenticator/v1/authentication/riot-identity/complete") return original.call(this, route, body, ...rest);
        try {
          if (body?.username !== username || body?.password !== password) {
            state.result = "changed";
            throw new Error("login_target_changed");
          }
          password = "";
          const result = await original.call(this, route, body, ...rest);
          const error = typeof result?.error === "string" ? result.error.toLowerCase() : "";
          state.result = error.includes("captcha") ? "captcha_rejected"
            : error.includes("rate") ? "rate_limited" : error ? "rejected"
              : result?.type === "multifactor" ? "verification"
                : result?.type === "success" ? "success" : "pending";
          return result;
        } catch (error) {
          if (state.result === "pending") state.result = "rejected";
          throw error;
        } finally { password = ""; binding.post = original; }
      };
      windowAction("Window.Minimize");
      for (const [name, value] of [["username", username], ["password", password]]) {
        const input = field(name);
        if (typeof props(input)?.onChange !== "function" || input.disabled) return false;
        input.value = value;
        props(input).onChange({ target: input, currentTarget: input });
      }
      await new Promise(resolve => setTimeout(resolve, 300));
      return [["username", username], ["password", password]].every(([name, value]) =>
        field(name)?.value === value && props(field(name))?.value === value);
    },
    submit() {
      const form = field("password")?.form;
      if (!form) return false;
      form.requestSubmit();
      return true;
    },
    status() {
      const overlay = document.querySelector('[aria-label="hcaptcha challenge"]');
      const captcha = !!overlay && overlay.getBoundingClientRect().width > 0
        && getComputedStyle(overlay).visibility !== "hidden" && getComputedStyle(overlay).opacity !== "0";
      if (captcha || state.result === "verification") return "verification";
      if (state.result !== "pending") return state.result;
      if (field("password") && !field("password").value && document.querySelector('[data-testid="login-form-error-tooltip"]')) return "rejected";
      return "pending";
    },
    showVerification() { windowAction("Window.Restore"); windowAction("Window.Show"); },
  };
  return true;
})()
