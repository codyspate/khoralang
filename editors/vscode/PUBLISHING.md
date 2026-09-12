# Publishing the extension to the registries

The workflow builds the `.vsix`, checks it, and publishes it as a release asset.
Three further steps put it where people actually look. Each is skipped until
its credential exists, so a release works exactly as it does today until one is
configured.

**Do Open VSX first.** It takes five minutes, needs no Microsoft account, and
covers Cursor, VSCodium, Windsurf, Gitpod and Theia. The marketplace is the
larger audience and by far the worse experience; it is worth doing second, with
a clear head, and not at all until you have decided which of the two routes
below you want.

## Open VSX (five minutes, no Azure)

1. Sign in at <https://open-vsx.org> with GitHub.
2. Profile → **Publisher Agreement** → sign it. Publishes fail until this is
   done, with a message that says so.
3. **Settings → Access Tokens** → create one.
4. Claim the namespace, once, from your own machine:

   ```
   npx ovsx create-namespace khora-lang -p <token>
   ```

5. Add the token as the repository secret **`OVSX_PAT`**.

Done. The next `vscode-v*` tag publishes there.

## The marketplace: two routes, both unpleasant

Azure DevOps is what the VS Code Marketplace runs on, and everything awkward
here follows from that.

**Global Personal Access Tokens are retired on 1 December 2026.** That matters
because the marketplace's own instructions tell you to create a PAT scoped to
*All accessible organizations* — which is the definition of a global PAT, and
therefore the exact thing being turned off. A PAT set up today works and stops
working on that date.

### Route A: trusted publishing (`--oidc`) — recommended

GitHub Actions proves the workflow's identity directly to the marketplace. No
Azure subscription, no managed identity, no federated credential, no stored
token anywhere.

1. On <https://marketplace.visualstudio.com/manage>, open the publisher and
   look for **Trusted Publishing** (some accounts do not have it yet; if it is
   absent, this route is not available to you today and route B is).
2. Add a policy for this repository:
   - Organization/user: `codyspate`
   - Repository: `khoralang`
   - Workflow filename: `extension.yml` — the file name only, no path
3. Add the repository **variable** (not secret) **`VSCE_TRUSTED_PUBLISHING`**
   set to `true`. That is what switches the workflow onto this path; no secret
   is involved, because that is the entire point.

The workflow already requests `id-token: write` and calls `vsce publish --oidc`.

**One caveat, stated plainly.** `--oidc` is implemented and working in `vsce`,
but it is marked `hideHelp` and ships only in the prerelease line
(`@vscode/vsce@next`, 3.9.3-12 at the time of writing) — it does not appear in
`vsce publish --help` on the current stable release. The workflow therefore
pins `@next` for this path only. Microsoft is clearly moving here: their own
README documents it while their CLI hides it. Revisit when it reaches stable
and drop the pin.

### Route B: a Personal Access Token — works now, dies 1 December 2026

Use this if trusted publishing is not on your publisher page yet.

1. Create an Azure DevOps organisation at <https://dev.azure.com> with any
   Microsoft account. The name is never shown to anybody; it exists only to own
   the token.
2. User settings → **Personal access tokens** → **New Token**:
   - **Organization: All accessible organizations.** The single most common
     mistake, and a token scoped to one organisation fails with an error that
     never mentions the reason.
   - **Scopes**: Custom defined → show all scopes → **Marketplace → Manage**.
   - Expiration: a year, or 1 December 2026, whichever comes first.
3. Create the publisher at <https://marketplace.visualstudio.com/manage> with
   the same account. The publisher ID must be exactly **`khora-lang`**, because
   that is what `package.json` says and the two have to agree.
4. Add the token as the repository secret **`VSCE_PAT`**.

### The route not taken

Microsoft's recommended replacement is Entra ID with workload identity
federation: an Azure subscription, a user-assigned managed identity, a service
connection, a federated credential, and an undocumented `az rest` call against
`app.vssps.visualstudio.com` to discover the identity's Azure DevOps profile ID,
because the marketplace keeps its own identity record separate from both the
ARM resource ID and the Entra object ID. It is designed for Azure Pipelines and
an organisation with an Azure footprint.

For a repository that already has the GitHub identity the marketplace is
willing to trust, it is the wrong shape. Route A is that same standard with the
parts you do not need removed.

## Then

```
git tag vscode-v0.3.1 && git push origin vscode-v0.3.1
```

The run packages the `.vsix`, verifies it holds the extension, the language
client and the icon, publishes the release asset, then publishes to whichever
registries are configured. Missing credentials skip their step and fail
nothing.

Listings appear within a few minutes and become searchable within about an hour.

## Checking it worked

```
code --install-extension khora-lang.khora
```

This installs *by name* rather than from a file, so it only succeeds once the
listing is live. Until then, installing means downloading a `.vsix`, running
`code --install-extension <file>`, and fully quitting VS Code — reloading the
window is not enough, because extensions are scanned at startup.
