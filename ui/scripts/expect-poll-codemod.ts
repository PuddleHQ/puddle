// SPDX-License-Identifier: GPL-3.0-or-later
// Codemod for the e2e specs: `expect(await read)` becomes `await expect.poll(() => read)`.
//
// `expect(await read)` takes one reading of the page before it has settled, so a spec that passes on an
// idle machine fails on a loaded one. `expect.poll` reads again until the matcher holds (or the expect
// timeout runs out). The ESLint rule in eslint.config.js keeps new ones out; this tool converts the old ones.
//
//   node scripts/expect-poll-codemod.ts [--check] [--keep 'pattern=reason']... <spec file>...
//
// A site matching a `--keep` pattern (a regular expression over the whole statement) stays a one-shot, and
// gets an `// eslint-disable-next-line no-restricted-syntax -- reason` line above it. A site that already
// carries such a line is left alone, so a second run changes nothing. `--check` writes nothing and exits 1
// when a file would change. Run prettier on the files afterwards: the output is not reformatted here.
import { readFileSync, writeFileSync } from "node:fs";
import ts from "typescript";

export interface Keep {
  pattern: RegExp;
  reason: string;
}

export interface Rewritten {
  text: string;
  converted: number;
  kept: number;
}

const DISABLE = /eslint-disable-next-line no-restricted-syntax -- \S/;

interface Edit {
  start: number;
  end: number;
  text: string;
}

// The `expect(await read[, message])` calls in a file, outermost first, each with the statement it sits in.
function sites(file: ts.SourceFile) {
  const found: {
    call: ts.CallExpression;
    awaited: ts.AwaitExpression;
    statement: ts.ExpressionStatement;
  }[] = [];
  const visit = (node: ts.Node) => {
    if (
      ts.isCallExpression(node) &&
      ts.isIdentifier(node.expression) &&
      node.expression.text === "expect" &&
      node.arguments[0] !== undefined &&
      ts.isAwaitExpression(node.arguments[0])
    ) {
      // The call must start its statement (`expect(await read).toBe(x);`): the `await` goes in front of it.
      let head: ts.Node = node;
      while (
        head.parent &&
        (ts.isPropertyAccessExpression(head.parent) ||
          ts.isCallExpression(head.parent)) &&
        head.parent.expression === head
      ) {
        head = head.parent;
      }
      if (head.parent && ts.isExpressionStatement(head.parent)) {
        found.push({
          call: node,
          awaited: node.arguments[0],
          statement: head.parent,
        });
        return; // an `expect` inside this read is left for a second run
      }
    }
    ts.forEachChild(node, visit);
  };
  visit(file);
  return found;
}

export function rewrite(
  source: string,
  fileName: string,
  keeps: Keep[],
): Rewritten {
  const file = ts.createSourceFile(
    fileName,
    source,
    ts.ScriptTarget.Latest,
    true,
  );
  const lines = source.split("\n");
  const edits: Edit[] = [];
  let converted = 0;
  let kept = 0;
  for (const { call, awaited, statement } of sites(file)) {
    const awaitLine = file.getLineAndCharacterOfPosition(
      awaited.getStart(file),
    ).line;
    if (DISABLE.test(lines[awaitLine - 1] ?? "")) continue;
    const keep = keeps.find((k) => k.pattern.test(statement.getText(file)));
    if (keep) {
      const indent = /^\s*/.exec(lines[awaitLine] ?? "")?.[0] ?? "";
      const at = file.getPositionOfLineAndCharacter(awaitLine, 0);
      edits.push({
        start: at,
        end: at,
        text: `${indent}// eslint-disable-next-line no-restricted-syntax -- ${keep.reason}\n`,
      });
      kept += 1;
      continue;
    }
    const read = awaited.expression;
    const readText = read.getText(file);
    const async = /\bawait\b/.test(readText) ? "async " : "";
    const message = call.arguments[1];
    const options = message ? `, { message: ${message.getText(file)} }` : "";
    edits.push({
      start: call.getStart(file),
      end: call.getEnd(),
      text: `await expect.poll(${async}() => ${readText}${options})`,
    });
    converted += 1;
  }
  let text = source;
  for (const e of edits.sort((a, b) => b.start - a.start)) {
    text = text.slice(0, e.start) + e.text + text.slice(e.end);
  }
  return { text, converted, kept };
}

export function parseKeep(arg: string): Keep {
  const at = arg.lastIndexOf("=");
  if (at < 1 || at === arg.length - 1) {
    throw new Error(`--keep wants 'pattern=reason', got: ${arg}`);
  }
  return { pattern: new RegExp(arg.slice(0, at)), reason: arg.slice(at + 1) };
}

export function run(argv: string[]): number {
  const keeps: Keep[] = [];
  const files: string[] = [];
  let check = false;
  for (let i = 0; i < argv.length; i++) {
    const arg = argv[i] as string;
    if (arg === "--check") check = true;
    else if (arg === "--keep") keeps.push(parseKeep(argv[++i] ?? ""));
    else files.push(arg);
  }
  let changed = 0;
  for (const path of files) {
    const before = readFileSync(path, "utf8");
    const out = rewrite(before, path, keeps);
    if (out.text === before) continue;
    changed += 1;
    console.log(`${path}: ${out.converted} converted, ${out.kept} kept`);
    if (!check) writeFileSync(path, out.text);
  }
  return check && changed > 0 ? 1 : 0;
}

if (import.meta.main) process.exitCode = run(process.argv.slice(2));
