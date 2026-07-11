const REPO = "Akarikev/harbor";
const API = `https://api.github.com/repos/${REPO}/releases/latest`;

const selectors = {
  appimage: "#download-appimage",
  deb: "#download-deb",
  rpm: "#download-rpm",
  checksum: "#checksum-link",
};

function pickAsset(assets, pattern) {
  return assets.find((asset) => pattern.test(asset.name));
}

function setLink(selector, asset, label) {
  const el = document.querySelector(selector);
  if (!el) return;

  if (asset) {
    el.href = asset.browser_download_url;
    if (label) el.textContent = label;
    el.classList.remove("is-disabled");
    return;
  }

  el.href = `https://github.com/${REPO}/releases/latest`;
  el.classList.add("is-disabled");
}

async function loadReleaseLinks() {
  const appimageBtn = document.querySelector(selectors.appimage);

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

    setLink(selectors.appimage, appimage);
    setLink(selectors.deb, deb, ".deb package");
    setLink(selectors.rpm, rpm, ".rpm package");
    setLink(selectors.checksum, checksums, "SHA256SUMS");

    if (appimageBtn && appimage) {
      appimageBtn.textContent = "Download for Linux";
    }
  } catch {
    if (appimageBtn) {
      appimageBtn.href = `https://github.com/${REPO}/releases/latest`;
      appimageBtn.textContent = "View releases";
    }

    for (const selector of [selectors.deb, selectors.rpm, selectors.checksum]) {
      const el = document.querySelector(selector);
      if (el) {
        el.href = `https://github.com/${REPO}/releases/latest`;
      }
    }
  }
}

loadReleaseLinks();
