const REPO = "Akarikev/harbor";
const API = `https://api.github.com/repos/${REPO}/releases/latest`;

const selectors = {
  appimage: "#download-appimage",
  deb: "#download-deb",
  rpm: "#download-rpm",
  checksum: "#checksum-link",
  launchNote: "#launch-note",
  altFormats: "#alt-formats",
};

const linuxIcon = `<svg class="linux-icon" width="18" height="18" viewBox="0 0 24 24" fill="currentColor" aria-hidden="true"><path d="M12.504 0c-.155 0-.315.008-.48.021-4.226.333-3.105 4.807-3.17 6.298-.076 1.88-1.155 3.502-2.333 4.858-1.303 1.504-2.53 2.956-2.53 5.357 0 3.714 2.923 6.035 6.283 6.035 3.564 0 6.275-2.296 6.275-6.035 0-2.401-1.227-3.853-2.53-5.357-1.178-1.356-2.257-2.978-2.333-4.858-.065-1.491 1.056-5.965-3.17-6.298A8.4 8.4 0 0 0 12.504 0zm-.012 1.224c.155 0 .31.006.462.018 3.253.256 2.385 3.806 2.451 5.134.082 2.034 1.266 3.787 2.505 5.232 1.224 1.412 2.384 2.748 2.384 4.952 0 3.037-2.363 4.811-5.8 4.811-3.251 0-5.808-1.774-5.808-4.811 0-2.204 1.16-3.54 2.384-4.952 1.239-1.445 2.423-3.198 2.505-5.232.066-1.328-.802-4.878 2.451-5.134.152-.012.307-.018.462-.018z"/><circle cx="10.2" cy="8.8" r="1.1"/><circle cx="14.8" cy="8.8" r="1.1"/><path d="M10.8 12.2c.6.9 1.8.9 2.4 0" fill="none" stroke="currentColor" stroke-width=".8" stroke-linecap="round"/></svg>`;

function pickAsset(assets, pattern) {
  return assets.find((asset) => pattern.test(asset.name));
}

function setLink(selector, asset) {
  const el = document.querySelector(selector);
  if (!el) return false;

  if (asset) {
    el.href = asset.browser_download_url;
    el.classList.remove("is-unavailable");
    return true;
  }

  el.removeAttribute("href");
  return false;
}

function showPreLaunchState() {
  const appimageBtn = document.querySelector(selectors.appimage);
  const launchNote = document.querySelector(selectors.launchNote);
  const altFormats = document.querySelector(selectors.altFormats);
  const verifyLink = document.querySelector(selectors.checksum);

  if (appimageBtn) {
    appimageBtn.href = "#";
    appimageBtn.innerHTML = `${linuxIcon} Download for Linux`;
    appimageBtn.classList.add("is-disabled");
  }

  if (launchNote) launchNote.hidden = false;
  if (altFormats) altFormats.classList.add("is-unavailable");
  if (verifyLink) verifyLink.classList.add("is-unavailable");
}

async function loadReleaseLinks() {
  try {
    const response = await fetch(API, {
      headers: { Accept: "application/vnd.github+json" },
    });

    if (!response.ok) {
      throw new Error(`GitHub API ${response.status}`);
    }

    const release = await response.json();
    const assets = release.assets ?? [];

    const appimage = pickAsset(assets, /\.AppImage$/i);
    const deb = pickAsset(assets, /\.deb$/i);
    const rpm = pickAsset(assets, /\.rpm$/i);
    const checksums = pickAsset(assets, /^SHA256SUMS$/i);

    if (!appimage) {
      showPreLaunchState();
      return;
    }

    setLink(selectors.appimage, appimage);
    setLink(selectors.deb, deb);
    setLink(selectors.rpm, rpm);
    setLink(selectors.checksum, checksums);

    document.querySelector(selectors.appimage)?.classList.remove("is-disabled");
    document.querySelector(selectors.launchNote)?.hidden = true;
    document.querySelector(selectors.altFormats)?.classList.remove("is-unavailable");
    document.querySelector(selectors.checksum)?.classList.remove("is-unavailable");
  } catch {
    showPreLaunchState();
  }
}

loadReleaseLinks();
