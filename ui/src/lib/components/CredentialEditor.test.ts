// SPDX-License-Identifier: GPL-3.0-or-later
import {
  cleanup,
  fireEvent,
  render,
  screen,
  waitFor,
} from "@testing-library/svelte";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { describeSource } from "#lib/identities/model.ts";
import { IdentitiesStore } from "#lib/stores/identities.svelte.ts";
import {
  credential,
  FakeIdentities,
  ghSource,
  profile,
} from "#lib/testing/fake-identities.ts";
import CredentialEditor from "./CredentialEditor.svelte";

let api: FakeIdentities;
let store: IdentitiesStore;
let onAdd: ReturnType<typeof vi.fn>;
let onCancel: ReturnType<typeof vi.fn>;

const FOUND = {
  accounts: [
    {
      via: "gh",
      host: "github.com",
      account: "tijs-work",
      org: null,
      signed_in: true,
    },
    {
      via: "gh",
      host: "github.com",
      account: "tijs-old",
      org: null,
      signed_in: false,
    },
    {
      via: "gcm_azure_repos",
      host: "dev.azure.com",
      account: "tijs@contoso.example",
      org: "contoso",
      signed_in: true,
    },
    {
      via: "gcm_azure_repos",
      host: "dev.azure.com",
      account: "orphan",
      org: null,
      signed_in: true,
    },
  ],
  problems: [
    { via: "gcm_github", message: "git is not installed or not on PATH" },
  ],
} as const;

beforeEach(() => {
  api = new FakeIdentities();
  api.found = structuredClone(FOUND) as never;
  store = new IdentitiesStore({ api: api as never });
  onAdd = vi.fn();
  onCancel = vi.fn();
});
afterEach(cleanup);

let onProfile: ReturnType<typeof vi.fn>;

function mount(existing = [] as never[]) {
  onProfile = vi.fn();
  render(CredentialEditor, {
    store,
    existing,
    onAdd,
    onCancel,
    onProfile,
  } as never);
}

const submit = () =>
  fireEvent.click(screen.getByRole("button", { name: "Add credential" }));
const type = (label: string | RegExp, value: string) =>
  fireEvent.input(screen.getByLabelText(label), { target: { value } });

