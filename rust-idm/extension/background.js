// Hands browser downloads and right-clicked links to the running rdm app.
// Works in Chrome/Edge (MV3 service worker) and Firefox (MV2 background page).
const api = typeof browser !== "undefined" ? browser : chrome;
const ENDPOINT = "http://127.0.0.1:15080";

async function send(url, filename, referer) {
  try {
    const res = await fetch(`${ENDPOINT}/add`, {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ url, filename, referer }),
    });
    return res.ok;
  } catch (err) {
    return false;
  }
}

function toast(message) {
  try {
    api.notifications.create({
      type: "basic",
      iconUrl: "icon.png",
      title: "rdm",
      message,
    });
  } catch (err) {
    /* notifications are optional */
  }
}

// Intercept normal downloads and move them into rdm.
api.downloads.onCreated.addListener(async (item) => {
  if (!item.url || item.url.startsWith("blob:") || item.url.startsWith("data:")) return;
  const name = item.filename ? item.filename.split(/[\\/]/).pop() : undefined;
  const ok = await send(item.url, name, item.referrer);
  if (ok) {
    api.downloads.cancel(item.id);
    api.downloads.erase({ id: item.id });
  }
});

api.runtime.onInstalled.addListener(() => {
  api.contextMenus.create({
    id: "rdm-download-link",
    title: "Download with rdm",
    contexts: ["link", "image", "video", "audio"],
  });
  api.contextMenus.create({
    id: "rdm-download-page-links",
    title: "Download all links on this page with rdm",
    contexts: ["page"],
  });
});

api.contextMenus.onClicked.addListener(async (info, tab) => {
  if (info.menuItemId === "rdm-download-page-links") {
    try {
      const [{ result }] = await api.scripting.executeScript({
        target: { tabId: tab.id },
        func: () => Array.from(document.querySelectorAll("a[href]")).map((a) => a.href),
      });
      const res = await fetch(`${ENDPOINT}/add`, {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({ urls: result, referer: tab.url }),
      });
      toast(res.ok ? "Page links sent to rdm" : "rdm is not running");
    } catch (err) {
      toast("Could not read this page");
    }
    return;
  }

  const url = info.linkUrl || info.srcUrl;
  if (!url) return;
  const ok = await send(url, undefined, info.pageUrl);
  toast(ok ? "Sent to rdm" : "rdm is not running");
});
