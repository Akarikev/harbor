# harbor v0.1.0 launch checklist

## Pre-launch

1. Ensure CI is green on `main`
2. Verify local build: `bun run build`
3. Deploy Vercel site (root directory: `website/`) — connect to `Akarikev/harbor`
4. Confirm screenshot and copy on landing page

## Release

```bash
# Tag and push — GitHub Actions builds and publishes release assets
git tag v0.1.0
git push origin v0.1.0
```

Wait for the [Release workflow](.github/workflows/release.yml) to finish.

## Go public

GitHub Release assets on a private repo are only visible to collaborators. Before sharing download links:

```bash
gh repo edit Akarikev/harbor --visibility public --accept-visibility-change-consequences
```

## Post-launch verification

1. Open the Vercel landing page — download buttons should resolve via GitHub API
2. Download AppImage from Releases, run it, confirm tray icon appears
3. Kill a test dev server (e.g. `python -m http.server`) and confirm it works
4. Confirm ulauncher / system ports show lock badge and cannot be killed

## Optional

- Add custom domain in Vercel project settings
- Update README website link once domain is live
