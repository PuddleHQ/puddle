#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-or-later
"""Cuts the part of GitHub's REST description that Puddle's readers use out of the pinned full file.

The full file (github/rest-api-description, `api.github.com.2022-11-28.json`, 14 MB) is pinned by commit
and SHA-256 in ci/github-rest/pin.txt. Tests cannot download it, so the operations Puddle reads are
cut out of it into crates/puddle-repos/tests/data/github-rest-slice.json: for each operation its
parameters, its 200 response (body schema and header names), and every component those reach through
`$ref`. Nothing is edited, so a schema in the slice is GitHub's schema.

  slice.py write  <full.json> <slice.json>    cut the slice
  slice.py check  <full.json> <slice.json>    exit 1 unless <slice.json> is exactly what `write` would cut
  slice.py self-test

Standard library only: it runs on any runner and in the host checks.
"""
import json
import sys

# The operations the readers of crates/puddle-repos/src/github.rs call.
OPERATIONS = [("/user", "get"), ("/user/repos", "get"), ("/user/orgs", "get")]


def refs(node, found):
    """Collects every local `$ref` under `node` into `found`."""
    if isinstance(node, dict):
        for key, value in node.items():
            if key == "$ref" and isinstance(value, str) and value.startswith("#/"):
                found.add(value)
            else:
                refs(value, found)
    elif isinstance(node, list):
        for item in node:
            refs(item, found)


def resolve(full, ref):
    node = full
    for part in ref[2:].split("/"):
        node = node[part]
    return node


def cut(full):
    """The slice of `full`: the operations, and the components they reach."""
    out = {"openapi": full["openapi"], "info": {"version": full["info"]["version"]}, "paths": {}}
    wanted = set()
    for path, method in OPERATIONS:
        op = full["paths"][path][method]
        response = op["responses"]["200"]
        kept = {
            "parameters": op.get("parameters", []),
            "responses": {"200": {k: response[k] for k in ("content", "headers") if k in response}},
        }
        out["paths"].setdefault(path, {})[method] = kept
        refs(kept, wanted)
    done = set()
    components = {}
    while wanted - done:
        ref = sorted(wanted - done)[0]
        done.add(ref)
        node = resolve(full, ref)
        _, _, kind, name = ref.split("/")
        components.setdefault(kind, {})[name] = node
        refs(node, wanted)
    out["components"] = components
    return out


def dump(doc):
    return json.dumps(doc, sort_keys=True, separators=(",", ":")) + "\n"


def self_test():
    full = {
        "openapi": "3.0.3",
        "info": {"version": "1"},
        "paths": {
            p: {
                "get": {
                    "parameters": [{"$ref": "#/components/parameters/page"}],
                    "responses": {
                        "200": {
                            "description": "ok",
                            "headers": {"link": {"$ref": "#/components/headers/link"}},
                            "content": {"application/json": {"schema": {"$ref": "#/components/schemas/a"}}},
                        },
                        "401": {"description": "dropped"},
                    },
                }
            }
            for p, _ in OPERATIONS
        },
        "components": {
            "schemas": {
                "a": {"type": "object", "properties": {"b": {"$ref": "#/components/schemas/b"}}},
                "b": {"type": "string"},
                "unused": {"type": "string"},
            },
            "parameters": {"page": {"name": "page", "in": "query"}},
            "headers": {"link": {"schema": {"type": "string"}}},
        },
    }
    got = cut(full)
    assert set(got["components"]["schemas"]) == {"a", "b"}, got["components"]
    assert "401" not in got["paths"]["/user"]["get"]["responses"]
    assert set(got["components"]) == {"schemas", "parameters", "headers"}
    assert dump(got) == dump(cut(full))
    print("slice.py self-test passed")


def main(argv):
    if argv[1:] == ["self-test"]:
        self_test()
        return 0
    if len(argv) != 4 or argv[1] not in ("write", "check"):
        print(__doc__, file=sys.stderr)
        return 2
    with open(argv[2], encoding="utf-8") as handle:
        text = dump(cut(json.load(handle)))
    if argv[1] == "write":
        with open(argv[3], "w", encoding="utf-8", newline="\n") as handle:
            handle.write(text)
        return 0
    with open(argv[3], encoding="utf-8") as handle:
        if handle.read() != text:
            print("the checked-in slice is not what the pinned description gives: run ci/github-rest/slice.py write", file=sys.stderr)
            return 1
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
