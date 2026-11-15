// SPDX-License-Identifier: AGPL-3.0-or-later
// One terminal: the ticket, the socket, the flow control and the size rules, between an emulator (`Screen`) and the host. The emulator and the socket are handed in, so
// every rule here is tested without a browser. No byte of the terminal is logged or stored: the
// scrollback lives in the emulator, in the page.
import { api as defaultApi, type ApiClient } from "#lib/api/client.ts";
import { sentence } from "#lib/rules/model.ts";
import { SERVICE_DOWN } from "#lib/stores/read-failure.ts";
import { cleanTitle, endLine, BROKE, type EndLine } from "./text.ts";
import type { Screen } from "./xterm-screen.ts";
import {
  ackMessage,
  clampSize,
  parseEnded,
  resizeMessage,
  socketUrl,
  splitInput,
  SUBPROTOCOL,
  type Size,
} from "./wire.ts";

/** The part of `WebSocket` the session uses. */
export interface SocketLike {
  binaryType: string;
  onopen: (() => void) | null;
  onmessage: ((event: { data: unknown }) => void) | null;
  onclose: (() => void) | null;
  onerror: (() => void) | null;
  send(data: string | Uint8Array): void;
  close(): void;
}

export interface SessionDeps {
  api: Pick<ApiClient, "POST">;
  socket: (url: string, protocols: string[]) => SocketLike;
  origin: () => string;
}

export type SessionState = "idle" | "connecting" | "live" | "ended";

/** How long the size must stay put before it goes to the host. */
export const RESIZE_DELAY_MS = 100;
/** Acknowledge drawn bytes once this many are waiting, or after the delay, whichever comes first. */
const ACK_BATCH = 16 * 1024;
const ACK_DELAY_MS = 20;

export class TerminalSession {
  readonly workspace: string;
  /** The number in "Terminal N", from 1, never reused in a workspace while the page lives. */
  readonly number: number;
  state = $state<SessionState>("idle");
  /** The line under an ended terminal. */
  end = $state.raw<EndLine | null>(null);
  /** A program's window title, cleaned; empty when none set one. */
  title = $state("");
  /** The bell rang while this sub-tab was not the one in view. */
  bell = $state(false);
  /** Some output has arrived; until then a workspace that is still starting is the reason for the quiet. */
  output = $state(false);
  /** The host refused to open it (a limit, a stopped workspace): no session ever ran. */
  refused = $state(false);
  readonly screen: Screen;
  readonly #deps: SessionDeps;
  #socket: SocketLike | null = null;
  #size: Size = { cols: 80, rows: 24 };
  #sentSize: Size = { cols: 80, rows: 24 };
  #resizeTimer: ReturnType<typeof setTimeout> | null = null;
  #ackTimer: ReturnType<typeof setTimeout> | null = null;
  #drawn = 0;
  #queued: Uint8Array[] = [];
  #run = 0;
  #closed = false;
  readonly #encoder = new TextEncoder();

