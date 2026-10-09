// Private credential and loopback API helpers for the renderer login experiment.
import { execFileSync } from "node:child_process";
import { readFile } from "node:fs/promises";
import https from "node:https";
import path from "node:path";

function request(url, { method = "GET", body, localAuth } = {}) {
  if (url.protocol !== "https:" || url.hostname !== "127.0.0.1") throw new Error("destination_rejected");
  return new Promise((resolve, reject) => {
    const headers = { Accept: "application/json", "Content-Type": "application/json" };
    const req = https.request(url, { method, headers, auth: localAuth,
      rejectUnauthorized: false }, (res) => {
      const chunks = [];
      let size = 0;
      res.on("data", (chunk) => {
        size += chunk.length;
        if (size > 2 * 1024 * 1024) res.destroy();
        else chunks.push(chunk);
      });
      res.on("error", () => reject(new Error("response_failed")));
      res.on("end", () => {
        let json = null;
        try { json = JSON.parse(Buffer.concat(chunks).toString("utf8")); } catch {}
        resolve({ code: res.statusCode, json });
      });
    });
    const timer = setTimeout(() => req.destroy(), 15_000);
    req.on("close", () => clearTimeout(timer));
    req.on("error", () => reject(new Error("request_failed")));
    req.end(body === undefined ? undefined : JSON.stringify(body));
  });
}

export async function localRequest(route, method = "GET", body) {
  const parts = (await readFile(path.join(process.env.LOCALAPPDATA,
    "Riot Games/Riot Client/Config/lockfile"), "utf8")).trim().split(":");
  if (parts.length !== 5 || !/^\d+$/.test(parts[2]) || Number(parts[2]) < 1 || Number(parts[2]) > 65535) {
    throw new Error("invalid_lockfile");
  }
  return request(new URL(`https://127.0.0.1:${parts[2]}${route}`), {
    method, body, localAuth: `riot:${parts[3]}`,
  });
}

export function readPassword(account) {
  // The child writes only to this private pipe, never to the terminal or disk.
  const source = String.raw`
$ErrorActionPreference = 'Stop'
$taskTarget = [Console]::In.ReadToEnd()
Add-Type -TypeDefinition @'
using System;
using System.Runtime.InteropServices;
public static class RiotTestCredential {
  [StructLayout(LayoutKind.Sequential, CharSet=CharSet.Unicode)]
  public struct Credential {
    public UInt32 Flags, Type;
    public string TargetName, Comment;
    public System.Runtime.InteropServices.ComTypes.FILETIME LastWritten;
    public UInt32 BlobSize;
    public IntPtr Blob;
    public UInt32 Persist, AttributeCount;
    public IntPtr Attributes;
    public string TargetAlias, UserName;
  }
  [DllImport("advapi32.dll", EntryPoint="CredReadW", CharSet=CharSet.Unicode, SetLastError=true)]
  public static extern bool Read(string target, UInt32 type, UInt32 flags, out IntPtr result);
  [DllImport("advapi32.dll")] public static extern void CredFree(IntPtr pointer);
  public static string Password(string target) {
    IntPtr pointer;
    if (!Read(target, 1, 0, out pointer)) throw new Exception("credential_unavailable");
    try {
      var value = (Credential)Marshal.PtrToStructure(pointer, typeof(Credential));
      return Marshal.PtrToStringUni(value.Blob, (int)value.BlobSize / 2);
    } finally { CredFree(pointer); }
  }
}
'@
[Console]::OutputEncoding = New-Object System.Text.UTF8Encoding($false)
[Console]::Write([RiotTestCredential]::Password($taskTarget))
`;
  try {
    return execFileSync("powershell.exe", ["-NoProfile", "-NonInteractive", "-Command", source], {
      input: `${account.region}:${account.account_id}@LeagueAccounts`, encoding: "utf8",
      stdio: ["pipe", "pipe", "pipe"], windowsHide: true, timeout: 15_000,
    });
  } catch { throw new Error("credential_unavailable"); }
}
