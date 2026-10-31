// SPDX-License-Identifier: GPL-3.0-or-later
// What the certificates step says: the development certificate (what Puddle knows about it) and
// the company roots workspaces get. Words only; the page decides the layout.
import type { components } from "#lib/api/schema.d.ts";
import type { NetworkHealth } from "#lib/network/model.ts";
import type { DoctorStatus } from "./doctor.ts";

export type DevCertificate = components["schemas"]["DevCertificate"];

export interface Line {
  /** Drawn like a check of the system check: `ok`, `info`, `warn`, or `skipped` for "not looked at". */
  tone: Extract<DoctorStatus, "ok" | "info" | "warn" | "skipped">;
  title: string;
  text: string;
}

/** Whether the step offers the Trust button: Windows can be asked, and the user has not said yes. */
export function canTrust(status: DevCertificate | undefined): boolean {
  return status === "needs_trust" || status === "trust_declined";
}

/** What the last press of "Trust it now" left to say, for the status line under the button. */
export function trustNote(press: {
  trusting: boolean;
  error: string | null;
  result: { waiting: boolean; detail: string | null } | null;
  status: DevCertificate | undefined;
}): string {
  if (press.trusting) {
    return "Waiting for your answer in Windows' Security Warning. It can open behind this window: look for it in the taskbar.";
  }
  if (press.error !== null) return press.error;
  if (press.result === null) return "";
  const parts = [`${devCertificateLine(press.status).title}.`];
  if (press.result.waiting) {
    parts.push(
      "Windows is still waiting for your answer to the Security Warning, which can be behind this window. Pressing again does not open another.",
    );
  }
  if (press.result.detail !== null)
    parts.push(`Details: ${press.result.detail}`);
  return parts.join(" ");
}

/** The line about the development certificate: what Puddle uses and whether the browser trusts it. */
export function devCertificateLine(status: DevCertificate | undefined): Line {
  switch (status) {
    case "reusing_existing":
      return {
        tone: "ok",
        title: "Using your existing .NET development certificate",
        text: "Windows already trusts it, so HTTPS on localhost works in your browser for apps in every workspace. No trust dialog needed.",
      };
    case "trusted":
      return {
        tone: "ok",
        title: "Development certificate trusted",
        text: "Puddle made a certificate for localhost and Windows trusts it, so HTTPS on localhost works in your browser for apps in every workspace. If Firefox was open when you trusted it, restart Firefox.",
      };
    case "needs_trust":
      return {
        tone: "info",
        title: "Development certificate: not trusted yet",
        text: "Puddle hasn't asked Windows to trust the certificate it makes for localhost. Until you do, an HTTPS development server shows a browser warning. Press Trust it now and Windows shows one Security Warning that names the certificate: choose Yes to trust it.",
      };
    case "trust_declined":
      return {
        tone: "warn",
        title: "Development certificate: not trusted",
        text: "You declined Windows' confirmation. Apps in your workspaces still get the certificate, but your browser warns about HTTPS on localhost until you trust it. Press Trust it now to be asked again.",
      };
    case "trust_failed":
      return {
        tone: "warn",
        title: "Development certificate: couldn't be trusted",
        text: "Windows didn't let Puddle trust it. A company policy that forbids adding certificates is one reason, and Puddle never overrides a policy. An HTTPS development server shows a browser warning.",
      };
    case "unsupported":
      return {
        tone: "info",
        title: "Development certificate: not trusted on this system yet",
        text: "Apps in your workspaces get a certificate for localhost, but Puddle can't add it to your browser's trust yet, so an HTTPS development server shows a browser warning.",
      };
    default:
      return {
        tone: "skipped",
        title: "Development certificate: not checked yet",
        text: "An HTTPS development server in a workspace shows a browser warning until Puddle can trust a certificate for it.",
      };
  }
}

const plural = (n: number, one: string, many: string): string =>
  n === 1 ? one : many;

/** The line about the company roots, from the network-health report; `null` while it is unread. */
export function rootsLine(
  report: Pick<NetworkHealth, "roots"> | null,
  failed: boolean,
): Line {
  if (report === null) {
    return failed
      ? {
          tone: "skipped",
          title: "Company certificates: not read",
          text: "Puddle couldn't read its certificate report. Workspaces still get the standard certificates.",
        }
      : {
          tone: "skipped",
          title: "Company certificates",
          text: "Puddle is reading this computer's certificate stores…",
        };
  }
  const { roots } = report;
  if (!roots.synced) {
    return {
      tone: "skipped",
      title: "Company certificates: not read yet",
      text: "Puddle has not read this computer's certificate stores yet.",
    };
  }
  if (roots.unreadable_stores.length > 0) {
    const n = roots.unreadable_stores.length;
    return {
      tone: "warn",
      title: `${n} certificate ${plural(n, "store", "stores")} couldn't be read`,
      text: "Tools in a workspace may fail on sites your company network inspects. The details are in Network health.",
    };
  }
  if (roots.roots === 0) {
    return {
      tone: "info",
      title: "No company root certificates found",
      text: "Workspaces use the standard set of certificates.",
    };
  }
  return {
    tone: "ok",
    title: `${roots.roots} company root ${plural(roots.roots, "certificate", "certificates")} found`,
    text: "Added to every workspace, so tools trust your network's TLS inspection.",
  };
}
