// SPDX-License-Identifier: AGPL-3.0-or-later
// Runs before the main window's own scripts. Locks `Object.prototype` against pollution while still
// letting code give a plain object its own `toString`, `valueOf` or `constructor`.
//
// A plain `Object.freeze(Object.prototype)` makes every built-in on it read-only, and JavaScript then
// refuses an assignment such as `keys.toString = f` on any ordinary object too, because the
// inherited property is read-only (the "override mistake"). The terminal emulator does exactly that
// when it loads, so a frozen prototype broke the Terminal tab. Here each data property of the
// prototype becomes a getter and setter first: reading gives the built-in, and assigning defines the
// property on the object assigned to. Assigning on the prototype itself still throws, and the
// freeze still refuses new or changed properties on it.
(function () {
  "use strict";
  const proto = Object.prototype;
  for (const key of Reflect.ownKeys(proto)) {
    const found = Object.getOwnPropertyDescriptor(proto, key);
    if (found === undefined || !("value" in found) || !found.configurable) {
      continue;
    }
    const value = found.value;
    Object.defineProperty(proto, key, {
      get() {
        return value;
      },
      set(next) {
        if (this === proto) {
          throw new TypeError(
            `Object.prototype is locked: ${String(key)} can't be changed`,
          );
        }
        Object.defineProperty(this, key, {
          value: next,
          writable: true,
          enumerable: true,
          configurable: true,
        });
      },
      enumerable: found.enumerable,
      configurable: false,
    });
  }
  Object.freeze(proto);
})();
