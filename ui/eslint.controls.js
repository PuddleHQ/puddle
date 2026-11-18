// SPDX-License-Identifier: AGPL-3.0-or-later

// A component never styles a bare form control. Fields, dropdowns, text boxes, ticks, radios and
// buttons take their look from src/lib/theme/controls.css (and base.css), so a screen that writes
// `input { ... }`, `.row select { ... }` or `:global(.x textarea)` is a second copy that drifts
// (about 30 files had their own, and the ones that had none showed the browser's raw grey control). Layout of a control (a width, a flex share) goes on a class
// in the component or a shared utility class from controls.css, never on the element name.
// ui/scripts/controls-lint.test.ts proves the rule fires on the old code and passes the fix.
const CONTROLS = ["input", "select", "textarea", "button"];

/**
 * Selectors in a stylesheet whose last compound is a bare control tag.
 * @param {string} css
 */
export function bareControlSelectors(css) {
  /** @type {{ selector: string, line: number }[]} */
  const found = [];
  const stripped = css.replace(/\/\*[\s\S]*?\*\//g, (/** @type {string} */ m) =>
    m.replace(/[^\n]/g, " "),
  );
  for (const rule of stripped.matchAll(/([^{}]+)\{/g)) {
    const prelude = (rule[1] ?? "").trim();
    if (prelude.startsWith("@")) continue;
    for (const selector of prelude.split(",")) {
      const unwrapped = selector.replace(/:global\(([^)]*)\)/g, "$1").trim();
      const last = unwrapped.split(/[\s>+~]+/).pop() ?? "";
      const tag = /^([a-z]+)(?![\w-])/.exec(last)?.[1];
      if (tag && CONTROLS.includes(tag)) {
        const lead =
          (rule[1] ?? "").length - (rule[1] ?? "").trimStart().length;
        const before = stripped.slice(0, rule.index + lead);
        found.push({
          selector: selector.trim(),
          line: before.split("\n").length,
        });
      }
    }
  }
  return found;
}

/** @type {import("eslint").Rule.RuleModule} */
const rule = {
  meta: {
    type: "problem",
    schema: [],
    messages: {
      bare: "`{{selector}}` styles a bare {{tag}}. Controls are styled once in src/lib/theme/controls.css; give this element a class for layout (for example `.input-fill`, `.input-wide`, or your own class) and select the class.",
    },
  },
  create(context) {
    return {
      /** @param {any} node */
      SvelteStyleElement(node) {
        const css = node.children
          .map((/** @type {{ value?: string }} */ c) => c.value ?? "")
          .join("");
        const startLine = node.loc.start.line;
        for (const { selector, line } of bareControlSelectors(css)) {
          const tag = CONTROLS.find((t) =>
            new RegExp(`(^|[\\s>+~(])${t}(?![\\w-])`).test(selector),
          );
          context.report({
            loc: { line: startLine + line - 1, column: 0 },
            messageId: "bare",
            data: { selector, tag: tag ?? "control" },
          });
        }
      },
    };
  },
};

/** @type {import("eslint").Linter.Config} */
export const noBareControls = {
  files: ["**/*.svelte"],
  plugins: { puddle: { rules: { "no-bare-controls": rule } } },
  rules: { "puddle/no-bare-controls": "error" },
};
