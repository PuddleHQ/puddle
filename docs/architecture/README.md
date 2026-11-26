# Puddle architecture

How Puddle is built, in the twelve sections of the [arc42](https://arc42.org/overview) template
(version 8). One file per section. Read them in order the first time; after that, jump to the
section that answers your question.

| Section                                                        | Answers                                                                              |
| -------------------------------------------------------------- | ------------------------------------------------------------------------------------ |
| [1. Introduction and goals](01-introduction-and-goals.md)      | What Puddle is for, its main requirements, its top quality goals, who cares about it |
| [2. Architecture constraints](02-architecture-constraints.md)  | What limits the design: technical, organisational and convention constraints         |
| [3. Context and scope](03-context-and-scope.md)                | Who and what Puddle talks to, and over which channels                                |
| [4. Solution strategy](04-solution-strategy.md)                | The few fundamental decisions and why                                                |
| [5. Building block view](05-building-block-view.md)            | The crates, the UI and the guest agent: what each does and depends on                |
| [6. Runtime view](06-runtime-view.md)                          | How the parts work together in the important scenarios                               |
| [7. Deployment view](07-deployment-view.md)                    | What runs where, on Windows and Linux hosts and in the VM                            |
| [8. Cross-cutting concepts](08-crosscutting-concepts.md)       | Security, identity, audit, persistence, errors, testing and other shared rules       |
| [9. Architecture decisions](09-architecture-decision-log.md)   | The significant decisions, with context, options and consequences                    |
| [10. Quality requirements](10-quality-requirements.md)         | The quality tree and the scenarios Puddle is measured against                        |
| [11. Risks and technical debt](11-risks-and-technical-debt.md) | What could go wrong and what is knowingly unfinished                                 |
| [12. Glossary](12-glossary.md)                                 | The product terms used in code, UI and these pages                                   |

## How to use these pages

- **New contributor:** read sections 1, 3, 4 and 5 for the big picture, then the glossary for the
  words the code uses.
- **Changing a component:** find it in section 5, follow its scenarios in section 6, and check the
  concepts in section 8 that it touches (secrets, audit, errors, persistence).
- **Agents:** before a change, read the section 5 entry for the crate you touch and the section 8
  concepts it names. A change that makes a page wrong updates that page in the same pull request.

## How these pages relate to the other docs

- [docs/principles.md](../principles.md) is what Puddle promises its users. These pages describe how
  the system keeps those promises.
- [docs/adr/](../adr/README.md) holds the numbered decision records. Section 9 summarises them and
  the decisions that have no record of their own.
- [docs/spec/](../spec/) holds the detailed specifications of single parts (rules, direct SSH).
  These pages link to them instead of repeating them.
- [docs/STANDARDS.md](../STANDARDS.md) holds the coding, testing and CI rules. Section 8 names them;
  STANDARDS.md stays the source.

## Rules for these pages

- Every claim names the crate, module or file it comes from, so a reader can check it. Where the code
  and a page disagree, the code is right and the page gets fixed.
- Say why in prose. These pages are public and self-contained: they cite no private planning notes.
- Use a Mermaid diagram where a picture explains more than a paragraph, not by default.
