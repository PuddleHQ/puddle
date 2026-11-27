// SPDX-License-Identifier: AGPL-3.0-or-later
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
  /** What this means for the person, behind a closed toggle. Absent where there is nothing to add. */
  impact?: string;
  /** What to do about it, behind a closed toggle. Absent where there is nothing to do. */
  remedy?: string;
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

/** What the step is for, in one place the page shows above the lines. */
export const PURPOSE =
  "Some apps in your workspaces serve a web page over HTTPS, and Puddle forwards that page to your own browser at https://localhost. Puddle makes one certificate for localhost so your browser opens those pages without a warning. Nothing else needs it.";

/** The expert text behind the page's "More details" toggle: why, what is trusted, how to undo it, what skipping costs. */
export const MORE_DETAILS: readonly string[] = [
  "A web app you run in a workspace (for example a Vite or ASP.NET server using HTTPS) listens inside the workspace. Puddle forwards its port to your computer, so you open it as https://localhost:PORT in your own browser.",
  "A browser only trusts an HTTPS address when a certificate it knows vouches for it. Without one it shows a 'not secure' page, and features that need a secure address, such as secure cookies, sign-in redirects and service workers, can fail.",
  "Trusting means Windows asks you once. It shows a Security Warning that names the certificate: choose Yes. The question is about this one certificate, made on this computer for localhost only, and it applies to your Windows user only. No administrator rights are needed, and Puddle changes nothing else.",
  "To remove it later, open 'Manage user certificates' in Windows, go to Trusted Root Certification Authorities, then Certificates, and delete the one for localhost that Puddle made. This screen then shows it as not trusted again.",
  "If you skip, everything in Puddle keeps working. Only your browser warns on https://localhost pages that a workspace serves, until you trust the certificate. Puddle asks only on this screen. The same certificate is also placed inside each workspace, so tools there such as curl, git and node trust it either way.",
];

/** The line about the development certificate: what Puddle uses and whether the browser trusts it. */
export function devCertificateLine(status: DevCertificate | undefined): Line {
  switch (status) {
    case "trusted":
      return {
        tone: "ok",
        title: "Development certificate trusted",
        text: "Windows trusts Puddle's localhost certificate. If Firefox was open when you trusted it, restart Firefox.",
        impact:
          "Your browser opens apps your workspaces serve over HTTPS without a warning.",
      };
    case "needs_trust":
      return {
        tone: "info",
        title: "Development certificate: not trusted yet",
        text: "Your browser doesn't trust Puddle's localhost certificate yet.",
        impact:
          "A workspace app opened at https://localhost shows a 'not secure' warning, and some features that need a secure address may not work. Nothing else is affected.",
        remedy:
          "Press Trust it now. Windows asks once, in a Security Warning that names the certificate: choose Yes, which trusts only this certificate, for your user. Or skip.",
      };
    case "trust_declined":
      return {
        tone: "warn",
        title: "Development certificate: not trusted",
        text: "You answered No in Windows, so your browser still warns on workspace apps at https://localhost.",
        impact:
          "Apps in your workspaces still work. Only your browser shows the warning.",
        remedy: "Press Trust it now to be asked again, or skip.",
      };
    case "trust_failed":
      return {
        tone: "warn",
        title: "Development certificate: couldn't be trusted",
        text: "Windows didn't let Puddle trust the certificate.",
        impact:
          "Your browser warns on workspace apps at https://localhost. Everything else works.",
        remedy:
          "A company policy that forbids adding certificates is one reason, and Puddle never overrides a policy. Ask your administrator, or click through the browser warning.",
      };
    case "unsupported":
      return {
        tone: "info",
        title: "Development certificate: not trusted on this system yet",
        text: "Puddle can't add its localhost certificate to your browser's trust on this system yet.",
        impact:
          "Your browser warns on workspace apps at https://localhost. Everything else works.",
      };
    default:
      return {
        tone: "skipped",
        title: "Development certificate: not checked yet",
        text: "Puddle hasn't looked at whether your browser trusts its localhost certificate.",
        impact:
          "Until it is trusted, your browser warns on workspace apps at https://localhost.",
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
  const names = [
    ...new Set(
      roots.certificates
        .filter((c) => c.kind === "root" && c.subject)
        .map((c) => c.subject as string),
    ),
  ];
  const shown = names.slice(0, 3);
  const more = roots.roots - shown.length;
  const includes =
    shown.length === 0
      ? ""
      : ` Includes ${shown.join(", ")}${more > 0 ? ` and ${more} more` : ""}.`;
  return {
    tone: "ok",
    title: `${roots.roots} root ${plural(roots.roots, "certificate", "certificates")} copied from this computer`,
    text: `From this computer's trust store, kept for company networks that inspect HTTPS, so tools in a workspace trust them.${includes}`,
    impact:
      "On a home computer these are usually added by Windows or by software you installed, not by a company. Puddle only copies them into your workspaces; nothing is sent anywhere.",
  };
}
