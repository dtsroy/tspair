// tspair dashboard: wires the Tauri IPC surface (commands + events) to the UI.
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";

interface Status {
  connected: boolean;
  role: string;
  mine: string;
  peer: string | null;
  peer_ip: string;
  peer_advertised: boolean | null;
  status_line: string;
}

interface PeerInfo {
  hostname: string;
  ip: string;
  online: boolean;
  advertised: boolean;
}

interface ActionResult {
  ok: boolean;
  message: string;
}

interface ConfigDto {
  role: string;
  port: number;
  ping_interval: number;
  status_interval: number;
}

const MINE_LABELS: Record<string, string> = {
  none: "Independent (no exit node)",
  using_other: "Using peer as exit node",
  being_used: "Serving as peer's exit node",
};
const PEER_LABELS: Record<string, string> = {
  none: "Independent",
  using_other: "Using this device",
  being_used: "Routing via this device",
};

function el<T extends HTMLElement>(id: string): T {
  return document.getElementById(id) as T;
}

function logLine(msg: string): void {
  const log = el<HTMLDivElement>("log");
  const ts = new Date().toLocaleTimeString();
  log.textContent += `[${ts}] ${msg}\n`;
  log.scrollTop = log.scrollHeight;
}

function toast(title: string, body: string): void {
  const t = document.createElement("div");
  t.className = "toast";
  const h = document.createElement("div");
  h.className = "title";
  h.textContent = title;
  const b = document.createElement("div");
  b.className = "body";
  b.textContent = body;
  t.append(h, b);
  document.body.appendChild(t);
  setTimeout(() => t.remove(), 5000);
}

function renderStatus(s: Status): void {
  el<HTMLSpanElement>("conn-dot").className = s.connected ? "dot on" : "dot off";
  el<HTMLSpanElement>("conn-label").textContent = s.status_line;

  const mine = el<HTMLDivElement>("mine");
  mine.textContent = MINE_LABELS[s.mine] ?? s.mine;
  mine.className = `state state-${s.mine.replace("_", "-")}`;

  const peer = el<HTMLDivElement>("peer");
  peer.textContent = s.peer ? (PEER_LABELS[s.peer] ?? s.peer) : "unknown";
  peer.className = `state state-${s.peer ? s.peer.replace("_", "-") : "unknown"}`;

  el<HTMLButtonElement>("btn-stop").disabled = s.mine !== "using_other";
}

function renderPeer(p: PeerInfo): void {
  const adv = p.advertised ? " · advertising as exit node" : "";
  el<HTMLDivElement>("peer-info").textContent =
    `${p.hostname || "unknown"} (${p.ip}) — ${p.online ? "online" : "offline"}${adv}`;
}

async function loadStatus(): Promise<void> {
  renderStatus(await invoke<Status>("get_status"));
}

async function loadConfig(): Promise<void> {
  const cfg = await invoke<ConfigDto>("get_config");
  el<HTMLInputElement>("cfg-role").value = cfg.role;
  el<HTMLInputElement>("cfg-port").value = String(cfg.port);
  el<HTMLInputElement>("cfg-ping").value = String(cfg.ping_interval);
  el<HTMLInputElement>("cfg-status").value = String(cfg.status_interval);
}

async function runAction(name: "use_peer" | "stop_using_peer"): Promise<void> {
  logLine(`action: ${name}…`);
  const res = await invoke<ActionResult>(name);
  logLine(`action ${name}: ok=${res.ok} — ${res.message}`);
  toast("tspair", res.message);
}

async function saveConfig(): Promise<void> {
  const cfg: ConfigDto = {
    role: el<HTMLInputElement>("cfg-role").value,
    port: Number(el<HTMLInputElement>("cfg-port").value),
    ping_interval: Number(el<HTMLInputElement>("cfg-ping").value) || 15,
    status_interval: Number(el<HTMLInputElement>("cfg-status").value) || 30,
  };
  const res = await invoke<ActionResult>("set_config", { config: cfg });
  el<HTMLSpanElement>("cfg-note").textContent = res.message;
  logLine(`config saved: ${res.message}`);
}

async function main(): Promise<void> {
  el<HTMLButtonElement>("btn-use").addEventListener("click", () => runAction("use_peer"));
  el<HTMLButtonElement>("btn-stop").addEventListener("click", () => runAction("stop_using_peer"));
  el<HTMLButtonElement>("btn-refresh").addEventListener("click", async () => {
    await invoke("refresh_now");
    logLine("manual refresh triggered");
  });
  el<HTMLButtonElement>("btn-save-cfg").addEventListener("click", saveConfig);

  await listen<Status>("state-changed", (e) => renderStatus(e.payload));
  await listen<PeerInfo>("peer-info", (e) => {
    renderPeer(e.payload);
    logLine(
      `peer detected: ${e.payload.hostname} (${e.payload.ip}) ` +
        `${e.payload.online ? "online" : "offline"}` +
        (e.payload.advertised ? " · exit node advertised" : "")
    );
  });
  await listen<{ title: string; body: string }>("notify", (e) =>
    toast(e.payload.title, e.payload.body)
  );

  try {
    await loadConfig();
    await loadStatus();
  } catch (err) {
    logLine(`init failed: ${err}`);
  }
}

main();
