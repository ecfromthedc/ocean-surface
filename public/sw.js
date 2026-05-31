// Ocean Surface service worker — minimal, installability + shell cache only.
//
// Strategy: network-first for everything, cache the static shell as a
// fallback. NEVER touch /v1/* or /api/* — those are live daemon/proxy calls
// (turns, SSE, STT, TTS, config) and must always go to the network.

// Bump on deploy to evict the prior shell. `activate` deletes every cache that
// isn't this one, so a version bump guarantees stale HTML/assets are dropped.
const CACHE = 'ocean-shell-v3';
// Precache immutable static assets. The start_url ('/') is fetched on install
// and stored under a STABLE key (OFFLINE_KEY) — separate from live navigation
// responses — so Chrome's installable-PWA check sees a working offline
// start_url, while navigations stay network-first and never serve a stale,
// old-bundle HTML.
const SHELL = ['/manifest.webmanifest', '/icon-192.png', '/icon-512.png'];
const OFFLINE_KEY = '/__offline_shell__';

self.addEventListener('install', (event) => {
  event.waitUntil(
    caches.open(CACHE).then(async (c) => {
      await c.addAll(SHELL).catch(() => {});
      // Seed the offline navigation fallback so the app is installable +
      // launchable with no network.
      try {
        const resp = await fetch('/', { cache: 'no-store' });
        if (resp.ok) await c.put(OFFLINE_KEY, resp.clone());
      } catch (_) {}
    })
  );
  self.skipWaiting();
});

self.addEventListener('activate', (event) => {
  event.waitUntil(
    caches.keys().then((keys) =>
      Promise.all(keys.filter((k) => k !== CACHE).map((k) => caches.delete(k)))
    )
  );
  self.clients.claim();
});

self.addEventListener('fetch', (event) => {
  const url = new URL(event.request.url);

  // Bypass the SW entirely for live endpoints — let them hit the network
  // directly so SSE streaming and POSTs are never intercepted or cached.
  if (url.pathname.startsWith('/v1/') || url.pathname.startsWith('/api/')) {
    return;
  }

  // Only handle same-origin GETs; everything else passes through.
  if (event.request.method !== 'GET' || url.origin !== self.location.origin) {
    return;
  }

  // Navigations (the HTML doc): network-first so the latest bundle is always
  // served; refresh the offline fallback on each success; fall back to the
  // stored shell only when offline. Never serves a stale doc while online.
  if (event.request.mode === 'navigate') {
    event.respondWith(
      fetch(event.request)
        .then((resp) => {
          const copy = resp.clone();
          caches.open(CACHE).then((c) => c.put(OFFLINE_KEY, copy)).catch(() => {});
          return resp;
        })
        .catch(() => caches.match(OFFLINE_KEY))
    );
    return;
  }

  // Static assets (hashed JS/WASM, icons, manifest): network-first, fall back
  // to cache so a flaky connection still launches.
  event.respondWith(
    fetch(event.request)
      .then((resp) => {
        const copy = resp.clone();
        caches.open(CACHE).then((c) => c.put(event.request, copy)).catch(() => {});
        return resp;
      })
      .catch(() => caches.match(event.request))
  );
});
