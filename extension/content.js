// SPDX-License-Identifier: Apache-2.0
// Copyright (c) 2026 sovith3

/**
 * Content script: the only object the page can reach.
 *
 * Page -> content:   window.postMessage({ channel, direction: "to-extension", message })
 * Content -> page:   window.postMessage({ channel, direction: "to-page", frame })
 *
 * Nothing is injected into the page realm, so the host stays isolated from
 * the website's own JS context while still being reachable from any origin.
 */

(() => {
  const CHANNEL = "rust-sentinel-bridge";
  let port = null;
  let reconnectTimer = null;

  function connect() {
    try {
      port = chrome.runtime.connect({ name: "sentinel-page" });
    } catch (error) {
      port = null;
      scheduleReconnect();
      return;
    }

    port.onMessage.addListener((frame) => {
      window.postMessage({ channel: CHANNEL, direction: "to-page", frame }, "*");
    });

    port.onDisconnect.addListener(() => {
      const message = chrome.runtime.lastError
        ? chrome.runtime.lastError.message
        : "extension context closed";

      window.postMessage(
        {
          channel: CHANNEL,
          direction: "to-page",
          frame: {
            id: "bridge-status",
            adapter: "core",
            event: "ERROR",
            data: { code: "EXTENSION_DISCONNECTED", message, timestamp: Date.now() }
          }
        },
        "*"
      );

      port = null;
      scheduleReconnect();
    });
  }

  function scheduleReconnect() {
    if (reconnectTimer) {
      return;
    }
    reconnectTimer = setTimeout(() => {
      reconnectTimer = null;
      connect();
    }, 2000);
  }

  window.addEventListener("message", (event) => {
    if (event.source !== window) {
      return;
    }

    const data = event.data;
    if (!data || data.channel !== CHANNEL || data.direction !== "to-extension") {
      return;
    }

    if (!port) {
      connect();
    }

    if (!port) {
      return;
    }

    const message = data.message || {};
    try {
      port.postMessage(message.request || message);
    } catch (error) {
      window.postMessage(
        {
          channel: CHANNEL,
          direction: "to-page",
          frame: {
            id: message.id || "unknown",
            adapter: message.adapter || "core",
            event: "ERROR",
            data: { code: "EXTENSION_POST_FAILED", message: String(error) }
          }
        },
        "*"
      );
    }
  });

  connect();
})();
