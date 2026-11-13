// SPDX-License-Identifier: AGPL-3.0-or-later
// A minimal Chrome DevTools Protocol client for the shell's end-to-end scripts (plain Node 24, no
// packages): one page target over its websocket.

/** Opens `url` (a target's `webSocketDebuggerUrl`) and resolves with the page's handle. */
export function connect(url) {
  return new Promise((resolve, reject) => {
    const ws = new WebSocket(url);
    let next = 1;
    const pending = new Map();
    ws.onmessage = (e) => {
      const m = JSON.parse(e.data);
      const p = pending.get(m.id);
      if (p) {
        pending.delete(m.id);
        m.error ? p.reject(new Error(JSON.stringify(m.error))) : p.resolve(m.result);
      }
    };
    ws.onerror = () => reject(new Error("DevTools websocket failed"));
    ws.onopen = () => {
      const send = (method, params = {}) => {
        const id = next++;
        const done = new Promise((res, rej) => pending.set(id, { resolve: res, reject: rej }));
        ws.send(JSON.stringify({ id, method, params }));
        return done;
      };
      resolve({
        send,
        // `gesture`: run as if the user had clicked, so window.open is not blocked as a pop-up.
        evaluate: async (expression, gesture = false) => {
          const r = await send("Runtime.evaluate", { expression, returnByValue: true, awaitPromise: true, userGesture: gesture });
          if (r.exceptionDetails) throw new Error(JSON.stringify(r.exceptionDetails));
          return r.result.value;
        },
        close: () => ws.close(),
      });
    };
  });
}
