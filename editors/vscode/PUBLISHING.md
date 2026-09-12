# Publishing the extension to the registries

The workflow already builds and checks the `.vsix`, and publishes it as a
release asset. Two further steps put it where people actually look — the
Visual Studio Marketplace and Open VSX — and each is skipped until its token
exists. Nothing here changes the artifact; the registries receive the same file
the release does.

Both need an account you create once, then a repository secret. Neither can be
done from CI.

## Visual Studio Marketplace

The one VS Code itself searches. It is the longer of the two because Microsoft
puts an Azure DevOps organisation in the middle.

1. **Create an Azure DevOps organisation** at <https://dev.azure.com>, signing
   in with any Microsoft account. The organisation's name does not matter and
   is never shown to anybody; it exists only to own the token.

2. **Create a Personal Access Token.** User settings (top right) → *Personal
   access tokens* → *New Token*.
   - **Organization**: `All accessible organizations`. This is the step people
     get wrong: a token scoped to one organisation cannot publish, and the
     error it produces does not say so.
   - **Scopes**: *Custom defined* → *Marketplace* → **Manage**.
   - **Expiration**: a year is the maximum. Note the date; a publish that
     starts failing a year from now will be this.

   Copy the token. It is shown once.

3. **Create the publisher** at
   <https://marketplace.visualstudio.com/manage>, using the same Microsoft
   account. The publisher ID must be exactly **`khora-lang`** — it is already
   in `package.json`, and the two have to agree or the publish is rejected.

4. **Add the token** to the repository: *Settings* → *Secrets and variables* →
   *Actions* → *New repository secret*, named **`VSCE_PAT`**.

## Open VSX

What VSCodium, Cursor, Gitpod and Eclipse Theia search. Shorter, and no
Microsoft account.

1. **Sign in** at <https://open-vsx.org> with GitHub.
2. **Sign the publisher agreement** — profile → *Publisher Agreement*. Publishes
   fail with a clear message until this is done.
3. **Create an access token** under *Settings* → *Access Tokens*.
4. **Create the `khora-lang` namespace**, once:

   ```
   npx ovsx create-namespace khora-lang -p <token>
   ```

5. **Add the token** as the repository secret **`OVSX_PAT`**.

## Then

Tag a release as usual:

```
git tag vscode-v0.3.1 && git push origin vscode-v0.3.1
```

The run packages the `.vsix`, verifies it contains the extension, the language
client and the icon, publishes the release asset, and then publishes to
whichever registries have tokens. A missing token skips its step and fails
nothing.

Listings take a few minutes to appear and are searchable within about an hour.

## What the reader gets

Today, installing means finding a release, downloading a file, running
`code --install-extension`, and fully quitting VS Code. Afterwards it means
typing "khora" into the extensions pane. That difference is most of why anybody
tries an editor plugin at all.

## Checking it worked

```
code --install-extension khora-lang.khora
```

installs from the marketplace by name rather than from a file — which only
succeeds if the listing is live.
