// SPDX-License-Identifier: Apache-2.0
// Copyright (c) 2026 sovith3

/**
 * Service worker that owns the single native messaging port.
 *
 * Page  --postMessage--> content.js --runtime port--> this worker
 * this worker --connectNative--> rust-sentinel-bridge.exe
 */

const HOST_NAME = "com.rustsentinel.bridge";
const RECONNECT_DELAY_MS = 2000;
const REQUEST_TIMEOUT_MS = 120000;

let nativePort = null;
let nativeReady = false;
let reconnectTimer = null;
let sequence = 0;

/** requestId -> Set<chrome.runtime.Port> that must receive the frames. */
const pending = new Map();
/** Ports opened by content scripts and by the popup. */
const clients = new Set();

function newId() {
  if (typeof crypto !== "undefined" && crypto.randomUUID) {
    return crypto.randomUUID();
  }
  sequence += 1;
  return `req-${Date.now()}-${sequence}`;
}

function safePost(port, frame) {
  try {
    port.postMessage(frame);
    return true;
  } catch (error) {
    clients.delete(port);
    return false;
  }
}

function broadcast(frame) {
  for (const port of clients) {
    safePost(port, frame);
  }
}

function notify(event, data) {
  broadcast({
    id: "bridge-status",
    adapter: "core",
    event,
    data: { ...data, host: HOST_NAME, timestamp: Date.now() }
  });
}

function connectNative() {
  if (nativePort) {
    return nativePort;
  }

  try {
    nativePort = chrome.runtime.connectNative(HOST_NAME);
  } catch (error) {
    nativePort = null;
    notify("ERROR", { code: "NATIVE_CONNECT_FAILED", message: String(error) });
    scheduleReconnect();
    return null;
  }

  nativeReady = true;
  nativePort.onMessage.addListener(onNativeMessage);
  nativePort.onDisconnect.addListener(() => {
    const error = chrome.runtime.lastError;
    nativePort = null;
    nativeReady = false;
    notify("ERROR", {
      code: "NATIVE_DISCONNECTED",
      message: error ? error.message : "native host closed the pipe",
      hint: "Run rust-sentinel-bridge.exe once to (re)install the manifest, then reload the extension."
    });
    scheduleReconnect();
  });

  notify("STATUS", { message: "native host connected" });
  return nativePort;
}

function scheduleReconnect() {
  if (reconnectTimer) {
    return;
  }
  reconnectTimer = setTimeout(() => {
    reconnectTimer = null;
    connectNative();
  }, RECONNECT_DELAY_MS);
}

function onNativeMessage(frame) {
  if (!frame || typeof frame !== "object") {
    return;
  }

  const targets = pending.get(frame.id);
  if (targets) {
    for (const port of [...targets]) {
      safePost(port, frame);
    }
    if (frame.event === "RESULT" || frame.event === "ERROR") {
      pending.delete(frame.id);
    }
    return;
  }

  // Unsolicited frames (for example an automatic download scan) reach everyone.
  broadcast(frame);
}

function sendRequest(request, port) {
  const native = connectNative();
  if (!native) {
    return null;
  }

  const id = newId();
  const targets = new Set();
  if (port) {
    targets.add(port);
  }
  pending.set(id, targets);

  native.postMessage({
    id,
    adapter: request.adapter || "core",
    action: request.action || "PING",
    payload: request.payload || {}
  });

  setTimeout(() => {
    if (pending.has(id)) {
      pending.delete(id);
      if (port) {
        safePost(port, {
          id,
          adapter: request.adapter || "core",
          event: "ERROR",
          data: {
            code: "REQUEST_TIMEOUT",
            message: `no answer from ${HOST_NAME} within ${REQUEST_TIMEOUT_MS / 1000}s`,
            timestamp: Date.now()
          }
        });
      }
    }
  }, REQUEST_TIMEOUT_MS);

  return id;
}

chrome.runtime.onConnect.addListener((port) => {
  if (port.name !== "sentinel-page" && port.name !== "sentinel-popup") {
    return;
  }

  clients.add(port);
  connectNative();

  safePost(port, {
    id: "bridge-status",
    adapter: "core",
    event: "STATUS",
    data: {
      message: nativeReady ? "bridge ready" : "bridge connecting",
      host: HOST_NAME,
      connected: nativeReady,
      timestamp: Date.now()
    }
  });

  port.onMessage.addListener((message) => {
    if (!message || typeof message !== "object") {
      return;
    }
    const request = message.request || message;
    sendRequest(request, port);
  });

  port.onDisconnect.addListener(() => {
    clients.delete(port);
  });
});

chrome.runtime.onMessage.addListener((message, _sender, sendResponse) => {
  if (message && message.type === "BRIDGE_STATUS") {
    sendResponse({ connected: nativeReady, host: HOST_NAME });
    return false;
  }
  return false;
});

if (chrome.downloads && chrome.downloads.onChanged) {
  chrome.downloads.onChanged.addListener((delta) => {
    if (!delta.state || delta.state.current !== "complete") {
      return;
    }
    if (!nativeReady && !connectNative()) {
      return;
    }
    sendRequest(
      {
        adapter: "security_scanner",
        action: "SCAN_DOWNLOADS_LATEST",
        payload: {}
      },
      null
    );
  });
}

chrome.runtime.onInstalled.addListener(() => {
  connectNative();
});
