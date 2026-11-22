// SPDX-License-Identifier: Apache-2.0
// Copyright (c) 2026 sovith3

const statusEl = document.getElementById("status");
const logEl = document.getElementById("log");
const buttons = [...document.querySelectorAll("button[data-action]")];

const port = chrome.runtime.connect({ name: "sentinel-popup" });
let counter = 0;

function log(line) {
  const stamp = new Date().toLocaleTimeString();
  logEl.textContent += `[${stamp}] ${line}\n`;
  logEl.scrollTop = logEl.scrollHeight;
}

/** Champion-select frames are frequent, so they get a one-line summary. */
function describeStream(adapter, data) {
  if (adapter !== "lol_lcu") {
    return JSON.stringify(data).slice(0, 240);
  }

  const sequence = typeof data.sequence === "number" ? `#${data.sequence} ` : "";
  const source = data.source ? `[${data.source}] ` : "";

  if (data.inChampSelect === false) {
    return `${sequence}${source}champion select ended`;
  }

  const summary = data.summary;
  if (!summary) {
    return `${sequence}${source}${data.event || "event"} ${JSON.stringify(data.phase ?? "").slice(0, 120)}`;
  }

  const picks = (summary.myTeam || [])
    .map((member) => member.championId || member.championPickIntent || "?")
    .join(",");
  const seconds =
    typeof summary.timeLeftMs === "number" ? `${Math.round(summary.timeLeftMs / 1000)}s` : "?";
  return `${sequence}${source}${summary.phase || "ChampSelect"} · ${seconds} left · my team ${picks}`;
}

function setStatus(text, kind) {
  statusEl.textContent = text;
  statusEl.className = kind || "";
}

port.onMessage.addListener((frame) => {
  const { event, data, adapter } = frame || {};

  if (event === "STATUS") {
    if (data.connected === true || data.connected === false) {
      setStatus(data.message || "bridge ready", data.connected ? "ok" : "");
    } else {
      log(`${adapter}: ${data.message}`);
    }
    return;
  }

  if (event === "ERROR") {
    setStatus(data.code || "error", "error");
    log(`${adapter}: ${data.code} - ${data.message}`);
    return;
  }

  if (event === "RESULT") {
    setStatus("ready", "ok");
    log(`${adapter}: ${JSON.stringify(data).slice(0, 900)}`);
    return;
  }

  if (event === "STREAM") {
    log(`${adapter} stream: ${describeStream(adapter, data || {})}`);
  }
});

port.onDisconnect.addListener(() => {
  setStatus("extension worker stopped", "error");
});

for (const button of buttons) {
  button.addEventListener("click", () => {
    counter += 1;
    const request = {
      adapter: button.dataset.adapter,
      action: button.dataset.action,
      payload: {}
    };
    log(`-> ${request.adapter}/${request.action} (#${counter})`);
    port.postMessage(request);
  });
}

chrome.runtime.sendMessage({ type: "BRIDGE_STATUS" }, (reply) => {
  if (chrome.runtime.lastError) {
    setStatus("native host unavailable", "error");
    return;
  }
  setStatus(
    reply && reply.connected ? `connected to ${reply.host}` : "native host starting…",
    reply && reply.connected ? "ok" : ""
  );
});

log("popup ready");
