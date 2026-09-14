// Hands browser downloads and right-clicked links to the running rdm app.
const ENDPOINT = "http://127.0.0.1:15080";

async function send(url, filename) {
  try {
    const res = await fetch(`${ENDPOINT}/add`, {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ url, filename }),
    });
    return res.ok;
  } catch (err) {
    return false;
  }
}

// Intercept normal downloads and move them into rdm.
chrome.downloads.onCreated.addListener(async (item) => {
  if (!item.url || item.url.startsWith("blob:") || item.url.startsWith("data:")) return;
  const ok = await send(item.url, item.filename ? item.filename.split(/[\\/]/).pop() : undefined);
  if (ok) {
    chrome.downloads.cancel(item.id);
    chrome.downloads.erase({ id: item.id });
  }
});

chrome.runtime.onInstalled.addListener(() => {
  chrome.contextMenus.create({
    id: "rdm-download-link",
    title: "Download with rdm",
    contexts: ["link", "image", "video", "audio"],
  });
});

chrome.contextMenus.onClicked.addListener(async (info) => {
  const url = info.linkUrl || info.srcUrl;
  if (!url) return;
  const ok = await send(url);
  chrome.notifications.create({
    type: "basic",
    iconUrl: "icon.png",
    title: "rdm",
    message: ok ? "Sent to rdm" : "rdm is not running",
  });
});