  constructor(
    workspace: string,
    number: number,
    screen: Screen,
    deps: Partial<SessionDeps> = {},
  ) {
    this.workspace = workspace;
    this.number = number;
    this.screen = screen;
    this.#deps = {
      api: deps.api ?? defaultApi,
      socket:
        deps.socket ??
        ((url, protocols) => new WebSocket(url, protocols) as SocketLike),
      origin: deps.origin ?? (() => globalThis.location.origin),
    };
    screen.onInput((text) => this.#send(this.#encoder.encode(text)));
    screen.onBinary((bytes) => this.#send(bytes));
    screen.onResize((size) => this.#resized(size));
    screen.onTitle((title) => {
      this.title = cleanTitle(title);
    });
    screen.onBell(() => {
      this.bell = true;
    });
  }

  /** What the sub-tab says: the program's title, else "Terminal N". */
  get label(): string {
    return this.title === "" ? `Terminal ${this.number}` : this.title;
  }

  /** The terminal is open or about to open: it counts against the limits. */
  get open(): boolean {
    return this.state !== "ended";
  }

  /** Asks for a terminal of `size`, then opens its socket with the ticket. */
  async connect(size: Size = this.#size): Promise<void> {
    const run = ++this.#run;
    this.#size = clampSize(size.cols, size.rows);
    this.#sentSize = this.#size;
    this.state = "connecting";
    this.end = null;
    this.refused = false;
    this.output = false;
    let ticket: { terminal: number; ticket: string } | undefined;
    let why = SERVICE_DOWN;
    try {
      const { data, error } = await this.#deps.api.POST(
        "/api/workspaces/{id}/terminals",
        {
          params: { path: { id: this.workspace } },
          body: { cols: this.#size.cols, rows: this.#size.rows },
        },
      );
      ticket = data;
      if (!data && error) why = sentence(error.message);
    } catch {
      ticket = undefined;
    }
    if (run !== this.#run || this.#closed) return;
    if (!ticket) {
      this.refused = true;
      this.#finish({ line: why, detail: null, action: "new" });
      return;
    }
    const socket = this.#deps.socket(
      socketUrl(this.#deps.origin(), this.workspace, ticket.terminal),
      [SUBPROTOCOL, `ticket.${ticket.ticket}`],
    );
    socket.binaryType = "arraybuffer";
    this.#socket = socket;
    socket.onopen = () => {
      if (run !== this.#run) return;
      this.state = "live";
      this.#flush(socket);
    };
    socket.onmessage = (event) => {
      if (run === this.#run) this.#received(event.data);
    };
    const broke = () => {
      if (run === this.#run && this.state !== "ended") this.#finish(BROKE);
    };
    socket.onclose = broke;
    socket.onerror = broke;
  }

  /** A new session in this sub-tab under a separator; the scrollback above stays. */
  async restart(): Promise<void> {
    this.screen.separator();
    this.screen.enable();
    await this.connect();
  }

  /** Closing the sub-tab: the host ends the session and what it started. */
  close(): void {
    this.#closed = true;
    this.#run += 1;
    this.#timers();
    this.#socket?.close();
    this.#socket = null;
    this.state = "ended";
    this.screen.dispose();
  }

  #finish(line: EndLine): void {
    this.state = "ended";
    this.end = line;
    this.#timers();
    this.screen.disable();
    const socket = this.#socket;
    this.#socket = null;
    this.#queued = [];
    socket?.close();
  }

  #timers(): void {
    if (this.#resizeTimer !== null) clearTimeout(this.#resizeTimer);
    if (this.#ackTimer !== null) clearTimeout(this.#ackTimer);
    this.#resizeTimer = null;
    this.#ackTimer = null;
  }

  #received(data: unknown): void {
    if (typeof data === "string") {
      const ended = parseEnded(data);
      if (ended) this.#finish(endLine(ended));
      return;
    }
    if (!(data instanceof ArrayBuffer)) return;
    const bytes = new Uint8Array(data);
    this.output = true;
    this.screen.write(bytes, () => this.#acknowledge(bytes.length));
  }

  #acknowledge(bytes: number): void {
    this.#drawn += bytes;
    if (this.#drawn >= ACK_BATCH) {
      this.#sendAck();
    } else if (this.#ackTimer === null) {
      this.#ackTimer = setTimeout(() => this.#sendAck(), ACK_DELAY_MS);
    }
  }

  #sendAck(): void {
    if (this.#ackTimer !== null) clearTimeout(this.#ackTimer);
    this.#ackTimer = null;
    const bytes = this.#drawn;
    this.#drawn = 0;
    if (bytes > 0 && this.state === "live") {
      this.#socket?.send(ackMessage(bytes));
    }
  }

  #send(bytes: Uint8Array): void {
    if (this.state === "connecting") {
      this.#queued.push(bytes);
    } else if (this.state === "live" && this.#socket) {
      for (const piece of splitInput(bytes)) this.#socket.send(piece);
    }
  }

  #flush(socket: SocketLike): void {
    for (const bytes of this.#queued.splice(0)) {
      for (const piece of splitInput(bytes)) socket.send(piece);
    }
    this.#sendSize();
  }

  #resized(size: Size): void {
    this.#size = clampSize(size.cols, size.rows);
    if (this.#resizeTimer !== null) clearTimeout(this.#resizeTimer);
    this.#resizeTimer = setTimeout(() => {
      this.#resizeTimer = null;
      this.#sendSize();
    }, RESIZE_DELAY_MS);
  }

  #sendSize(): void {
    const size = this.#size;
    if (
      this.state !== "live" ||
      (size.cols === this.#sentSize.cols && size.rows === this.#sentSize.rows)
    ) {
      return;
    }
    this.#sentSize = size;
    this.#socket?.send(resizeMessage(size));
  }
}