describe("accounts found on this computer", () => {
  it("lists the usable ones, marks an expired sign-in and says what could not be looked up", async () => {
    mount();
    expect(
      await screen.findByLabelText(/tijs-work on github.com \(GitHub CLI\)/),
    ).toBeInTheDocument();
    expect(screen.getByLabelText(/tijs-old on github.com/)).toBeInTheDocument();
    expect(screen.getByText("sign-in expired")).toBeInTheDocument();
    expect(
      screen.getByLabelText(/tijs@contoso.example on dev.azure.com\/contoso/),
    ).toBeInTheDocument();
    // An Azure DevOps entry with no organisation can't name a credential.
    expect(screen.queryByLabelText(/orphan/)).toBeNull();
    expect(
      screen.getByText("git is not installed or not on PATH."),
    ).toBeInTheDocument();
  });

  it("adds the picked account with what it covers, which can be changed first", async () => {
    mount();
    await fireEvent.click(
      await screen.findByLabelText(/tijs-work on github.com/),
    );
    expect(
      screen.getByLabelText(/Owners or organisations it covers on github.com/),
    ).toHaveValue("");
    expect(screen.getByLabelText("The rest of github.com")).toBeChecked();
    await type(/Owners or organisations/, "Acme, acme-labs");
    await fireEvent.click(screen.getByLabelText("The rest of github.com"));
    await submit();
    expect(onAdd).toHaveBeenCalledWith(
      {
        host: "github.com",
        source: { kind: "gh", host: "github.com", account: "tijs-work" },
        covers: { owners: ["acme", "acme-labs"], rest_of_host: false },
      },
      null,
    );
  });

  it("fills the organisation in for an Azure DevOps account", async () => {
    mount();
    await fireEvent.click(await screen.findByLabelText(/tijs@contoso.example/));
    expect(screen.getByLabelText(/Owners or organisations/)).toHaveValue(
      "contoso",
    );
    expect(
      screen.getByLabelText(/The rest of dev.azure.com/),
    ).not.toBeChecked();
    await submit();
    expect(onAdd.mock.calls[0]?.[0]).toMatchObject({
      host: "dev.azure.com",
      source: { kind: "git_credential", path: "contoso" },
    });
  });

  it("says Puddle will decrypt the host the credential covers, for a found account and a pasted token", async () => {
    mount();
    await fireEvent.click(await screen.findByLabelText(/tijs-work/));
    expect(
      screen.getByText(
        /Puddle will decrypt connections to github.com \(port 443 only\)/,
      ),
    ).toBeTruthy();
    await fireEvent.click(screen.getByLabelText("Paste a token"));
    await type("Git host", "git.acme.example");
    expect(
      screen.getByText(
        /Puddle will decrypt connections to git.acme.example \(port 443 only\)/,
      ),
    ).toBeTruthy();
  });

  it("asks for an account first, and refuses one the identity already has", async () => {
    mount([credential({ source: ghSource("tijs-work") })] as never);
    await screen.findByLabelText(/tijs-work/);
    await submit();
    expect(screen.getByRole("alert")).toHaveTextContent("Pick an account");
    await fireEvent.click(screen.getByLabelText(/tijs-work on github.com/));
    await submit();
    expect(screen.getByRole("alert")).toHaveTextContent(
      "already on this identity",
    );
    expect(onAdd).not.toHaveBeenCalled();
  });

  it("says when nothing is found or the lookup failed", async () => {
    api.found = { accounts: [], problems: [] };
    mount();
    expect(
      await screen.findByText(/No signed-in GitHub CLI/),
    ).toBeInTheDocument();
    cleanup();
    api.down = true;
    store = new IdentitiesStore({ api: api as never });
    mount();
    expect(
      await screen.findByText(/couldn't look for signed-in accounts/),
    ).toBeInTheDocument();
  });

  it("does not look again when the accounts are known", async () => {
    await store.loadFound();
    api.calls = [];
    mount();
    await screen.findByLabelText(/tijs-work/);
    expect(api.calls).not.toContain("GET /api/credentials/found");
  });

  it("cancels", async () => {
    mount();
    await fireEvent.click(screen.getByRole("button", { name: "Cancel" }));
    expect(onCancel).toHaveBeenCalled();
  });
});

describe("a pasted token", () => {
  async function paste() {
    mount();
    await fireEvent.click(screen.getByLabelText("Paste a token"));
  }

  it("is kept at once and never comes back: the credential names it by id", async () => {
    await paste();
    expect(screen.getByLabelText("Token")).toHaveAttribute("type", "password");
    await type("Token", "  ghp_secret  ");
    await submit();
    await waitFor(() => expect(onAdd).toHaveBeenCalled());
    const [added, stored] = onAdd.mock.calls[0]!;
    expect(added).toMatchObject({
      host: "github.com",
      source: { kind: "stored", host: "github.com", org: null },
      covers: { owners: [], rest_of_host: true },
    });
    expect(stored).toBe(added.source);
    expect(JSON.stringify(added)).not.toContain("ghp_secret");
    expect(screen.getByLabelText("Token")).toHaveValue("");
    expect(api.tokens).toHaveLength(1);
  });

  it("belongs to one organisation on Azure DevOps", async () => {
    await paste();
    await type("Git host", "dev.azure.com");
    expect(
      screen.getByLabelText("Azure DevOps organisation"),
    ).toBeInTheDocument();
    await type("Token", "abc");
    await submit();
    expect(screen.getByRole("alert")).toHaveTextContent("organisation");
    await type("Azure DevOps organisation", "contoso");
    await type(/Owners or organisations/, "contoso");
    await fireEvent.click(screen.getByLabelText(/The rest of dev.azure.com/));
    await submit();
    await waitFor(() => expect(onAdd).toHaveBeenCalled());
    expect(onAdd.mock.calls[0]?.[0].source).toMatchObject({
      kind: "stored",
      org: "contoso",
    });
  });

  it("checks the host, the token and what is covered before it asks", async () => {
    await paste();
    await type("Git host", "nodots");
    await submit();
    expect(screen.getByRole("alert")).toHaveTextContent("not a host name");
    await type("Git host", "github.com");
    await submit();
    expect(screen.getByRole("alert")).toHaveTextContent("Paste the token");
    await type("Token", "two words");
    await submit();
    expect(screen.getByRole("alert")).toHaveTextContent("no spaces");
    await type("Token", "fine");
    await type(/Owners or organisations/, "bad/owner");
    await submit();
    expect(screen.getByRole("alert")).toHaveTextContent("bad/owner");
    await type(/Owners or organisations/, "");
    await fireEvent.click(screen.getByLabelText("The rest of github.com"));
    await submit();
    expect(screen.getByRole("alert")).toHaveTextContent("Name an owner");
    expect(api.tokens).toHaveLength(0);
    expect(onAdd).not.toHaveBeenCalled();
  });

  it("shows the host's refusal at the token field and keeps nothing", async () => {
    await paste();
    await type("Token", "fine");
    api.refuse = {
      status: 503,
      message: "the operating system's credential store is not available",
    };
    await submit();
    expect(await screen.findByRole("alert")).toHaveTextContent(
      "credential store is not available",
    );
    expect(onAdd).not.toHaveBeenCalled();
  });

  it("can go back to the accounts found", async () => {
    await paste();
    await fireEvent.click(screen.getByLabelText("Found on this computer"));
    expect(screen.queryByLabelText("Token")).toBeNull();
    await fireEvent.click(await screen.findByLabelText(/tijs-work/));
    await fireEvent.click(screen.getByLabelText("Paste a token"));
    await fireEvent.click(screen.getByLabelText("Found on this computer"));
    expect(screen.getByLabelText(/Owners or organisations/)).toHaveValue("");
  });
});

describe("the account behind a credential", () => {
  const work = describeSource(ghSource("tijs-work"));
  const mine = profile({
    account: "tijs-work",
    name: "Tijs Work",
    email: "1+tijs-work@users.noreply.github.com",
    organisations: ["acme", "acme-labs"],
    notes: [{ code: "sso_partial", message: "<b>acme-sso</b> is missing." }],
  });

  it("hands the name and address over and offers the organisations as chips that fill the coverage", async () => {
    api.profiles[work] = mine;
    mount();
    await fireEvent.click(
      await screen.findByLabelText(/tijs-work on github.com/),
    );
    const acme = await screen.findByRole("button", { name: "acme" });
    expect(onProfile).toHaveBeenCalledWith(mine);
    expect(acme).toHaveAttribute("aria-pressed", "false");
    await fireEvent.click(acme);
    expect(acme).toHaveAttribute("aria-pressed", "true");
    expect(screen.getByLabelText(/Owners or organisations/)).toHaveValue(
      "acme",
    );
    await fireEvent.click(screen.getByRole("button", { name: "acme-labs" }));
    expect(screen.getByLabelText(/Owners or organisations/)).toHaveValue(
      "acme, acme-labs",
    );
    await fireEvent.click(acme);
    expect(screen.getByLabelText(/Owners or organisations/)).toHaveValue(
      "acme-labs",
    );
    // The host's words are text, never markup.
    expect(screen.getByText("<b>acme-sso</b> is missing.")).toBeInTheDocument();
  });

  it("shows the host's problem as text and offers nothing", async () => {
    api.profiles[work] = profile({
      problem: {
        code: "token_rejected",
        message: "GitHub did not accept the login <i>x</i>.",
        needs_sign_in: true,
      },
    });
    mount();
    await fireEvent.click(await screen.findByLabelText(/tijs-work/));
    expect(
      await screen.findByText("GitHub did not accept the login <i>x</i>."),
    ).toBeInTheDocument();
    expect(onProfile).not.toHaveBeenCalled();
    await submit();
    expect(onAdd).toHaveBeenCalled();
  });

  it("says an Azure DevOps author has to be typed and offers its organisation", async () => {
    api.profiles[
      describeSource({
        kind: "git_credential",
        host: "dev.azure.com",
        path: "contoso",
        username: null,
      })
    ] = profile({ organisations: ["contoso"] });
    mount();
    await fireEvent.click(await screen.findByLabelText(/tijs@contoso.example/));
    expect(
      await screen.findByText(/type the author name and address yourself/),
    ).toBeInTheDocument();
    expect(onProfile).not.toHaveBeenCalled();
    expect(screen.getByRole("button", { name: "contoso" })).toHaveAttribute(
      "aria-pressed",
      "true",
    );
  });

  it("never holds up adding the credential while the host is slow, and drops a late answer", async () => {
    let release = () => {};
    api.profileGate = new Promise<void>((resolve) => (release = resolve));
    api.profiles[work] = mine;
    mount();
    await fireEvent.click(await screen.findByLabelText(/tijs-work/));
    expect(
      await screen.findByText(/Asking github.com who/),
    ).toBeInTheDocument();
    await submit();
    expect(onAdd).toHaveBeenCalledTimes(1);
    // Going back to the pasted-token form forgets the lookup in flight.
    await fireEvent.click(screen.getByLabelText("Paste a token"));
    release();
    await waitFor(() =>
      expect(api.calls).toContain("POST /api/credentials/profile"),
    );
    expect(onProfile).not.toHaveBeenCalled();
    expect(screen.queryByRole("button", { name: "acme" })).toBeNull();
  });

  it("tells a failed lookup in words and still adds the credential", async () => {
    mount();
    await fireEvent.click(await screen.findByLabelText(/tijs-work/));
    await waitFor(() =>
      expect(api.calls).toContain("POST /api/credentials/profile"),
    );
    api.refuse = { status: 503, message: "the service is not available" };
    await fireEvent.click(await screen.findByLabelText(/tijs-old/));
    expect(
      await screen.findByText(/The service is not available/),
    ).toBeInTheDocument();
    expect(onProfile).not.toHaveBeenCalled();
  });

  it("looks up a pasted token by keeping it, and Add credential reuses that entry", async () => {
    api.profiles[
      describeSource({
        kind: "stored",
        id: "tok-fake-100",
        host: "github.com",
        org: null,
      })
    ] = mine;
    mount();
    await fireEvent.click(screen.getByLabelText("Paste a token"));
    await type("Token", "ghp_secret");
    await fireEvent.click(
      screen.getByRole("button", { name: "Look up this account" }),
    );
    expect(
      await screen.findByRole("button", { name: "acme" }),
    ).toBeInTheDocument();
    expect(onProfile).toHaveBeenCalledWith(mine);
    await submit();
    expect(api.tokens).toHaveLength(1);
    expect(onAdd.mock.calls[0]?.[1]).toMatchObject({ kind: "stored" });
    expect(screen.getByLabelText("Token")).toHaveValue("");
  });

  it("checks the pasted form before it keeps anything, and replaces a kept token the user changed", async () => {
    mount();
    await fireEvent.click(screen.getByLabelText("Paste a token"));
    const look = () =>
      fireEvent.click(
        screen.getByRole("button", { name: "Look up this account" }),
      );
    await look();
    expect(await screen.findByRole("alert")).toHaveTextContent(/token/i);
    await type("Git host", "");
    await look();
    expect(screen.getByRole("alert")).toHaveTextContent(/host/i);
    await type("Git host", "dev.azure.com");
    await look();
    expect(screen.getByRole("alert")).toHaveTextContent(/organisation/i);
    await type("Azure DevOps organisation", "contoso");
    await type("Token", "first");
    await look();
    await waitFor(() => expect(api.tokens).toHaveLength(1));
    await type("Token", "second");
    await look();
    await waitFor(() =>
      expect(
        api.calls.filter((c) => c === "POST /api/credentials/stored"),
      ).toHaveLength(2),
    );
    await waitFor(() => expect(api.tokens).toHaveLength(1));
    // A refusal to keep the token shows at the token field.
    await type("Token", "third");
    api.refuse = {
      status: 503,
      message: "the credential store is not available",
    };
    await look();
    expect(await screen.findByRole("alert")).toHaveTextContent(
      /credential store/,
    );
  });

  it("removes a kept token again when the editor goes away without adding it", async () => {
    mount();
    await fireEvent.click(screen.getByLabelText("Paste a token"));
    await type("Token", "ghp_secret");
    await fireEvent.click(
      screen.getByRole("button", { name: "Look up this account" }),
    );
    await waitFor(() => expect(api.tokens).toHaveLength(1));
    cleanup();
    await waitFor(() => expect(api.tokens).toHaveLength(0));
  });
});
